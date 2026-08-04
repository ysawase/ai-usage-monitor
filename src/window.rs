use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
use windows::Win32::UI::Accessibility::HWINEVENTHOOK;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::diagnose;
use crate::localization::{self, LanguageId, Strings};
#[cfg(test)]
use crate::models::UsageData;
use crate::models::{AppUsageData, UsageSection};
#[cfg(feature = "self-update")]
use crate::native_interop::TIMER_UPDATE_CHECK;
use crate::native_interop::{
    self, Color, TIMER_COUNTDOWN, TIMER_POLL, TIMER_RESET_POLL, WM_APP_TRAY, WM_APP_USAGE_UPDATED,
};
use crate::poller;
use crate::snapshot_schema;
use crate::snapshot_store;
use crate::theme;
use crate::tray_icon;
#[cfg(feature = "self-update")]
use crate::updater::UpdateCheckResult;
use crate::updater::{self, InstallChannel, ReleaseDescriptor};

/// Wrapper to make HWND sendable across threads (safe for PostMessage usage)
#[derive(Clone, Copy)]
struct SendHwnd(isize);

unsafe impl Send for SendHwnd {}

impl SendHwnd {
    fn from_hwnd(hwnd: HWND) -> Self {
        Self(hwnd.0 as isize)
    }
    fn to_hwnd(self) -> HWND {
        HWND(self.0 as *mut _)
    }
}

/// Shared application state
struct AppState {
    hwnd: SendHwnd,
    taskbar_hwnd: Option<HWND>,
    tray_notify_hwnd: Option<HWND>,
    win_event_hook: Option<HWINEVENTHOOK>,
    is_dark: bool,
    embedded: bool,
    language_override: Option<LanguageId>,
    language: LanguageId,
    install_channel: InstallChannel,

    display_basis: DisplayBasis,
    display_density: DisplayDensity,
    short_window_visibility: ShortWindowVisibility,
    short_window_alert_sensitivity: ShortWindowAlertSensitivity,

    session_state: CellState,
    session_percent: Option<f64>,
    session_text: String,
    weekly_state: CellState,
    weekly_percent: Option<f64>,
    weekly_text: String,
    codex_session_state: CellState,
    codex_session_percent: Option<f64>,
    codex_session_text: String,
    codex_weekly_state: CellState,
    codex_weekly_percent: Option<f64>,
    codex_weekly_text: String,
    antigravity_session_state: CellState,
    antigravity_session_percent: Option<f64>,
    antigravity_session_text: String,
    antigravity_weekly_state: CellState,
    antigravity_weekly_percent: Option<f64>,
    antigravity_weekly_text: String,
    show_claude_code: bool,
    show_codex: bool,
    show_antigravity: bool,

    data: Option<AppUsageData>,

    poll_interval_ms: u32,
    retry_count: u32,
    force_notify_auth_error: bool,
    auth_error_paused_polling: bool,
    auth_watch_mode: poller::CredentialWatchMode,
    auth_watch_snapshot: poller::CredentialWatchSnapshot,
    last_poll_ok: bool,
    update_status: UpdateStatus,
    last_update_check_unix: Option<u64>,

    taskbar_index: usize,
    tray_offset: i32,
    dragging: bool,
    drag_start_mouse_x: i32,
    drag_start_client_x: i32,
    drag_start_offset: i32,

    widget_visible: bool,
    always_on_top: bool,
}

#[derive(Clone, Debug)]
enum UpdateStatus {
    Idle,
    Checking,
    Applying,
    UpToDate,
    Available(ReleaseDescriptor),
}

/// User's chosen basis for the usage number and bar: how much of the quota
/// has been used, or how much is left. Only the AppState-level display copy
/// is affected — the internal `UsageSection::percentage` (always "used%")
/// from `poller`/`models`/`snapshot_schema` is never rewritten.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DisplayBasis {
    UsedPercentage,
    RemainingAllowance,
}

impl Default for DisplayBasis {
    fn default() -> Self {
        DisplayBasis::UsedPercentage
    }
}

/// How much pace/guidance detail the popup shows alongside each window's
/// percentage. Not yet connected to any drawing code — see
/// AUM-PACE-GUIDANCE-01's later units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DisplayDensity {
    Compact,
    Standard,
    Detailed,
}

impl Default for DisplayDensity {
    fn default() -> Self {
        Self::Standard
    }
}

/// Whether the 5h window is always shown, only shown while it's in an
/// overpacing warning state, or never shown. Not yet connected to any
/// drawing code — see AUM-PACE-GUIDANCE-01's later units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ShortWindowVisibility {
    Always,
    WarningOnly,
    Hidden,
}

impl Default for ShortWindowVisibility {
    fn default() -> Self {
        Self::WarningOnly
    }
}

/// Availability/status of a single (provider, window) usage cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CellState {
    Loading,
    Ok,
    FetchFailed,
    Retrying,
    NotConfigured,
    NotAvailable,
}

/// What a single usage cell should render. `bar_percent` is `Some` only when
/// there is a real current value to draw — a normal 0% is `Some(0.0)`, while
/// loading/error/unconfigured/not-available states are `None` so the bar can
/// never show a stale or invented "current" fill. `text` always carries the
/// user-facing string (either the basis-formatted number, or a localized
/// status word).
struct CellDisplay {
    bar_percent: Option<f64>,
    text: String,
}

/// Convert an internal used-percentage into the value to show, for the
/// chosen basis. The same result feeds both the number and the bar fill
/// (via `CellDisplay::bar_percent`), so they can never disagree.
fn display_value(basis: DisplayBasis, used_percent: f64) -> f64 {
    let used = used_percent.clamp(0.0, 100.0);
    match basis {
        DisplayBasis::UsedPercentage => used,
        DisplayBasis::RemainingAllowance => (100.0 - used).clamp(0.0, 100.0),
    }
}

/// Countdown text for a reset time, computed directly from `resets_at`
/// rather than by reparsing `poller::format_line`'s output. This mirrors
/// `poller::format_countdown`'s day/hour/minute/second floor rounding
/// exactly (verified line-by-line against the current `src/poller.rs`; see
/// the completion report). That function is a private detail of
/// `poller::format_line` and isn't reachable from here without a
/// `src/poller.rs` visibility change, which is out of scope for this
/// change. `None` in, `None` out (no countdown shown). A past reset time
/// reuses the existing `strings.now` word, same as `poller.rs`.
fn countdown_text(resets_at: Option<SystemTime>, strings: Strings) -> Option<String> {
    let reset = resets_at?;
    let remaining = match reset.duration_since(SystemTime::now()) {
        Ok(d) => d,
        Err(_) => return Some(strings.now.to_string()),
    };

    let total_secs = remaining.as_secs();
    let total_mins = total_secs / 60;
    let total_hours = total_secs / 3600;
    let total_days = total_secs / 86400;

    Some(if total_days >= 1 {
        format!("{total_days}{}", strings.day_suffix)
    } else if total_hours >= 1 {
        format!("{total_hours}{}", strings.hour_suffix)
    } else if total_mins >= 1 {
        format!("{total_mins}{}", strings.minute_suffix)
    } else {
        format!("{total_secs}{}", strings.second_suffix)
    })
}

/// The display-basis prefix ("Used"/"Remaining") is shown once in the
/// popup's header row rather than repeated on every cell — see
/// `draw_basis_label_row` — so this is just "<percent>%" optionally
/// followed by " · <reset-in word> <countdown>".
fn format_cell_text(basis: DisplayBasis, section: &UsageSection, strings: Strings) -> String {
    let pct = display_value(basis, section.percentage);
    let pct_text = format!("{pct:.0}%");
    match countdown_text(section.resets_at, strings) {
        Some(countdown) => format!("{pct_text} \u{00b7} {} {countdown}", strings.reset_in),
        None => pct_text,
    }
}

/// Localized status word for a non-`Ok` cell. `CellState::Ok` has no status
/// word of its own — a caller reaching this with `Ok` has already failed to
/// pair it with `Some(section)`, which is a caller bug, not a real "loading"
/// state; assert in debug builds and fail safe to "not available" text
/// rather than silently presenting it as ordinary loading.
fn status_text(state: CellState, strings: Strings) -> &'static str {
    match state {
        CellState::Loading => strings.loading,
        CellState::FetchFailed => strings.fetch_failed,
        CellState::Retrying => strings.retrying,
        CellState::NotConfigured => strings.not_configured,
        CellState::NotAvailable => strings.not_available,
        CellState::Ok => {
            debug_assert!(
                false,
                "status_text called with CellState::Ok (caller should pair Ok with Some(section))"
            );
            strings.not_available
        }
    }
}

fn render_cell(
    state: CellState,
    section: Option<&UsageSection>,
    basis: DisplayBasis,
    strings: Strings,
) -> CellDisplay {
    match (state, section) {
        (CellState::Ok, Some(section)) => CellDisplay {
            bar_percent: Some(display_value(basis, section.percentage)),
            text: format_cell_text(basis, section, strings),
        },
        _ => CellDisplay {
            bar_percent: None,
            text: status_text(state, strings).to_string(),
        },
    }
}

/// Classify a just-completed provider poll into session/weekly cell states.
/// `Disabled` (provider not requested this poll) is not a normal render
/// target — it maps to `NotAvailable` rather than `Loading`, since it does
/// not mean "waiting for first data"; the genuine "never polled yet" state
/// is `CellState::Loading` set once at `AppState` construction and left
/// alone here.
fn poll_cell_states(outcome: &poller::ProviderPollOutcome) -> (CellState, CellState) {
    match outcome {
        poller::ProviderPollOutcome::Success { usage, .. } => (
            if usage.session_available() {
                CellState::Ok
            } else {
                CellState::NotAvailable
            },
            if usage.weekly_available() {
                CellState::Ok
            } else {
                CellState::NotAvailable
            },
        ),
        poller::ProviderPollOutcome::Error { error, .. } => {
            let state = match error {
                poller::PollError::AuthRequired | poller::PollError::TokenExpired => {
                    CellState::FetchFailed
                }
                poller::PollError::NoCredentials => CellState::NotConfigured,
                poller::PollError::RequestFailed => CellState::Retrying,
            };
            (state, state)
        }
        poller::ProviderPollOutcome::Disabled => (CellState::NotAvailable, CellState::NotAvailable),
    }
}

/// Overwrite each provider's cached `UsageData` with this poll's result only
/// when that provider actually succeeded this round; a provider that didn't
/// succeed (error or disabled) keeps whatever was cached before. That old
/// value is never read once the corresponding `CellState` (set from the same
/// `report`, right alongside this call) is anything but `Ok` — see
/// `render_cell`. Called the same way from both the success and failure
/// branches of `do_poll` so "this provider is Ok" and "this provider's
/// displayed value is from an old poll" can never occur together, regardless
/// of ordering.
fn merge_successful_providers(data: &mut Option<AppUsageData>, report: &poller::PollReport) {
    let data = data.get_or_insert_with(AppUsageData::default);
    if let poller::ProviderPollOutcome::Success { usage, .. } = &report.claude_code {
        data.claude_code = Some(usage.clone());
    }
    if let poller::ProviderPollOutcome::Success { usage, .. } = &report.codex {
        data.codex = Some(usage.clone());
    }
    if let poller::ProviderPollOutcome::Success { usage, .. } = &report.antigravity {
        data.antigravity = Some(usage.clone());
    }
}

// ── Weekly pace guidance (AUM-PACE-GUIDANCE-01) ───────────────────────────
//
// This section is pure logic only: no popup drawing, settings, or
// localization are wired to it yet.

/// Fixed window durations. `UsageSection` only carries `percentage` and an
/// absolute `resets_at`, never a window start time, so pace/guidance math
/// treats these as constants — the same assumption the existing "5h"/"7d"
/// row labels already make for every provider.
const SESSION_WINDOW_SECS: u64 = 5 * 3600;
const WEEKLY_WINDOW_SECS: u64 = 7 * 24 * 3600;

/// How far past a window's nominal length `resets_at` is still trusted as
/// "the window just started" (elapsed = 0) rather than rejected outright.
/// Covers small clock/poll skew between client and server without
/// pretending to know a real elapsed time beyond the window boundary.
const WINDOW_TIME_TOLERANCE_SECS: u64 = 5 * 60;

/// Below this much elapsed time into the window, pace is considered too
/// noisy to judge.
const PACE_JUDGING_MIN_ELAPSED_SECS: u64 = 6 * 3600;
/// pace_diff (used% − elapsed%) boundaries, in percentage points.
const PACE_UNDER_PACE_MAX_PT: f64 = -10.0;
const PACE_ON_TRACK_MAX_PT: f64 = 10.0;
const PACE_SLIGHTLY_OVER_MAX_PT: f64 = 25.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WeeklyPaceStatus {
    Judging,
    UnderPace,
    OnTrack,
    SlightlyOverpacing,
    Overpacing,
}

/// Seconds remaining until `resets_at`, or `None` if it's missing or already
/// past (can't safely derive elapsed/remaining from a stale or absent reset).
fn remaining_secs_at(resets_at: Option<SystemTime>, now: SystemTime) -> Option<u64> {
    let reset = resets_at?;
    reset.duration_since(now).ok().map(|d| d.as_secs())
}

/// Elapsed time into a fixed-length window, derived from time remaining
/// until reset:
/// - `remaining_secs <= window_secs`: the ordinary case.
/// - up to `WINDOW_TIME_TOLERANCE_SECS` past `window_secs`: treated as
///   elapsed = 0 (the window just started; small clock/poll skew).
/// - beyond that tolerance: `None` — not a value a real window can produce,
///   so no elapsed time is guessed.
fn elapsed_secs_in_window(remaining_secs: u64, window_secs: u64) -> Option<u64> {
    if remaining_secs <= window_secs {
        return Some(window_secs - remaining_secs);
    }
    let tolerated_max = window_secs.saturating_add(WINDOW_TIME_TOLERANCE_SECS);
    if remaining_secs <= tolerated_max {
        return Some(0);
    }
    None
}

/// `pace_diff = used% − elapsed%`, judged against fixed point-boundaries.
/// Always resolves to a concrete status (`Judging` included) given valid
/// elapsed/window/used inputs; callers gate on missing/unsafe reset data
/// themselves (via `remaining_secs_at`/`elapsed_secs_in_window`) before ever
/// calling this.
fn weekly_pace_status(elapsed_secs: u64, window_secs: u64, used_percent: f64) -> WeeklyPaceStatus {
    if window_secs == 0 || elapsed_secs < PACE_JUDGING_MIN_ELAPSED_SECS {
        return WeeklyPaceStatus::Judging;
    }
    let elapsed_fraction = (elapsed_secs as f64 / window_secs as f64).clamp(0.0, 1.0);
    let pace_diff = used_percent.clamp(0.0, 100.0) - elapsed_fraction * 100.0;

    if pace_diff <= PACE_UNDER_PACE_MAX_PT {
        WeeklyPaceStatus::UnderPace
    } else if pace_diff <= PACE_ON_TRACK_MAX_PT {
        WeeklyPaceStatus::OnTrack
    } else if pace_diff <= PACE_SLIGHTLY_OVER_MAX_PT {
        WeeklyPaceStatus::SlightlyOverpacing
    } else {
        WeeklyPaceStatus::Overpacing
    }
}

/// Below this much time left, guidance switches from "%/day" to "%/hour".
/// Remaining time only ever counts down within a window (it jumps back up
/// once, at reset, to a fresh window) so this threshold is crossed at most
/// once per window — not something that flaps back and forth on its own.
const FUTURE_PACE_HOURLY_THRESHOLD_SECS: u64 = 24 * 3600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FuturePaceUnit {
    PerDay,
    PerHour,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FuturePaceGuidance {
    value: f64,
    unit: FuturePaceUnit,
}

/// "Remaining allowance ÷ time left", regardless of the user's chosen
/// display basis — this is a budget/burn-rate figure ("how much room is
/// left, spread over how much time is left"), not a restatement of the
/// current value, so it does not flip with `DisplayBasis`.
fn future_pace_guidance(used_percent: f64, remaining_secs: u64) -> Option<FuturePaceGuidance> {
    if remaining_secs == 0 {
        return None;
    }
    let remaining_percent = (100.0 - used_percent.clamp(0.0, 100.0)).max(0.0);
    if remaining_secs < FUTURE_PACE_HOURLY_THRESHOLD_SECS {
        let remaining_hours = remaining_secs as f64 / 3600.0;
        Some(FuturePaceGuidance {
            value: remaining_percent / remaining_hours,
            unit: FuturePaceUnit::PerHour,
        })
    } else {
        let remaining_days = remaining_secs as f64 / 86400.0;
        Some(FuturePaceGuidance {
            value: remaining_percent / remaining_days,
            unit: FuturePaceUnit::PerDay,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ShortWindowAlertSensitivity {
    Sensitive,
    Standard,
    Relaxed,
}

impl Default for ShortWindowAlertSensitivity {
    fn default() -> Self {
        ShortWindowAlertSensitivity::Standard
    }
}

struct ShortWindowAlertThresholds {
    grace_secs: u64,
    min_used_percent: f64,
    exhaustion_lead_secs: u64,
}

impl ShortWindowAlertSensitivity {
    fn thresholds(self) -> ShortWindowAlertThresholds {
        match self {
            ShortWindowAlertSensitivity::Sensitive => ShortWindowAlertThresholds {
                grace_secs: 20 * 60,
                min_used_percent: 30.0,
                exhaustion_lead_secs: 30 * 60,
            },
            ShortWindowAlertSensitivity::Standard => ShortWindowAlertThresholds {
                grace_secs: 30 * 60,
                min_used_percent: 40.0,
                exhaustion_lead_secs: 45 * 60,
            },
            ShortWindowAlertSensitivity::Relaxed => ShortWindowAlertThresholds {
                grace_secs: 45 * 60,
                min_used_percent: 50.0,
                exhaustion_lead_secs: 75 * 60,
            },
        }
    }
}

/// True only when all three conditions hold: past the grace period, past
/// the minimum used%, and — projecting the current average pace
/// (`used% / elapsed`) linearly forward — 100% is reached with at least
/// `exhaustion_lead_secs` to spare before the actual reset (boundary
/// inclusive throughout: `>=`, not `>`). Division is only reached once
/// `used > 0.0` is established; `used >= 100.0` is handled directly
/// (already exhausted, so overpacing iff at least `exhaustion_lead_secs`
/// remain in the window) without going through the projection at all.
fn short_window_is_overpacing(
    elapsed_secs: u64,
    remaining_secs: u64,
    used_percent: f64,
    sensitivity: ShortWindowAlertSensitivity,
) -> bool {
    let t = sensitivity.thresholds();

    if elapsed_secs < t.grace_secs {
        return false;
    }
    let used = used_percent.clamp(0.0, 100.0);
    if used < t.min_used_percent {
        return false;
    }
    if used >= 100.0 {
        return remaining_secs >= t.exhaustion_lead_secs;
    }

    let secs_to_exhaustion = (100.0 - used) * elapsed_secs as f64 / used;
    if !secs_to_exhaustion.is_finite() || secs_to_exhaustion < 0.0 {
        return false;
    }

    (remaining_secs as f64 - secs_to_exhaustion) >= t.exhaustion_lead_secs as f64
}

const RETRY_BASE_MS: u32 = 30_000; // 30 seconds

const POLL_1_MIN: u32 = 60_000;
const POLL_5_MIN: u32 = 300_000;
const POLL_15_MIN: u32 = 900_000;
const POLL_1_HOUR: u32 = 3_600_000;

// Menu item IDs for update frequency
const IDM_FREQ_1MIN: u16 = 10;
const IDM_FREQ_5MIN: u16 = 11;
const IDM_FREQ_15MIN: u16 = 12;
const IDM_FREQ_1HOUR: u16 = 13;
const IDM_START_WITH_WINDOWS: u16 = 20;
const IDM_ALWAYS_ON_TOP: u16 = 32;
const IDM_RESET_POSITION: u16 = 30;
#[cfg(feature = "self-update")]
const IDM_VERSION_ACTION: u16 = 31;
const IDM_LANG_SYSTEM: u16 = 40;
const IDM_LANG_ENGLISH: u16 = 41;
const IDM_LANG_DUTCH: u16 = 42;
const IDM_LANG_SPANISH: u16 = 43;
const IDM_LANG_FRENCH: u16 = 44;
const IDM_LANG_GERMAN: u16 = 45;
const IDM_LANG_JAPANESE: u16 = 46;
const IDM_LANG_KOREAN: u16 = 47;
const IDM_LANG_TRADITIONAL_CHINESE: u16 = 48;
const IDM_LANG_RUSSIAN: u16 = 49;
const IDM_LANG_PORTUGUESE_BRAZIL: u16 = 50;
const IDM_LANG_SIMPLIFIED_CHINESE: u16 = 51;
const IDM_MODEL_CLAUDE_CODE: u16 = 60;
const IDM_MODEL_CODEX: u16 = 61;
#[cfg(feature = "antigravity")]
const IDM_MODEL_ANTIGRAVITY: u16 = 62;
// 70 is `tray_icon::IDM_TOGGLE_WIDGET`, not redefined here — it shares the
// same `WM_COMMAND` id space as every constant in this block, so it must be
// treated as already taken (71 is skipped too, to leave no ambiguity next
// to it). See `wm_command_menu_ids_are_globally_unique` for the test that
// checks this across both files.
const IDM_DISPLAY_BASIS_USED: u16 = 72;
const IDM_DISPLAY_BASIS_REMAINING: u16 = 73;
// 74-79 intentionally left free.
const IDM_DISPLAY_DENSITY_COMPACT: u16 = 80;
const IDM_DISPLAY_DENSITY_STANDARD: u16 = 81;
const IDM_DISPLAY_DENSITY_DETAILED: u16 = 82;
const IDM_SHORT_WINDOW_VISIBILITY_ALWAYS: u16 = 83;
const IDM_SHORT_WINDOW_VISIBILITY_WARNING_ONLY: u16 = 84;
const IDM_SHORT_WINDOW_VISIBILITY_HIDDEN: u16 = 85;
const IDM_SHORT_WINDOW_ALERT_SENSITIVITY_SENSITIVE: u16 = 86;
const IDM_SHORT_WINDOW_ALERT_SENSITIVITY_STANDARD: u16 = 87;
const IDM_SHORT_WINDOW_ALERT_SENSITIVITY_RELAXED: u16 = 88;

/// Pure `menu ID -> enum value` lookups, shared by `show_context_menu`
/// (which sets which item starts checked) and the `WM_COMMAND` handler
/// (which applies the selection). Kept separate from any Win32 call so both
/// directions of the ID/value mapping can be unit tested without a window.
fn display_density_for_menu_id(id: u16) -> Option<DisplayDensity> {
    match id {
        IDM_DISPLAY_DENSITY_COMPACT => Some(DisplayDensity::Compact),
        IDM_DISPLAY_DENSITY_STANDARD => Some(DisplayDensity::Standard),
        IDM_DISPLAY_DENSITY_DETAILED => Some(DisplayDensity::Detailed),
        _ => None,
    }
}

fn short_window_visibility_for_menu_id(id: u16) -> Option<ShortWindowVisibility> {
    match id {
        IDM_SHORT_WINDOW_VISIBILITY_ALWAYS => Some(ShortWindowVisibility::Always),
        IDM_SHORT_WINDOW_VISIBILITY_WARNING_ONLY => Some(ShortWindowVisibility::WarningOnly),
        IDM_SHORT_WINDOW_VISIBILITY_HIDDEN => Some(ShortWindowVisibility::Hidden),
        _ => None,
    }
}

fn short_window_alert_sensitivity_for_menu_id(id: u16) -> Option<ShortWindowAlertSensitivity> {
    match id {
        IDM_SHORT_WINDOW_ALERT_SENSITIVITY_SENSITIVE => {
            Some(ShortWindowAlertSensitivity::Sensitive)
        }
        IDM_SHORT_WINDOW_ALERT_SENSITIVITY_STANDARD => Some(ShortWindowAlertSensitivity::Standard),
        IDM_SHORT_WINDOW_ALERT_SENSITIVITY_RELAXED => Some(ShortWindowAlertSensitivity::Relaxed),
        _ => None,
    }
}

const WM_DPICHANGED_MSG: u32 = 0x02E0;
#[cfg(feature = "self-update")]
const WM_APP_UPDATE_CHECK_COMPLETE: u32 = WM_APP + 2;
const TRAY_ICON_UPDATE_REPOSITION_SUPPRESS_MS: u64 = 750;

/// How often the watchdog thread polls for an explorer.exe restart (which
/// recreates the taskbar and wipes our tray-icon registration).
const TASKBAR_WATCH_INTERVAL_SECS: u64 = 2;

static SUPPRESS_TRAY_REPOSITION_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

/// Current system DPI (96 = 100% scaling, 144 = 150%, 192 = 200%, etc.)
static CURRENT_DPI: AtomicU32 = AtomicU32::new(96);

/// Scale a base pixel value (designed at 96 DPI) to the current DPI.
fn sc(px: i32) -> i32 {
    let dpi = CURRENT_DPI.load(Ordering::Relaxed);
    (px as f64 * dpi as f64 / 96.0).round() as i32
}

/// Re-query the monitor DPI for our window and update the cached value.
/// Uses GetDpiForWindow which returns the live DPI (unlike GetDpiForSystem
/// which is cached at process startup and never changes).
fn refresh_dpi() {
    let hwnd = {
        let state = lock_state();
        state.as_ref().map(|s| s.hwnd.to_hwnd())
    };
    if let Some(hwnd) = hwnd {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        if dpi > 0 {
            CURRENT_DPI.store(dpi, Ordering::Relaxed);
        }
    }
}

/// Spacing below which two relaunches are treated as a storm (e.g. explorer.exe
/// crash-looping); when detected we back off instead of spawning in a tight loop.
const RELAUNCH_THROTTLE_SECS: u64 = 10;
const RELAUNCH_BACKOFF_SECS: u64 = 30;
/// Environment flag set on a relaunched child so it waits for the previous
/// instance's single-instance mutex instead of exiting immediately.
const ENV_RELAUNCH: &str = "CCUM_RELAUNCH";
/// Unix timestamp (seconds) of the relaunch that spawned this process, passed to
/// the child so it can detect a relaunch storm.
const ENV_LAST_RELAUNCH_UNIX: &str = "CCUM_LAST_RELAUNCH_UNIX";

/// Relaunch the widget as a fresh process after explorer.exe has restarted.
///
/// The popup window itself is top-level and survives an explorer.exe
/// restart, but the taskbar/tray notification area it was tracking does
/// not: the system tray icon needs to be re-added, and the taskbar anchor
/// (used for tray-relative X positioning) needs to be re-resolved against
/// the freshly created taskbar. Spawning a clean new process - which
/// re-registers the tray icon and re-selects the taskbar anchor on startup
/// - is the simplest robust recovery. The child is flagged via
/// `ENV_RELAUNCH` so it waits for this instance's single-instance mutex to
/// be released before taking over (see the guard in `run`).
fn relaunch_self() {
    // Back off if we are relaunching very soon after the relaunch that spawned
    // us: that signals the shell is crash-looping, not a one-off restart.
    let now = now_unix_secs();
    let last = std::env::var(ENV_LAST_RELAUNCH_UNIX)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if last != 0 && now.saturating_sub(last) < RELAUNCH_THROTTLE_SECS {
        diagnose::log("relaunch storm detected; backing off before relaunching");
        std::thread::sleep(Duration::from_secs(RELAUNCH_BACKOFF_SECS));
    }

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            diagnose::log_error("watchdog: unable to resolve current executable", error);
            return;
        }
    };

    let args: Vec<String> = std::env::args().skip(1).collect();
    match std::process::Command::new(exe)
        .args(&args)
        .env(ENV_RELAUNCH, "1")
        .env(ENV_LAST_RELAUNCH_UNIX, now.to_string())
        .spawn()
    {
        Ok(_) => {
            diagnose::log("watchdog: relaunched fresh instance, exiting old one");
            std::process::exit(0);
        }
        Err(error) => {
            diagnose::log_error("watchdog: unable to spawn relaunched instance", error);
        }
    }
}

/// Detect explorer.exe restarts and recover from them.
///
/// When explorer.exe restarts, the old taskbar HWND becomes invalid and a
/// new one is created; our tray icon registration and taskbar anchor go
/// stale along with it. This dedicated thread polls the taskbar handle and,
/// when it changes, relaunches the widget as a fresh process to re-register
/// the tray icon and re-select the taskbar anchor.
fn spawn_taskbar_watchdog() {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(TASKBAR_WATCH_INTERVAL_SECS));
        let stored = {
            let state = lock_state();
            state.as_ref().and_then(|s| s.taskbar_hwnd)
        };
        // Only relevant once we have selected a taskbar anchor at least once.
        let Some(old) = stored else {
            continue;
        };
        let taskbars = native_interop::find_taskbars();
        if !taskbars.is_empty() && !taskbars.iter().any(|taskbar| taskbar.hwnd == old) {
            let new = taskbars[0].hwnd;
            diagnose::log(format!(
                "watchdog: taskbar changed old={:?} new={:?} -> relaunching",
                old.0, new.0
            ));
            relaunch_self();
        }
    });
}

fn load_embedded_app_icons() -> (HICON, HICON) {
    unsafe {
        let mut exe_buf = [0u16; 260];
        let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
        if len == 0 {
            return (HICON::default(), HICON::default());
        }

        let mut large_icon = HICON::default();
        let mut small_icon = HICON::default();
        let extracted = ExtractIconExW(
            PCWSTR::from_raw(exe_buf.as_ptr()),
            0,
            Some(&mut large_icon),
            Some(&mut small_icon),
            1,
        );

        if extracted == 0 {
            (HICON::default(), HICON::default())
        } else {
            (large_icon, small_icon)
        }
    }
}

unsafe impl Send for AppState {}

static STATE: Mutex<Option<AppState>> = Mutex::new(None);

/// Lock STATE safely, recovering from poisoned mutex
fn lock_state() -> MutexGuard<'static, Option<AppState>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn settings_path() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(appdata)
        .join("ClaudeCodeUsageMonitor")
        .join("settings.json")
}

#[derive(Debug, Serialize, Deserialize)]
struct SettingsFile {
    #[serde(default)]
    tray_offset: i32,
    #[serde(default)]
    taskbar_index: usize,
    #[serde(default = "default_poll_interval")]
    poll_interval_ms: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_update_check_unix: Option<u64>,
    #[serde(default = "default_widget_visible")]
    widget_visible: bool,
    #[serde(default)]
    always_on_top: bool,
    #[serde(default = "default_show_claude_code")]
    show_claude_code: bool,
    #[serde(default = "default_show_codex")]
    show_codex: bool,
    #[serde(default = "default_show_antigravity")]
    show_antigravity: bool,
    #[serde(default, deserialize_with = "deserialize_display_basis")]
    display_basis: DisplayBasis,
    #[serde(default, deserialize_with = "deserialize_display_density")]
    display_density: DisplayDensity,
    #[serde(default, deserialize_with = "deserialize_short_window_visibility")]
    short_window_visibility: ShortWindowVisibility,
    #[serde(
        default,
        deserialize_with = "deserialize_short_window_alert_sensitivity"
    )]
    short_window_alert_sensitivity: ShortWindowAlertSensitivity,
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            tray_offset: 0,
            taskbar_index: 0,
            poll_interval_ms: default_poll_interval(),
            language: None,
            last_update_check_unix: None,
            widget_visible: true,
            always_on_top: false,
            show_claude_code: true,
            show_codex: false,
            show_antigravity: false,
            display_basis: DisplayBasis::default(),
            display_density: DisplayDensity::default(),
            short_window_visibility: ShortWindowVisibility::default(),
            short_window_alert_sensitivity: ShortWindowAlertSensitivity::default(),
        }
    }
}

/// Falls back to the default basis for any value this build doesn't
/// recognize (e.g. a newer settings.json written by a future version),
/// rather than letting one unrecognized field fail the whole `SettingsFile`
/// parse and reset every other saved preference back to default.
fn deserialize_display_basis<'de, D>(deserializer: D) -> Result<DisplayBasis, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)
        .ok()
        .and_then(|value| serde_json::from_value::<DisplayBasis>(value).ok())
        .unwrap_or_default())
}

/// Same lenient fallback as `deserialize_display_basis`, for `DisplayDensity`.
fn deserialize_display_density<'de, D>(deserializer: D) -> Result<DisplayDensity, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)
        .ok()
        .and_then(|value| serde_json::from_value::<DisplayDensity>(value).ok())
        .unwrap_or_default())
}

/// Same lenient fallback as `deserialize_display_basis`, for
/// `ShortWindowVisibility`.
fn deserialize_short_window_visibility<'de, D>(
    deserializer: D,
) -> Result<ShortWindowVisibility, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)
        .ok()
        .and_then(|value| serde_json::from_value::<ShortWindowVisibility>(value).ok())
        .unwrap_or_default())
}

/// Same lenient fallback as `deserialize_display_basis`, for
/// `ShortWindowAlertSensitivity`.
fn deserialize_short_window_alert_sensitivity<'de, D>(
    deserializer: D,
) -> Result<ShortWindowAlertSensitivity, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(serde_json::Value::deserialize(deserializer)
        .ok()
        .and_then(|value| serde_json::from_value::<ShortWindowAlertSensitivity>(value).ok())
        .unwrap_or_default())
}

fn default_poll_interval() -> u32 {
    POLL_15_MIN
}

fn default_widget_visible() -> bool {
    true
}

fn default_show_claude_code() -> bool {
    true
}

fn default_show_codex() -> bool {
    false
}

fn default_show_antigravity() -> bool {
    false
}

fn load_settings() -> SettingsFile {
    let content = match std::fs::read_to_string(settings_path()) {
        Ok(c) => c,
        Err(_) => return SettingsFile::default(),
    };
    let mut settings: SettingsFile = serde_json::from_str(&content).unwrap_or_default();
    #[cfg(not(feature = "antigravity"))]
    {
        settings.show_antigravity = false;
    }
    if !settings.show_claude_code && !settings.show_codex && !settings.show_antigravity {
        settings.show_claude_code = true;
    }
    settings
}

fn save_settings(settings: &SettingsFile) {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(path, json);
    }
}

fn save_state_settings() {
    let state = lock_state();
    if let Some(s) = state.as_ref() {
        save_settings(&SettingsFile {
            tray_offset: s.tray_offset,
            taskbar_index: s.taskbar_index,
            poll_interval_ms: s.poll_interval_ms,
            language: s
                .language_override
                .map(|language| language.code().to_string()),
            last_update_check_unix: s.last_update_check_unix,
            widget_visible: s.widget_visible,
            always_on_top: s.always_on_top,
            show_claude_code: s.show_claude_code,
            show_codex: s.show_codex,
            show_antigravity: s.show_antigravity,
            display_basis: s.display_basis,
            display_density: s.display_density,
            short_window_visibility: s.short_window_visibility,
            short_window_alert_sensitivity: s.short_window_alert_sensitivity,
        });
    }
}

fn tray_icon_data_from_state() -> Vec<tray_icon::TrayIconData> {
    let state = lock_state();
    match state.as_ref() {
        Some(s) if s.last_poll_ok => {
            // The tray icon's fill color/text-color thresholds in
            // `tray_icon::create_icon` assume `percent` is used-percentage
            // (they redden/invert as it rises toward 100 — see the
            // completion report). `s.*_percent`/`s.*_text` are basis-
            // converted for the popup and would both invert that meaning
            // and disagree with the icon's own color under "remaining"
            // (e.g. icon shows a low, safe-looking number while the
            // tooltip says "30% remaining" — actually 70% used). So every
            // tray field (icon percent AND tooltip text) is recomputed here
            // independently, always as used-percentage, regardless of
            // `s.display_basis`; the popup keeps the user's chosen basis.
            let strings = s.language.strings();

            let claude_session = s
                .data
                .as_ref()
                .and_then(|d| d.claude_code.as_ref())
                .map(|u| &u.session);
            let claude_weekly = s
                .data
                .as_ref()
                .and_then(|d| d.claude_code.as_ref())
                .map(|u| &u.weekly);
            let claude_session_used = render_cell(
                s.session_state,
                claude_session,
                DisplayBasis::UsedPercentage,
                strings,
            );
            let claude_weekly_used = render_cell(
                s.weekly_state,
                claude_weekly,
                DisplayBasis::UsedPercentage,
                strings,
            );

            let codex_session = s
                .data
                .as_ref()
                .and_then(|d| d.codex.as_ref())
                .map(|u| &u.session);
            let codex_weekly = s
                .data
                .as_ref()
                .and_then(|d| d.codex.as_ref())
                .map(|u| &u.weekly);
            let codex_session_used = render_cell(
                s.codex_session_state,
                codex_session,
                DisplayBasis::UsedPercentage,
                strings,
            );
            let codex_weekly_used = render_cell(
                s.codex_weekly_state,
                codex_weekly,
                DisplayBasis::UsedPercentage,
                strings,
            );

            let antigravity_session = s
                .data
                .as_ref()
                .and_then(|d| d.antigravity.as_ref())
                .map(|u| &u.session);
            let antigravity_weekly = s
                .data
                .as_ref()
                .and_then(|d| d.antigravity.as_ref())
                .map(|u| &u.weekly);
            let antigravity_session_used = render_cell(
                s.antigravity_session_state,
                antigravity_session,
                DisplayBasis::UsedPercentage,
                strings,
            );
            let antigravity_weekly_used = render_cell(
                s.antigravity_weekly_state,
                antigravity_weekly,
                DisplayBasis::UsedPercentage,
                strings,
            );

            let mut icons = Vec::new();
            if s.show_claude_code {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Claude,
                    percent: claude_session_used.bar_percent,
                    tooltip: format!(
                        "{} | {} | 5h: {} | 7d: {}",
                        strings.claude_code_model,
                        strings.used_percentage,
                        claude_session_used.text,
                        claude_weekly_used.text,
                    ),
                });
            }
            if s.show_codex {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Codex,
                    percent: codex_session_used.bar_percent,
                    tooltip: format!(
                        "{} | {} | 5h: {} | 7d: {}",
                        strings.codex_model,
                        strings.used_percentage,
                        codex_session_used.text,
                        codex_weekly_used.text,
                    ),
                });
            }
            if s.show_antigravity {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Antigravity,
                    percent: antigravity_session_used.bar_percent,
                    tooltip: format!(
                        "{} | {} | 5h: {} | 7d: {}",
                        strings.antigravity_model,
                        strings.used_percentage,
                        antigravity_session_used.text,
                        antigravity_weekly_used.text,
                    ),
                });
            }
            icons
        }
        Some(s) => {
            let mut icons = Vec::new();
            if s.show_claude_code {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Claude,
                    percent: None,
                    tooltip: s.language.strings().window_title.to_string(),
                });
            }
            if s.show_codex {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Codex,
                    percent: None,
                    tooltip: s.language.strings().codex_window_title.to_string(),
                });
            }
            if s.show_antigravity {
                icons.push(tray_icon::TrayIconData {
                    kind: tray_icon::TrayIconKind::Antigravity,
                    percent: None,
                    tooltip: s.language.strings().antigravity_window_title.to_string(),
                });
            }
            icons
        }
        None => Vec::new(),
    }
}

fn sync_tray_icons(hwnd: HWND) {
    let icons = tray_icon_data_from_state();
    tray_icon::sync(hwnd, &icons);
}

/// Apply (or remove) the topmost z-order per the "always on top" preference.
/// Always explicit about both directions (TOPMOST/NOTOPMOST) so an OFF
/// preference can't leave a stale topmost z-order in place.
fn apply_always_on_top(hwnd: HWND, always_on_top: bool) {
    let insert_after = if always_on_top {
        HWND_TOPMOST
    } else {
        HWND_NOTOPMOST
    };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            insert_after,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

fn toggle_widget_visibility(hwnd: HWND) {
    let (new_visible, always_on_top) = {
        let mut state = lock_state();
        if let Some(s) = state.as_mut() {
            s.widget_visible = !s.widget_visible;
            (s.widget_visible, s.always_on_top)
        } else {
            return;
        }
    };
    save_state_settings();
    unsafe {
        if new_visible {
            position_at_taskbar();
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            apply_always_on_top(hwnd, always_on_top);
            render_layered();
        } else {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// Locate the taskbar to anchor the top-level popup against (tray-relative
/// X position, work-area-relative Y position). This does not reparent the
/// window or change its style - the popup always remains a top-level,
/// topmost window positioned above the taskbar.
fn select_taskbar_anchor(requested_index: usize) -> bool {
    let taskbars = native_interop::find_taskbars();
    if taskbars.is_empty() {
        diagnose::log("no taskbar found; popup will use its default position");
        return false;
    }

    let index = requested_index.min(taskbars.len().saturating_sub(1));
    let taskbar = taskbars[index];
    diagnose::log(format!(
        "taskbar selected index={index} count={} hwnd={:?} rect=({}, {}, {}, {})",
        taskbars.len(),
        taskbar.hwnd,
        taskbar.rect.left,
        taskbar.rect.top,
        taskbar.rect.right,
        taskbar.rect.bottom
    ));

    let old_hook = {
        let mut state = lock_state();
        state.as_mut().and_then(|s| s.win_event_hook.take())
    };
    if let Some(hook) = old_hook {
        native_interop::unhook_win_event(hook);
    }

    let tray_notify = native_interop::find_child_window(taskbar.hwnd, "TrayNotifyWnd");
    if tray_notify.is_some() {
        diagnose::log("TrayNotifyWnd found");
    } else {
        diagnose::log("TrayNotifyWnd not found");
    }

    let hook = tray_notify.and_then(|tray_hwnd| {
        let thread_id = native_interop::get_window_thread_id(tray_hwnd);
        native_interop::set_tray_event_hook(thread_id, on_tray_location_changed)
    });
    if hook.is_some() {
        diagnose::log("tray event hook installed");
    } else {
        diagnose::log("tray event hook could not be installed");
    }

    let mut state = lock_state();
    if let Some(s) = state.as_mut() {
        s.taskbar_hwnd = Some(taskbar.hwnd);
        s.tray_notify_hwnd = tray_notify;
        s.win_event_hook = hook;
        s.taskbar_index = index;
    }
    true
}

fn taskbar_at_point(pt: POINT) -> Option<(usize, native_interop::TaskbarWindow)> {
    native_interop::find_taskbars()
        .into_iter()
        .enumerate()
        .find(|(_, taskbar)| {
            pt.x >= taskbar.rect.left
                && pt.x < taskbar.rect.right
                && pt.y >= taskbar.rect.top
                && pt.y < taskbar.rect.bottom
        })
}

fn tray_left_for_taskbar(taskbar_hwnd: HWND, taskbar_rect: RECT) -> i32 {
    let mut tray_left = taskbar_rect.right;
    if let Some(tray_hwnd) = native_interop::find_child_window(taskbar_hwnd, "TrayNotifyWnd") {
        if let Some(tray_rect) = native_interop::get_window_rect_safe(tray_hwnd) {
            tray_left = tray_rect.left;
        }
    }
    tray_left
}

fn clamp_offset_for_taskbar(taskbar_hwnd: HWND, taskbar_rect: RECT, offset: i32) -> i32 {
    let tray_left = tray_left_for_taskbar(taskbar_hwnd, taskbar_rect);
    let max_offset = (tray_left - taskbar_rect.left - total_widget_width()).max(0);
    offset.clamp(0, max_offset)
}

fn offset_for_drop_point(
    taskbar_hwnd: HWND,
    taskbar_rect: RECT,
    pt: POINT,
    drag_start_client_x: i32,
) -> i32 {
    let tray_left = tray_left_for_taskbar(taskbar_hwnd, taskbar_rect);
    let desired_left = pt.x - taskbar_rect.left - drag_start_client_x;
    let offset = tray_left - taskbar_rect.left - total_widget_width() - desired_left;
    clamp_offset_for_taskbar(taskbar_hwnd, taskbar_rect, offset)
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(feature = "self-update")]
fn update_check_interval() -> Duration {
    Duration::from_secs(24 * 60 * 60)
}

#[cfg(feature = "self-update")]
fn auto_update_check_due(last_update_check_unix: Option<u64>) -> bool {
    let Some(last_update_check_unix) = last_update_check_unix else {
        return true;
    };

    now_unix_secs().saturating_sub(last_update_check_unix) >= update_check_interval().as_secs()
}

#[cfg(feature = "self-update")]
fn schedule_auto_update_check(hwnd: HWND) {
    let delay_ms = {
        let state = lock_state();
        let Some(s) = state.as_ref() else {
            return;
        };

        if auto_update_check_due(s.last_update_check_unix) {
            None
        } else {
            let elapsed = now_unix_secs().saturating_sub(s.last_update_check_unix.unwrap_or(0));
            let remaining_secs = update_check_interval().as_secs().saturating_sub(elapsed);
            Some((remaining_secs.saturating_mul(1000)).min(u32::MAX as u64) as u32)
        }
    };

    unsafe {
        let _ = KillTimer(hwnd, TIMER_UPDATE_CHECK);
        if let Some(delay_ms) = delay_ms {
            SetTimer(hwnd, TIMER_UPDATE_CHECK, delay_ms.max(1), None);
        }
    }
}

/// Recompute the bar-fill value and display text for every cell from the
/// currently cached poll data (`state.data`), each cell's already-determined
/// `CellState` (set only by `do_poll`, untouched here), the current display
/// basis, and the current language. Safe to call on a countdown tick, a
/// display-basis change, or a language change — none of those change what
/// data is available, only how it should be shown right now. A non-`Ok`
/// cell's `section` lookup is irrelevant (`render_cell` ignores it), which
/// is what keeps a stale cached percentage from ever being drawn once a
/// poll has marked that cell as failed/loading/unavailable.
fn refresh_usage_texts(state: &mut AppState) {
    let strings = state.language.strings();
    let basis = state.display_basis;
    let data = state.data.as_ref();

    let claude_code = data.and_then(|d| d.claude_code.as_ref());
    let session = render_cell(
        state.session_state,
        claude_code.map(|u| &u.session),
        basis,
        strings,
    );
    state.session_percent = session.bar_percent;
    state.session_text = session.text;
    let weekly = render_cell(
        state.weekly_state,
        claude_code.map(|u| &u.weekly),
        basis,
        strings,
    );
    state.weekly_percent = weekly.bar_percent;
    state.weekly_text = weekly.text;

    let codex = data.and_then(|d| d.codex.as_ref());
    let codex_session = render_cell(
        state.codex_session_state,
        codex.map(|u| &u.session),
        basis,
        strings,
    );
    state.codex_session_percent = codex_session.bar_percent;
    state.codex_session_text = codex_session.text;
    let codex_weekly = render_cell(
        state.codex_weekly_state,
        codex.map(|u| &u.weekly),
        basis,
        strings,
    );
    state.codex_weekly_percent = codex_weekly.bar_percent;
    state.codex_weekly_text = codex_weekly.text;

    let antigravity = data.and_then(|d| d.antigravity.as_ref());
    let antigravity_session = render_cell(
        state.antigravity_session_state,
        antigravity.map(|u| &u.session),
        basis,
        strings,
    );
    state.antigravity_session_percent = antigravity_session.bar_percent;
    state.antigravity_session_text = antigravity_session.text;
    let antigravity_weekly = render_cell(
        state.antigravity_weekly_state,
        antigravity.map(|u| &u.weekly),
        basis,
        strings,
    );
    state.antigravity_weekly_percent = antigravity_weekly.bar_percent;
    state.antigravity_weekly_text = antigravity_weekly.text;
}

fn set_window_title(hwnd: HWND, strings: Strings) {
    unsafe {
        let title = native_interop::wide_str(strings.window_title);
        let _ = SetWindowTextW(hwnd, PCWSTR::from_raw(title.as_ptr()));
    }
}

#[cfg(feature = "self-update")]
fn show_info_message(hwnd: HWND, title: &str, message: &str) {
    unsafe {
        let title_wide = native_interop::wide_str(title);
        let message_wide = native_interop::wide_str(message);
        let _ = MessageBoxW(
            hwnd,
            PCWSTR::from_raw(message_wide.as_ptr()),
            PCWSTR::from_raw(title_wide.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

#[cfg(feature = "self-update")]
fn show_error_message(hwnd: HWND, title: &str, message: &str) {
    unsafe {
        let title_wide = native_interop::wide_str(title);
        let message_wide = native_interop::wide_str(message);
        let _ = MessageBoxW(
            hwnd,
            PCWSTR::from_raw(message_wide.as_ptr()),
            PCWSTR::from_raw(title_wide.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(feature = "self-update")]
fn show_update_prompt(hwnd: HWND, strings: Strings, release: &ReleaseDescriptor) -> bool {
    let message = strings
        .update_prompt_now
        .replace("{version}", &release.latest_version);

    unsafe {
        let title_wide = native_interop::wide_str(strings.update_available);
        let message_wide = native_interop::wide_str(&message);
        MessageBoxW(
            hwnd,
            PCWSTR::from_raw(message_wide.as_ptr()),
            PCWSTR::from_raw(title_wide.as_ptr()),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
}

fn apply_language_to_state(state: &mut AppState, language_override: Option<LanguageId>) {
    state.language_override = language_override;
    state.language = localization::resolve_language(language_override);
    set_window_title(state.hwnd.to_hwnd(), state.language.strings());
    refresh_usage_texts(state);
}

fn update_language_change() -> bool {
    let mut state = lock_state();
    let Some(app_state) = state.as_mut() else {
        return false;
    };

    if app_state.language_override.is_some() {
        return false;
    }

    let new_language = localization::detect_system_language();
    if new_language == app_state.language {
        return false;
    }

    apply_language_to_state(app_state, None);
    true
}

#[cfg(feature = "self-update")]
fn version_action_label(
    strings: Strings,
    language: LanguageId,
    install_channel: InstallChannel,
    status: &UpdateStatus,
) -> String {
    let current = env!("CARGO_PKG_VERSION");
    match status {
        UpdateStatus::Idle => format!("v{current} - {}", strings.check_for_updates),
        UpdateStatus::Checking => format!("v{current} - {}", strings.checking_for_updates),
        UpdateStatus::Applying => format!("v{current} - {}", strings.applying_update),
        UpdateStatus::UpToDate => format!("v{current} - {}", strings.up_to_date_short),
        UpdateStatus::Available(release) => match install_channel {
            InstallChannel::Portable => {
                format!(
                    "v{current} - {} v{}",
                    strings.update_to, release.latest_version
                )
            }
            InstallChannel::Winget => format!(
                "v{current} - {} v{}",
                localization::update_via_winget(language),
                release.latest_version
            ),
        },
    }
}

#[cfg(feature = "self-update")]
fn begin_update_check(hwnd: HWND, interactive: bool) {
    let send_hwnd = SendHwnd::from_hwnd(hwnd);
    let (strings, install_channel) = {
        let mut state = lock_state();
        let Some(app_state) = state.as_mut() else {
            return;
        };

        if matches!(
            app_state.update_status,
            UpdateStatus::Checking | UpdateStatus::Applying
        ) {
            if interactive {
                show_info_message(
                    hwnd,
                    app_state.language.strings().updates,
                    app_state.language.strings().update_in_progress,
                );
            }
            return;
        }

        app_state.update_status = UpdateStatus::Checking;
        (app_state.language.strings(), app_state.install_channel)
    };

    std::thread::spawn(move || {
        let hwnd = send_hwnd.to_hwnd();
        let checked_at = now_unix_secs();
        match updater::check_for_updates() {
            Ok(UpdateCheckResult::UpToDate) => {
                {
                    let mut state = lock_state();
                    if let Some(s) = state.as_mut() {
                        s.update_status = UpdateStatus::UpToDate;
                        s.last_update_check_unix = Some(checked_at);
                    }
                }
                save_state_settings();
                if interactive {
                    show_info_message(hwnd, strings.updates, strings.up_to_date);
                }
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_UPDATE_CHECK_COMPLETE, WPARAM(0), LPARAM(0));
                }
            }
            Ok(UpdateCheckResult::Available(release)) => {
                {
                    let mut state = lock_state();
                    if let Some(s) = state.as_mut() {
                        s.update_status = UpdateStatus::Available(release.clone());
                        s.last_update_check_unix = Some(checked_at);
                    }
                }
                save_state_settings();
                if interactive && show_update_prompt(hwnd, strings, &release) {
                    match install_channel {
                        InstallChannel::Portable => begin_update_apply(hwnd, release),
                        InstallChannel::Winget => begin_winget_update(hwnd),
                    }
                }
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_UPDATE_CHECK_COMPLETE, WPARAM(0), LPARAM(0));
                }
            }
            Err(error) => {
                {
                    let mut state = lock_state();
                    if let Some(s) = state.as_mut() {
                        s.update_status = UpdateStatus::Idle;
                        s.last_update_check_unix = Some(checked_at);
                    }
                }
                save_state_settings();
                if interactive {
                    let message = format!("{}.\n\n{}", strings.update_failed, error);
                    show_error_message(hwnd, strings.updates, &message);
                }
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_UPDATE_CHECK_COMPLETE, WPARAM(0), LPARAM(0));
                }
            }
        }
    });
}

#[cfg(feature = "self-update")]
fn begin_update_apply(hwnd: HWND, release: ReleaseDescriptor) {
    let send_hwnd = SendHwnd::from_hwnd(hwnd);
    let strings = {
        let mut state = lock_state();
        let Some(app_state) = state.as_mut() else {
            return;
        };

        if matches!(
            app_state.update_status,
            UpdateStatus::Checking | UpdateStatus::Applying
        ) {
            show_info_message(
                hwnd,
                app_state.language.strings().updates,
                app_state.language.strings().update_in_progress,
            );
            return;
        }

        app_state.update_status = UpdateStatus::Applying;
        app_state.language.strings()
    };

    std::thread::spawn(move || {
        let hwnd = send_hwnd.to_hwnd();
        match updater::begin_self_update(&release) {
            Ok(()) => unsafe {
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            },
            Err(error) => {
                {
                    let mut state = lock_state();
                    if let Some(s) = state.as_mut() {
                        s.update_status = UpdateStatus::Available(release);
                    }
                }
                let message = format!("{}.\n\n{}", strings.update_failed, error);
                show_error_message(hwnd, strings.updates, &message);
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_UPDATE_CHECK_COMPLETE, WPARAM(0), LPARAM(0));
                }
            }
        }
    });
}

#[cfg(feature = "self-update")]
fn begin_winget_update(hwnd: HWND) {
    let strings = {
        let state = lock_state();
        state.as_ref().map(|s| s.language.strings())
    }
    .unwrap_or(LanguageId::English.strings());

    match updater::begin_winget_update() {
        Ok(()) => unsafe {
            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        },
        Err(error) => {
            let message = format!("{}.\n\n{}", strings.update_failed, error);
            show_error_message(hwnd, strings.updates, &message);
        }
    }
}

const STARTUP_REGISTRY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_REGISTRY_KEY: &str = "ClaudeCodeUsageMonitor";

/// Returns true only if the startup registry value points to this executable.
fn is_startup_enabled() -> bool {
    unsafe {
        let path = native_interop::wide_str(STARTUP_REGISTRY_PATH);
        let key_name = native_interop::wide_str(STARTUP_REGISTRY_KEY);

        let mut hkey = HKEY::default();
        let result = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        );
        if result.is_err() {
            return false;
        }

        // Query the size of the value
        let mut data_size: u32 = 0;
        let result = RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(key_name.as_ptr()),
            None,
            None,
            None,
            Some(&mut data_size),
        );
        if result.is_err() || data_size == 0 {
            let _ = RegCloseKey(hkey);
            return false;
        }

        // Read the value
        let mut buf = vec![0u8; data_size as usize];
        let result = RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(key_name.as_ptr()),
            None,
            None,
            Some(buf.as_mut_ptr()),
            Some(&mut data_size),
        );
        let _ = RegCloseKey(hkey);
        if result.is_err() {
            return false;
        }

        // Convert the registry value (UTF-16) to a string
        let wide_slice =
            std::slice::from_raw_parts(buf.as_ptr() as *const u16, data_size as usize / 2);
        let reg_value = String::from_utf16_lossy(wide_slice)
            .trim_end_matches('\0')
            .to_string();

        // Get the current executable path
        let mut exe_buf = [0u16; 260];
        let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
        if len == 0 {
            return false;
        }
        let current_exe = String::from_utf16_lossy(&exe_buf[..len]);

        // Case-insensitive comparison (Windows paths are case-insensitive)
        reg_value.eq_ignore_ascii_case(&current_exe)
    }
}

fn set_startup_enabled(enable: bool) {
    unsafe {
        let path = native_interop::wide_str(STARTUP_REGISTRY_PATH);

        let mut hkey = HKEY::default();
        let result = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        );
        if result.is_err() {
            return;
        }

        let key_name = native_interop::wide_str(STARTUP_REGISTRY_KEY);

        if enable {
            let mut exe_buf = [0u16; 260];
            let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
            if len > 0 {
                // Write the wide string including null terminator
                let byte_len = ((len + 1) * 2) as u32;
                let _ = RegSetValueExW(
                    hkey,
                    PCWSTR::from_raw(key_name.as_ptr()),
                    0,
                    REG_SZ,
                    Some(std::slice::from_raw_parts(
                        exe_buf.as_ptr() as *const u8,
                        byte_len as usize,
                    )),
                );
            }
        } else {
            let _ = RegDeleteValueW(hkey, PCWSTR::from_raw(key_name.as_ptr()));
        }

        let _ = RegCloseKey(hkey);
    }
}

// Dimensions matching the C# version
const SEGMENT_W: i32 = 10;
const SEGMENT_H: i32 = 13;
const SEGMENT_GAP: i32 = 1;
const SEGMENT_COUNT: i32 = 10;
const CORNER_RADIUS: i32 = 2;

const LEFT_DIVIDER_W: i32 = 3;
/// Wider than the visible divider so the drag handle is easier to grab;
/// purely a hit-test width, does not affect drawing.
const DRAG_HANDLE_HIT_W: i32 = 10;
const DIVIDER_RIGHT_MARGIN: i32 = 10;
const LABEL_WIDTH: i32 = 18;
const LABEL_RIGHT_MARGIN: i32 = 10;
const BAR_RIGHT_MARGIN: i32 = 4;
/// Wide enough for the longest expected per-cell value+reset text — 3-digit
/// percent + " · " + reset-in word + countdown, no display-basis prefix
/// (that now lives once in the header row instead of on every cell). Worst
/// case measured against the actual localized `reset_in`/`day_suffix`
/// strings shipped in `src/localization/*.rs`: Japanese
/// "100% · リセットまで 7日" ≈ "100%"(~28px) + " · "(~14px) +
/// "リセットまで"(6 full-width glyphs ≈78px) + " "(~4px) + "7日"(~20px) ≈ 144px
/// at Segoe UI 12px (≈7px/Latin glyph, ≈13px/CJK full-width glyph). 160px
/// leaves a ~16px margin for other languages. This is a character-count
/// calculation, not a live GDI measurement (see completion report for why);
/// it has not been visually verified on this machine (no runtime access
/// here) and needs a home-PC check across all 11 languages.
const TEXT_WIDTH: i32 = 160;
const MODEL_RIGHT_MARGIN: i32 = 3;
const RIGHT_MARGIN: i32 = 1;
/// Height of each of the two header text rows (display-basis label, then
/// provider names) added above the existing 5h/7d bar rows.
const HEADER_ROW_H: i32 = 14;
/// 3px top margin + HEADER_ROW_H (basis label) + 2px + HEADER_ROW_H
/// (provider names) + 4px + SEGMENT_H (5h row) + 10px + SEGMENT_H (7d row)
/// + 5px bottom margin = 3+14+2+14+4+13+10+13+5 = 78. The 5h/7d row height
/// and the 10px gap between them are unchanged from before this feature;
/// only the two header rows and their margins are new. Not visually
/// verified on this machine — see completion report.
const WIDGET_HEIGHT: i32 = 78;

fn is_drag_handle_point(client_x: i32, client_y: i32) -> bool {
    let divider_h = sc(25);
    let divider_top = (sc(WIDGET_HEIGHT) - divider_h) / 2;
    client_x >= 0
        && client_x < sc(DRAG_HANDLE_HIT_W)
        && client_y >= divider_top
        && client_y < divider_top + divider_h
}

fn cursor_is_on_drag_handle(hwnd: HWND) -> bool {
    unsafe {
        let mut pt = POINT::default();
        if GetCursorPos(&mut pt).is_err() || !ScreenToClient(hwnd, &mut pt).as_bool() {
            return false;
        }
        is_drag_handle_point(pt.x, pt.y)
    }
}

fn active_model_count(show_claude_code: bool, show_codex: bool, show_antigravity: bool) -> i32 {
    (show_claude_code as i32 + show_codex as i32 + show_antigravity as i32).max(1)
}

fn row_bar_segment_count(active_models: i32) -> i32 {
    match active_models {
        1 => SEGMENT_COUNT,
        2 => 5,
        _ => 4,
    }
}

fn total_widget_width_for(active_models: i32) -> i32 {
    let bar_segments = row_bar_segment_count(active_models);
    let model_width = model_usage_width(bar_segments);

    sc(LEFT_DIVIDER_W)
        + sc(DIVIDER_RIGHT_MARGIN)
        + sc(LABEL_WIDTH)
        + sc(LABEL_RIGHT_MARGIN)
        + model_width * active_models
        + sc(MODEL_RIGHT_MARGIN) * (active_models - 1)
        + sc(RIGHT_MARGIN)
}

fn total_widget_width_for_state(state: &AppState) -> i32 {
    total_widget_width_for(active_model_count(
        state.show_claude_code,
        state.show_codex,
        state.show_antigravity,
    ))
}

fn total_widget_width() -> i32 {
    let active_models = {
        let state = lock_state();
        state
            .as_ref()
            .map(|s| active_model_count(s.show_claude_code, s.show_codex, s.show_antigravity))
            .unwrap_or(1)
    };
    total_widget_width_for(active_models)
}

fn claude_accent_color() -> Color {
    Color::from_hex("#D97757")
}

fn codex_accent_color(is_dark: bool) -> Color {
    if is_dark {
        Color::from_hex("#F5F5F5")
    } else {
        Color::from_hex("#1F1F1F")
    }
}

fn antigravity_accent_color() -> Color {
    Color::from_hex("#4285F4")
}

fn claude_usage_text_color(is_dark: bool) -> Color {
    if is_dark {
        Color::from_hex("#F09A7A")
    } else {
        Color::from_hex("#A94F32")
    }
}

fn codex_usage_text_color(is_dark: bool) -> Color {
    if is_dark {
        Color::from_hex("#F5F5F5")
    } else {
        Color::from_hex("#1F1F1F")
    }
}

fn antigravity_usage_text_color(is_dark: bool) -> Color {
    if is_dark {
        Color::from_hex("#8AB4F8")
    } else {
        Color::from_hex("#1967D2")
    }
}

pub fn run() {
    // Enable Per-Monitor DPI Awareness V2 for crisp rendering at any scale factor
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CURRENT_DPI.store(GetDpiForSystem(), Ordering::Relaxed);
    }
    diagnose::log("window::run started");

    // Single-instance guard: silently exit if another instance is running.
    // Exception: when relaunched after an explorer restart (ENV_RELAUNCH set),
    // wait for the previous instance to release the mutex, then take over.
    let is_relaunch = std::env::var(ENV_RELAUNCH).is_ok();
    let mutex_name = native_interop::wide_str("Global\\ClaudeCodeUsageMonitor");
    let _mutex = unsafe {
        let handle = CreateMutexW(None, true, PCWSTR::from_raw(mutex_name.as_ptr()));
        match handle {
            Ok(h) => {
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    if is_relaunch {
                        diagnose::log("relaunch: waiting for previous instance to exit");
                        let wait_result = WaitForSingleObject(h, 10_000);
                        if wait_result != WAIT_OBJECT_0 && wait_result != WAIT_ABANDONED {
                            diagnose::log(format!(
                                "startup aborted: previous instance did not exit cleanly ({wait_result:?})"
                            ));
                            return;
                        }
                    } else {
                        diagnose::log("startup aborted: another instance is already running");
                        return;
                    }
                }
                h
            }
            Err(error) => {
                diagnose::log_error(
                    "startup aborted: unable to create single-instance mutex",
                    error,
                );
                return;
            }
        }
    };

    let class_name = native_interop::wide_str("ClaudeCodeUsageMonitor");

    unsafe {
        let hinstance = GetModuleHandleW(PCWSTR::null()).unwrap();
        let (large_icon, small_icon) = load_embedded_app_icons();

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: HINSTANCE(hinstance.0),
            hIcon: large_icon,
            hIconSm: small_icon,
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
            ..Default::default()
        };

        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            diagnose::log("RegisterClassExW returned 0");
        }

        let settings = load_settings();
        let language_override = settings.language.as_deref().and_then(LanguageId::from_code);
        let language = localization::resolve_language(language_override);
        let install_channel = updater::current_install_channel();

        // Create as a top-level layered popup, anchored above the taskbar.
        let title = native_interop::wide_str(language.strings().window_title);
        let initial_model_count = active_model_count(
            settings.show_claude_code,
            settings.show_codex,
            settings.show_antigravity,
        );
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            PCWSTR::from_raw(class_name.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            WS_POPUP,
            0,
            0,
            total_widget_width_for(initial_model_count),
            sc(WIDGET_HEIGHT),
            HWND::default(),
            HMENU::default(),
            hinstance,
            None,
        )
        .unwrap();

        if !large_icon.is_invalid() {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                WPARAM(ICON_BIG as usize),
                LPARAM(large_icon.0 as isize),
            );
        }
        if !small_icon.is_invalid() {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                WPARAM(ICON_SMALL as usize),
                LPARAM(small_icon.0 as isize),
            );
        }

        diagnose::log(format!("main window created hwnd={:?}", hwnd));

        let is_dark = theme::is_dark_mode();

        {
            let mut state = lock_state();
            *state = Some(AppState {
                hwnd: SendHwnd::from_hwnd(hwnd),
                taskbar_hwnd: None,
                tray_notify_hwnd: None,
                win_event_hook: None,
                is_dark,
                embedded: false,
                language_override,
                language,
                install_channel,
                display_basis: settings.display_basis,
                display_density: settings.display_density,
                short_window_visibility: settings.short_window_visibility,
                short_window_alert_sensitivity: settings.short_window_alert_sensitivity,
                session_state: CellState::Loading,
                session_percent: None,
                session_text: String::new(),
                weekly_state: CellState::Loading,
                weekly_percent: None,
                weekly_text: String::new(),
                codex_session_state: CellState::Loading,
                codex_session_percent: None,
                codex_session_text: String::new(),
                codex_weekly_state: CellState::Loading,
                codex_weekly_percent: None,
                codex_weekly_text: String::new(),
                antigravity_session_state: CellState::Loading,
                antigravity_session_percent: None,
                antigravity_session_text: String::new(),
                antigravity_weekly_state: CellState::Loading,
                antigravity_weekly_percent: None,
                antigravity_weekly_text: String::new(),
                show_claude_code: settings.show_claude_code,
                show_codex: settings.show_codex,
                show_antigravity: settings.show_antigravity,
                data: None,
                poll_interval_ms: settings.poll_interval_ms,
                retry_count: 0,
                force_notify_auth_error: false,
                auth_error_paused_polling: false,
                auth_watch_mode: poller::CredentialWatchMode::ActiveSource,
                auth_watch_snapshot: Vec::new(),
                last_poll_ok: false,
                update_status: UpdateStatus::Idle,
                last_update_check_unix: settings.last_update_check_unix,
                taskbar_index: settings.taskbar_index,
                tray_offset: settings.tray_offset,
                dragging: false,
                drag_start_mouse_x: 0,
                drag_start_client_x: 0,
                drag_start_offset: 0,
                widget_visible: settings.widget_visible,
                always_on_top: settings.always_on_top,
            });
            if let Some(s) = state.as_mut() {
                refresh_usage_texts(s);
            }
        }

        // Locate the taskbar to anchor the popup against; this does not
        // reparent the window. Regardless of whether a taskbar is found,
        // the window is always initialized as a top-level popup.
        select_taskbar_anchor(settings.taskbar_index);

        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        // Explicitly apply NOTOPMOST (not just skip TOPMOST) when the saved
        // preference is off, so no stale topmost z-order can linger.
        apply_always_on_top(hwnd, settings.always_on_top);

        // Register system tray icon(s)
        sync_tray_icons(hwnd);

        // Position and show (only if widget_visible preference is true)
        position_at_taskbar();
        if settings.widget_visible {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        diagnose::log("window shown");

        // Initial render via UpdateLayeredWindow (for embedded) or InvalidateRect (fallback)
        render_layered();

        // Poll timer: 15 minutes
        let initial_poll_ms = {
            let state = lock_state();
            state
                .as_ref()
                .map(|s| s.poll_interval_ms)
                .unwrap_or(POLL_15_MIN)
        };
        SetTimer(hwnd, TIMER_POLL, initial_poll_ms, None);

        // Watch for explorer.exe restarts so we can re-add the tray icon and
        // re-select the taskbar anchor (the shell discards tray registrations
        // when it restarts). Runs on a dedicated thread, independent of the
        // window's own message loop.
        spawn_taskbar_watchdog();

        // Initial poll
        let send_hwnd = SendHwnd::from_hwnd(hwnd);
        std::thread::spawn(move || {
            diagnose::log("initial poll thread started");
            do_poll(send_hwnd);
        });

        #[cfg(feature = "self-update")]
        {
            schedule_auto_update_check(hwnd);
            let should_check_updates = {
                let state = lock_state();
                state
                    .as_ref()
                    .map(|s| auto_update_check_due(s.last_update_check_unix))
                    .unwrap_or(false)
            };
            if should_check_updates {
                begin_update_check(hwnd, false);
            }
        }

        // Initial theme check
        check_theme_change();

        // Message loop
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Render widget content and push to the layered window via UpdateLayeredWindow.
/// Renders fully opaque with the actual taskbar background colour so that
/// ClearType sub-pixel font rendering can be used for crisp, OS-native text.
fn render_layered() {
    refresh_dpi();
    let (
        hwnd_val,
        is_dark,
        embedded,
        strings,
        display_basis,
        session_pct,
        session_text,
        weekly_pct,
        weekly_text,
        codex_session_pct,
        codex_session_text,
        codex_weekly_pct,
        codex_weekly_text,
        antigravity_session_pct,
        antigravity_session_text,
        antigravity_weekly_pct,
        antigravity_weekly_text,
        show_claude_code,
        show_codex,
        show_antigravity,
    ) = {
        let state = lock_state();
        match state.as_ref() {
            Some(s) => (
                s.hwnd,
                s.is_dark,
                s.embedded,
                s.language.strings(),
                s.display_basis,
                s.session_percent,
                s.session_text.clone(),
                s.weekly_percent,
                s.weekly_text.clone(),
                s.codex_session_percent,
                s.codex_session_text.clone(),
                s.codex_weekly_percent,
                s.codex_weekly_text.clone(),
                s.antigravity_session_percent,
                s.antigravity_session_text.clone(),
                s.antigravity_weekly_percent,
                s.antigravity_weekly_text.clone(),
                s.show_claude_code,
                s.show_codex,
                s.show_antigravity,
            ),
            None => return,
        }
    };

    let hwnd = hwnd_val.to_hwnd();

    // For non-embedded fallback, just invalidate and let WM_PAINT handle it
    if !embedded {
        unsafe {
            let _ = InvalidateRect(hwnd, None, false);
        }
        return;
    }

    let width = total_widget_width();
    let height = sc(WIDGET_HEIGHT);

    let accent = claude_accent_color();
    let codex_accent = codex_accent_color(is_dark);
    let antigravity_accent = antigravity_accent_color();
    let track = if is_dark {
        Color::from_hex("#444444")
    } else {
        Color::from_hex("#AAAAAA")
    };
    let text_color = if is_dark {
        Color::from_hex("#888888")
    } else {
        Color::from_hex("#404040")
    };
    let bg_color = if is_dark {
        Color::from_hex("#1C1C1C")
    } else {
        Color::from_hex("#F3F3F3")
    };

    unsafe {
        let screen_dc = GetDC(hwnd);

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let mem_dc = CreateCompatibleDC(screen_dc);
        let dib =
            CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();

        if dib.is_invalid() || bits.is_null() {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(hwnd, screen_dc);
            return;
        }

        let old_bmp = SelectObject(mem_dc, dib);
        let pixel_count = (width * height) as usize;

        // Render once with the actual taskbar background colour.
        // Using an opaque background lets us use CLEARTYPE_QUALITY for
        // sub-pixel font rendering that matches the rest of the OS.
        paint_content(
            mem_dc,
            width,
            height,
            is_dark,
            &bg_color,
            &text_color,
            &accent,
            &track,
            strings,
            display_basis,
            session_pct,
            &session_text,
            weekly_pct,
            &weekly_text,
            codex_session_pct,
            &codex_session_text,
            codex_weekly_pct,
            &codex_weekly_text,
            antigravity_session_pct,
            &antigravity_session_text,
            antigravity_weekly_pct,
            &antigravity_weekly_text,
            show_claude_code,
            show_codex,
            show_antigravity,
            &codex_accent,
            &antigravity_accent,
        );

        // Background pixels → alpha 1 (nearly invisible but still hittable for right-click).
        // Content pixels → fully opaque (preserves ClearType sub-pixel rendering).
        let bg_bgr = bg_color.to_colorref();
        let pixel_data = std::slice::from_raw_parts_mut(bits as *mut u32, pixel_count);
        for px in pixel_data.iter_mut() {
            let rgb = *px & 0x00FFFFFF;
            if rgb == bg_bgr {
                *px = 0x01000000;
            } else {
                *px = rgb | 0xFF000000;
            }
        }

        // Push to window via UpdateLayeredWindow
        let pt_src = POINT { x: 0, y: 0 };
        let sz = SIZE {
            cx: width,
            cy: height,
        };
        let blend = BLENDFUNCTION {
            BlendOp: 0, // AC_SRC_OVER
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: 1, // AC_SRC_ALPHA
        };

        let _ = UpdateLayeredWindow(
            hwnd,
            screen_dc,
            None,
            Some(&sz),
            mem_dc,
            Some(&pt_src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );

        // Cleanup
        SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(hwnd, screen_dc);
    }
}

/// Paint all widget content onto a DC with a given background color.
fn paint_content(
    hdc: HDC,
    width: i32,
    height: i32,
    is_dark: bool,
    bg: &Color,
    text_color: &Color,
    accent: &Color,
    track: &Color,
    strings: Strings,
    display_basis: DisplayBasis,
    session_pct: Option<f64>,
    session_text: &str,
    weekly_pct: Option<f64>,
    weekly_text: &str,
    codex_session_pct: Option<f64>,
    codex_session_text: &str,
    codex_weekly_pct: Option<f64>,
    codex_weekly_text: &str,
    antigravity_session_pct: Option<f64>,
    antigravity_session_text: &str,
    antigravity_weekly_pct: Option<f64>,
    antigravity_weekly_text: &str,
    show_claude_code: bool,
    show_codex: bool,
    show_antigravity: bool,
    codex_accent: &Color,
    antigravity_accent: &Color,
) {
    unsafe {
        let client_rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };

        let bg_brush = CreateSolidBrush(COLORREF(bg.to_colorref()));
        FillRect(hdc, &client_rect, bg_brush);
        let _ = DeleteObject(bg_brush);

        // Left divider
        let divider_h = sc(25);
        let divider_top = (height - divider_h) / 2;
        let divider_bottom = divider_top + divider_h;

        let (div_left, div_right) = if is_dark {
            ((80, 80, 80), (40, 40, 40))
        } else {
            ((160, 160, 160), (230, 230, 230))
        };

        let left_brush = CreateSolidBrush(COLORREF(native_interop::colorref(
            div_left.0, div_left.1, div_left.2,
        )));
        let left_rect = RECT {
            left: 0,
            top: divider_top,
            right: sc(2),
            bottom: divider_bottom,
        };
        FillRect(hdc, &left_rect, left_brush);
        let _ = DeleteObject(left_brush);

        let right_brush = CreateSolidBrush(COLORREF(native_interop::colorref(
            div_right.0,
            div_right.1,
            div_right.2,
        )));
        let right_rect = RECT {
            left: sc(2),
            top: divider_top,
            right: sc(3),
            bottom: divider_bottom,
        };
        FillRect(hdc, &right_rect, right_brush);
        let _ = DeleteObject(right_brush);

        let content_x = sc(LEFT_DIVIDER_W) + sc(DIVIDER_RIGHT_MARGIN);
        let row2_y = height - sc(5) - sc(SEGMENT_H);
        let row1_y = row2_y - sc(10) - sc(SEGMENT_H);
        let provider_header_y = row1_y - sc(4) - sc(HEADER_ROW_H);
        let basis_label_y = provider_header_y - sc(2) - sc(HEADER_ROW_H);

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, COLORREF(text_color.to_colorref()));

        let font_name = native_interop::wide_str("Segoe UI");
        let font = CreateFontW(
            sc(-12),
            0,
            0,
            0,
            FW_MEDIUM.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_TT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            PCWSTR::from_raw(font_name.as_ptr()),
        );
        let old_font = SelectObject(hdc, font);

        let basis_label = match display_basis {
            DisplayBasis::UsedPercentage => strings.used_percentage,
            DisplayBasis::RemainingAllowance => strings.remaining_allowance,
        };
        draw_basis_label_row(
            hdc,
            content_x,
            basis_label_y,
            width - sc(RIGHT_MARGIN),
            text_color,
            basis_label,
        );
        draw_provider_header_row(
            hdc,
            content_x,
            provider_header_y,
            text_color,
            strings,
            show_claude_code,
            show_codex,
            show_antigravity,
        );

        draw_row(
            hdc,
            content_x,
            row1_y,
            is_dark,
            text_color,
            strings.session_window,
            session_pct,
            session_text,
            codex_session_pct,
            codex_session_text,
            antigravity_session_pct,
            antigravity_session_text,
            show_claude_code,
            show_codex,
            show_antigravity,
            accent,
            codex_accent,
            antigravity_accent,
            track,
        );
        draw_row(
            hdc,
            content_x,
            row2_y,
            is_dark,
            text_color,
            strings.weekly_window,
            weekly_pct,
            weekly_text,
            codex_weekly_pct,
            codex_weekly_text,
            antigravity_weekly_pct,
            antigravity_weekly_text,
            show_claude_code,
            show_codex,
            show_antigravity,
            accent,
            codex_accent,
            antigravity_accent,
            track,
        );

        SelectObject(hdc, old_font);
        let _ = DeleteObject(font);
    }
}

fn do_poll(send_hwnd: SendHwnd) {
    let hwnd = send_hwnd.to_hwnd();
    let (show_claude_code, show_codex, show_antigravity) = {
        let state = lock_state();
        state
            .as_ref()
            .map(|s| (s.show_claude_code, s.show_codex, s.show_antigravity))
            .unwrap_or((true, false, false))
    };

    let report = poller::poll_report(show_claude_code, show_codex, show_antigravity);

    match report.clone().into_app_usage_data() {
        Ok(data) => {
            persist_poll_snapshot(&report);

            let mut state = lock_state();
            if let Some(s) = state.as_mut() {
                // Classify availability straight from this poll's outcome for
                // every provider (not just the ones that succeeded): a
                // provider that failed this round while others succeeded
                // must not keep showing its old percentage as current.
                let (session_state, weekly_state) = poll_cell_states(&report.claude_code);
                s.session_state = session_state;
                s.weekly_state = weekly_state;
                let (codex_session_state, codex_weekly_state) = poll_cell_states(&report.codex);
                s.codex_session_state = codex_session_state;
                s.codex_weekly_state = codex_weekly_state;
                let (antigravity_session_state, antigravity_weekly_state) =
                    poll_cell_states(&report.antigravity);
                s.antigravity_session_state = antigravity_session_state;
                s.antigravity_weekly_state = antigravity_weekly_state;

                // Stop fast-poll if reset data is now fresh
                if !poller::app_is_past_reset(&data) {
                    unsafe {
                        let _ = KillTimer(hwnd, TIMER_RESET_POLL);
                    }
                }

                merge_successful_providers(&mut s.data, &report);
                s.last_poll_ok = true;
                refresh_usage_texts(s);

                // Recovered from errors — restore normal poll interval
                if s.retry_count > 0 {
                    s.retry_count = 0;
                    let interval = s.poll_interval_ms;
                    unsafe {
                        SetTimer(hwnd, TIMER_POLL, interval, None);
                    }
                }
                s.force_notify_auth_error = false;
                s.auth_error_paused_polling = false;
                s.auth_watch_mode = poller::CredentialWatchMode::ActiveSource;
                s.auth_watch_snapshot.clear();
            }

            unsafe {
                let _ = PostMessageW(hwnd, WM_APP_USAGE_UPDATED, WPARAM(0), LPARAM(0));
            }
        }
        Err(e) => {
            let auth_watch = match e {
                poller::PollError::AuthRequired | poller::PollError::TokenExpired
                    if show_antigravity && !show_claude_code && !show_codex =>
                {
                    Some((
                        poller::CredentialWatchMode::Antigravity,
                        poller::credential_watch_snapshot(poller::CredentialWatchMode::Antigravity),
                    ))
                }
                poller::PollError::AuthRequired | poller::PollError::TokenExpired => Some((
                    poller::CredentialWatchMode::ActiveSource,
                    poller::credential_watch_snapshot(poller::CredentialWatchMode::ActiveSource),
                )),
                poller::PollError::NoCredentials => Some((
                    poller::CredentialWatchMode::AllSources,
                    poller::credential_watch_snapshot(poller::CredentialWatchMode::AllSources),
                )),
                poller::PollError::RequestFailed => None,
            };
            // Distinguish auth-required errors from transient errors.
            let notify_auth_error = {
                let mut state = lock_state();
                let mut should_notify = false;
                if let Some(s) = state.as_mut() {
                    s.last_poll_ok = false;

                    // The whole poll failed, but classify per-provider from
                    // `report` anyway (Disabled/Error only here): this
                    // distinguishes NotConfigured/FetchFailed/Retrying per
                    // provider instead of collapsing everything into one
                    // generic error word, and `refresh_usage_texts` below
                    // reads these states to keep the bar unfilled rather
                    // than leaving the last successful percentage on screen.
                    let (session_state, weekly_state) = poll_cell_states(&report.claude_code);
                    s.session_state = session_state;
                    s.weekly_state = weekly_state;
                    let (codex_session_state, codex_weekly_state) = poll_cell_states(&report.codex);
                    s.codex_session_state = codex_session_state;
                    s.codex_weekly_state = codex_weekly_state;
                    let (antigravity_session_state, antigravity_weekly_state) =
                        poll_cell_states(&report.antigravity);
                    s.antigravity_session_state = antigravity_session_state;
                    s.antigravity_weekly_state = antigravity_weekly_state;
                    // No-op in practice today (a total-failure `report` never
                    // contains a `Success` outcome), but keeps this branch
                    // using the exact same update path as the success branch
                    // instead of relying on that invariant.
                    merge_successful_providers(&mut s.data, &report);
                    refresh_usage_texts(s);

                    match auth_watch {
                        Some((watch_mode, watch_snapshot)) => {
                            // Only show the balloon on the first failure so it doesn't spam.
                            if s.retry_count == 0 || s.force_notify_auth_error {
                                should_notify = true;
                            }
                            s.force_notify_auth_error = false;
                            s.auth_error_paused_polling = true;
                            s.auth_watch_mode = watch_mode;
                            s.auth_watch_snapshot = watch_snapshot;
                            s.retry_count = s.retry_count.saturating_add(1);
                            unsafe {
                                let _ = KillTimer(hwnd, TIMER_POLL);
                                let _ = KillTimer(hwnd, TIMER_RESET_POLL);
                                let _ = KillTimer(hwnd, TIMER_COUNTDOWN);
                                SetTimer(hwnd, TIMER_POLL, s.poll_interval_ms, None);
                            }
                        }
                        _ => {
                            // Transient network / credential-missing errors: exponential backoff.
                            s.force_notify_auth_error = false;
                            s.auth_error_paused_polling = false;
                            s.auth_watch_mode = poller::CredentialWatchMode::ActiveSource;
                            s.auth_watch_snapshot.clear();
                            s.retry_count = s.retry_count.saturating_add(1);
                            let backoff = RETRY_BASE_MS.saturating_mul(
                                1u32.checked_shl(s.retry_count - 1).unwrap_or(u32::MAX),
                            );
                            let retry_ms = backoff.min(s.poll_interval_ms);
                            unsafe {
                                let _ = KillTimer(hwnd, TIMER_RESET_POLL);
                                SetTimer(hwnd, TIMER_POLL, retry_ms, None);
                            }
                        }
                    }
                }
                should_notify
            };

            if notify_auth_error {
                let balloon = {
                    let state = lock_state();
                    state.as_ref().map(|s| {
                        if s.show_claude_code {
                            (
                                s.language.strings(),
                                tray_icon::TrayIconKind::Claude,
                                s.language.strings().token_expired_title,
                                s.language.strings().token_expired_body,
                            )
                        } else if s.show_codex {
                            (
                                s.language.strings(),
                                tray_icon::TrayIconKind::Codex,
                                s.language.strings().codex_token_expired_title,
                                s.language.strings().codex_token_expired_body,
                            )
                        } else {
                            (
                                s.language.strings(),
                                tray_icon::TrayIconKind::Antigravity,
                                s.language.strings().antigravity_token_expired_title,
                                s.language.strings().antigravity_token_expired_body,
                            )
                        }
                    })
                };
                if let Some((_strings, kind, title, body)) = balloon {
                    tray_icon::notify_balloon(hwnd, kind, title, body);
                }
            }

            unsafe {
                let _ = PostMessageW(hwnd, WM_APP_USAGE_UPDATED, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// Persist a snapshot of a successful poll. Failures here are logged as fixed,
/// non-identifying warnings and never affect the already-completed poll result
/// or the next poll.
fn persist_poll_snapshot(report: &poller::PollReport) {
    let Some(local_data_root) = snapshot_store::local_data_root() else {
        diagnose::log("snapshot persistence skipped: local data root unavailable");
        return;
    };

    let machine_id = match snapshot_store::ensure_machine_id(&local_data_root) {
        Ok(machine_id) => machine_id,
        Err(_) => {
            diagnose::log("snapshot persistence skipped: machine id unavailable");
            return;
        }
    };

    let snapshot =
        match snapshot_schema::snapshot_from_poll_report(&machine_id, report, SystemTime::now()) {
            Ok(snapshot) => snapshot,
            Err(_) => {
                diagnose::log("snapshot persistence skipped: unable to build snapshot");
                return;
            }
        };

    let paths = snapshot_store::SnapshotPaths::new(&local_data_root, &machine_id);
    let outcome = snapshot_store::persist_snapshot(&paths, &snapshot);
    match (outcome.current, outcome.history) {
        (snapshot_store::PersistResult::Saved, snapshot_store::PersistResult::Saved) => {}
        (snapshot_store::PersistResult::Failed, snapshot_store::PersistResult::Saved) => {
            diagnose::log("snapshot persistence warning: current snapshot save failed");
        }
        (snapshot_store::PersistResult::Saved, snapshot_store::PersistResult::Failed) => {
            diagnose::log("snapshot persistence warning: history snapshot save failed");
        }
        (snapshot_store::PersistResult::Failed, snapshot_store::PersistResult::Failed) => {
            diagnose::log("snapshot persistence warning: current and history snapshot save failed");
        }
    }
}

fn schedule_countdown_timer() {
    let state = lock_state();
    let s = match state.as_ref() {
        Some(s) => s,
        None => return,
    };

    let hwnd = s.hwnd.to_hwnd();
    if !s.last_poll_ok {
        unsafe {
            let _ = KillTimer(hwnd, TIMER_COUNTDOWN);
            let _ = KillTimer(hwnd, TIMER_RESET_POLL);
        }
        return;
    }

    let data = match &s.data {
        Some(d) => d,
        None => return,
    };

    // If a reset time has passed, poll every 5s to pick up fresh data
    if poller::app_is_past_reset(data) {
        unsafe {
            SetTimer(hwnd, TIMER_RESET_POLL, 5_000, None);
        }
    }

    let delays = [
        data.claude_code
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.session.resets_at)),
        data.claude_code
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.weekly.resets_at)),
        data.codex
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.session.resets_at)),
        data.codex
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.weekly.resets_at)),
        data.antigravity
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.session.resets_at)),
        data.antigravity
            .as_ref()
            .and_then(|usage| poller::time_until_display_change(usage.weekly.resets_at)),
    ];
    let min_delay = delays.into_iter().flatten().min();

    let ms = min_delay
        .unwrap_or(Duration::from_secs(60))
        .as_millis()
        .max(1000) as u32;

    unsafe {
        SetTimer(hwnd, TIMER_COUNTDOWN, ms, None);
    }
}

fn check_theme_change() {
    let new_dark = theme::is_dark_mode();
    let changed = {
        let mut state = lock_state();
        if let Some(s) = state.as_mut() {
            if s.is_dark != new_dark {
                s.is_dark = new_dark;
                true
            } else {
                false
            }
        } else {
            false
        }
    };
    if changed {
        render_layered();
    }
}

fn check_language_change() {
    if update_language_change() {
        render_layered();
    }
}

fn update_display() {
    let mut state = lock_state();
    let s = match state.as_mut() {
        Some(s) => s,
        None => return,
    };

    // Don't overwrite error text with stale cached data
    if !s.last_poll_ok {
        return;
    }

    refresh_usage_texts(s);
}

fn suppress_tray_reposition_for(duration: Duration) {
    let mut until = SUPPRESS_TRAY_REPOSITION_UNTIL
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *until = Some(Instant::now() + duration);
}

fn tray_reposition_is_suppressed() -> bool {
    let now = Instant::now();
    let mut until = SUPPRESS_TRAY_REPOSITION_UNTIL
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    match *until {
        Some(deadline) if now < deadline => true,
        Some(_) => {
            *until = None;
            false
        }
        None => false,
    }
}

fn position_at_taskbar() {
    refresh_dpi();
    // Drop the app-state lock before any Win32 call that may synchronously
    // re-enter our window procedure.
    let (hwnd, tray_offset, taskbar_hwnd) = {
        let state = lock_state();
        let s = match state.as_ref() {
            Some(s) => s,
            None => return,
        };

        // Don't fight the user's drag
        if s.dragging {
            return;
        }

        let taskbar_hwnd = match s.taskbar_hwnd {
            Some(h) => h,
            None => {
                diagnose::log("position_at_taskbar skipped: no taskbar handle");
                return;
            }
        };

        (s.hwnd.to_hwnd(), s.tray_offset, taskbar_hwnd)
    };

    let taskbar_rect = match native_interop::get_taskbar_rect(taskbar_hwnd) {
        Some(r) => r,
        None => {
            diagnose::log("position_at_taskbar skipped: unable to query taskbar rect");
            return;
        }
    };

    // The popup's usable bounds: the monitor's work area (screen minus the
    // taskbar), so the popup never overlaps the taskbar. If the work area
    // can't be queried, fall back to "everything above the taskbar,
    // unbounded at the top" rather than treating it as (0, 0).
    let work_area = native_interop::get_monitor_work_area(taskbar_hwnd).unwrap_or(RECT {
        left: taskbar_rect.left,
        top: i32::MIN / 2,
        right: taskbar_rect.right,
        bottom: taskbar_rect.top,
    });

    let mut tray_left = taskbar_rect.right;

    if let Some(tray_hwnd) = native_interop::find_child_window(taskbar_hwnd, "TrayNotifyWnd") {
        if let Some(tray_rect) = native_interop::get_window_rect_safe(tray_hwnd) {
            tray_left = tray_rect.left;
        }
    }

    let widget_width = total_widget_width();
    let max_offset = (tray_left - taskbar_rect.left - widget_width).max(0);
    let tray_offset = tray_offset.clamp(0, max_offset);
    let offset_changed = {
        let mut state = lock_state();
        if let Some(s) = state.as_mut() {
            if s.tray_offset != tray_offset {
                s.tray_offset = tray_offset;
                true
            } else {
                false
            }
        } else {
            false
        }
    };
    if offset_changed {
        save_state_settings();
    }

    let widget_height = sc(WIDGET_HEIGHT);
    let y = compute_popup_y(work_area.top, work_area.bottom, widget_height);
    let desired_x = tray_left - widget_width - tray_offset;
    let x = clamp_popup_x(desired_x, work_area.left, work_area.right, widget_width);
    native_interop::move_window(hwnd, x, y, widget_width, widget_height);
    diagnose::log(format!(
        "positioned popup at x={x} y={y} w={widget_width} h={widget_height}"
    ));
}

/// Compute the popup's top-left Y so its bottom edge sits flush with
/// `work_area_bottom` (the taskbar's top edge, for a bottom-docked
/// taskbar), extending upward into the work area. If the popup is taller
/// than the work area itself, `work_area_top` wins (the popup may then
/// extend past `work_area_bottom` as an unavoidable last resort - there is
/// no space above the taskbar tall enough to fit it).
fn compute_popup_y(work_area_top: i32, work_area_bottom: i32, popup_height: i32) -> i32 {
    (work_area_bottom - popup_height).max(work_area_top)
}

/// Clamp the popup's desired X so it stays within the work area
/// horizontally. If the popup is wider than the work area itself,
/// `work_area_left` wins (mirrors `compute_popup_y`'s last-resort rule).
fn clamp_popup_x(
    desired_x: i32,
    work_area_left: i32,
    work_area_right: i32,
    popup_width: i32,
) -> i32 {
    let max_x = (work_area_right - popup_width).max(work_area_left);
    desired_x.clamp(work_area_left, max_x)
}

/// WinEvent callback for tray icon location changes
unsafe extern "system" fn on_tray_location_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    static LAST_REPOSITION: Mutex<Option<std::time::Instant>> = Mutex::new(None);

    let is_tray = {
        let state = lock_state();
        state
            .as_ref()
            .and_then(|s| s.tray_notify_hwnd)
            .map(|h| h == hwnd)
            .unwrap_or(false)
    };

    if is_tray {
        if tray_reposition_is_suppressed() {
            return;
        }

        let should_reposition = {
            let mut last = LAST_REPOSITION.lock().unwrap_or_else(|e| e.into_inner());
            let now = std::time::Instant::now();
            if last
                .map(|t| now.duration_since(t).as_millis() > 500)
                .unwrap_or(true)
            {
                *last = Some(now);
                true
            } else {
                false
            }
        };
        if should_reposition {
            position_at_taskbar();
            render_layered();
        }
    }
}

/// Main window procedure
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            // For non-embedded fallback, paint normally
            let embedded = {
                let state = lock_state();
                state.as_ref().map(|s| s.embedded).unwrap_or(false)
            };
            if embedded {
                // Layered windows don't use WM_PAINT; just validate the region
                let mut ps = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            } else {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                paint(hdc, hwnd);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_DISPLAYCHANGE | WM_DPICHANGED_MSG | WM_SETTINGCHANGE => {
            if msg == WM_DPICHANGED_MSG {
                let new_dpi = (wparam.0 & 0xFFFF) as u32;
                CURRENT_DPI.store(new_dpi, Ordering::Relaxed);
            }
            if msg == WM_SETTINGCHANGE {
                check_theme_change();
                check_language_change();
            }
            refresh_dpi();
            position_at_taskbar();
            render_layered();
            LRESULT(0)
        }
        WM_TIMER => {
            let timer_id = wparam.0;
            match timer_id {
                TIMER_POLL => {
                    let auth_watch = {
                        let state = lock_state();
                        state.as_ref().map(|s| {
                            (
                                s.auth_error_paused_polling,
                                s.auth_watch_mode,
                                s.auth_watch_snapshot.clone(),
                            )
                        })
                    };
                    match auth_watch {
                        Some((true, watch_mode, previous_snapshot)) => {
                            let current_snapshot = poller::credential_watch_snapshot(watch_mode);
                            if current_snapshot != previous_snapshot {
                                let mut state = lock_state();
                                if let Some(s) = state.as_mut() {
                                    if s.auth_error_paused_polling
                                        && s.auth_watch_mode == watch_mode
                                    {
                                        s.auth_watch_snapshot = current_snapshot;
                                    }
                                }
                                drop(state);
                                let sh = SendHwnd::from_hwnd(hwnd);
                                std::thread::spawn(move || {
                                    do_poll(sh);
                                });
                            }
                        }
                        Some((false, _, _)) => {
                            let sh = SendHwnd::from_hwnd(hwnd);
                            std::thread::spawn(move || {
                                do_poll(sh);
                            });
                        }
                        None => {}
                    }
                }
                TIMER_COUNTDOWN => {
                    update_display();
                    render_layered();
                    schedule_countdown_timer();
                }
                TIMER_RESET_POLL => {
                    let should_poll = {
                        let state = lock_state();
                        state
                            .as_ref()
                            .map(|s| !s.auth_error_paused_polling)
                            .unwrap_or(false)
                    };
                    if should_poll {
                        let sh = SendHwnd::from_hwnd(hwnd);
                        std::thread::spawn(move || {
                            do_poll(sh);
                        });
                    }
                }
                #[cfg(feature = "self-update")]
                TIMER_UPDATE_CHECK => {
                    begin_update_check(hwnd, false);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_USAGE_UPDATED => {
            check_theme_change();
            check_language_change();
            render_layered();
            schedule_countdown_timer();
            suppress_tray_reposition_for(Duration::from_millis(
                TRAY_ICON_UPDATE_REPOSITION_SUPPRESS_MS,
            ));
            sync_tray_icons(hwnd);
            LRESULT(0)
        }
        #[cfg(feature = "self-update")]
        WM_APP_UPDATE_CHECK_COMPLETE => {
            schedule_auto_update_check(hwnd);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let is_dragging = {
                let state = lock_state();
                state.as_ref().map(|s| s.dragging).unwrap_or(false)
            };
            if is_dragging {
                let cursor = LoadCursorW(HINSTANCE::default(), IDC_SIZEWE).unwrap_or_default();
                SetCursor(cursor);
                return LRESULT(1);
            }
            if cursor_is_on_drag_handle(hwnd) {
                let cursor = LoadCursorW(HINSTANCE::default(), IDC_SIZEWE).unwrap_or_default();
                SetCursor(cursor);
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONDOWN => {
            let client_x = (lparam.0 & 0xFFFF) as i16 as i32;
            let client_y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            if !is_drag_handle_point(client_x, client_y) {
                return LRESULT(0);
            }

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let mut state = lock_state();
            if let Some(s) = state.as_mut() {
                s.dragging = true;
                s.drag_start_mouse_x = pt.x;
                s.drag_start_client_x = client_x;
                s.drag_start_offset = s.tray_offset;
            }
            SetCapture(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let is_dragging = {
                let state = lock_state();
                state.as_ref().map(|s| s.dragging).unwrap_or(false)
            };
            if is_dragging {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let move_target = {
                    let mut state = lock_state();
                    let s = match state.as_mut() {
                        Some(s) => s,
                        None => return LRESULT(0),
                    };

                    // Moving mouse left = positive delta = larger offset (further left)
                    let delta = s.drag_start_mouse_x - pt.x;
                    let mut new_offset = s.drag_start_offset + delta;

                    // Clamp: offset >= 0 (can't go right of default)
                    if new_offset < 0 {
                        new_offset = 0;
                    }

                    let taskbar_hwnd = s.taskbar_hwnd;
                    let hwnd_val = s.hwnd.to_hwnd();

                    // Clamp: don't go past left edge of taskbar
                    if let Some(taskbar_hwnd) = taskbar_hwnd {
                        if let Some(taskbar_rect) = native_interop::get_taskbar_rect(taskbar_hwnd) {
                            let mut tray_left = taskbar_rect.right;
                            if let Some(tray_hwnd) =
                                native_interop::find_child_window(taskbar_hwnd, "TrayNotifyWnd")
                            {
                                if let Some(tray_rect) =
                                    native_interop::get_window_rect_safe(tray_hwnd)
                                {
                                    tray_left = tray_rect.left;
                                }
                            }
                            let widget_width = total_widget_width_for_state(s);
                            let max_offset = (tray_left - taskbar_rect.left - widget_width).max(0);
                            if new_offset > max_offset {
                                new_offset = max_offset;
                            }

                            s.tray_offset = new_offset;

                            let work_area = native_interop::get_monitor_work_area(taskbar_hwnd)
                                .unwrap_or(RECT {
                                    left: taskbar_rect.left,
                                    top: i32::MIN / 2,
                                    right: taskbar_rect.right,
                                    bottom: taskbar_rect.top,
                                });
                            let widget_height = sc(WIDGET_HEIGHT);
                            let y = compute_popup_y(work_area.top, work_area.bottom, widget_height);
                            let desired_x = tray_left - widget_width - new_offset;
                            let x = clamp_popup_x(
                                desired_x,
                                work_area.left,
                                work_area.right,
                                widget_width,
                            );
                            Some((hwnd_val, x, y, widget_width, widget_height))
                        } else {
                            s.tray_offset = new_offset;
                            None
                        }
                    } else {
                        s.tray_offset = new_offset;
                        None
                    }
                };

                if let Some((hwnd_val, x, y, widget_width, widget_height)) = move_target {
                    native_interop::move_window(hwnd_val, x, y, widget_width, widget_height);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let drag_result = {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    if s.dragging {
                        s.dragging = false;
                        Some((s.taskbar_index, s.drag_start_client_x))
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some((current_taskbar_index, drag_start_client_x)) = drag_result {
                let _ = ReleaseCapture();
                if let Some((target_index, target_taskbar)) = taskbar_at_point(pt) {
                    if target_index != current_taskbar_index {
                        let new_offset = offset_for_drop_point(
                            target_taskbar.hwnd,
                            target_taskbar.rect,
                            pt,
                            drag_start_client_x,
                        );
                        {
                            let mut state = lock_state();
                            if let Some(s) = state.as_mut() {
                                s.tray_offset = new_offset;
                            }
                        }
                        if select_taskbar_anchor(target_index) {
                            position_at_taskbar();
                            render_layered();
                        }
                    }
                }
                save_state_settings();
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            show_context_menu(hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = wparam.0 as u16;
            match id {
                1 => {
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.session_state = CellState::Loading;
                            s.weekly_state = CellState::Loading;
                            s.codex_session_state = CellState::Loading;
                            s.codex_weekly_state = CellState::Loading;
                            s.antigravity_session_state = CellState::Loading;
                            s.antigravity_weekly_state = CellState::Loading;
                            refresh_usage_texts(s);
                            s.force_notify_auth_error = true;
                        }
                    }
                    render_layered();
                    let sh = SendHwnd::from_hwnd(hwnd);
                    std::thread::spawn(move || {
                        do_poll(sh);
                    });
                }
                #[cfg(feature = "self-update")]
                IDM_VERSION_ACTION => {
                    let (install_channel, release) = {
                        let state = lock_state();
                        match state.as_ref() {
                            Some(s) => (
                                s.install_channel,
                                match &s.update_status {
                                    UpdateStatus::Available(release) => Some(release.clone()),
                                    _ => None,
                                },
                            ),
                            None => (InstallChannel::Portable, None),
                        }
                    };

                    match install_channel {
                        InstallChannel::Winget => {
                            if release.is_some() {
                                begin_winget_update(hwnd);
                            } else {
                                begin_update_check(hwnd, true);
                            }
                        }
                        InstallChannel::Portable => {
                            if let Some(release) = release {
                                begin_update_apply(hwnd, release);
                            } else {
                                begin_update_check(hwnd, true);
                            }
                        }
                    }
                }
                2 => {
                    let hook = {
                        let state = lock_state();
                        state.as_ref().and_then(|s| s.win_event_hook)
                    };
                    if let Some(h) = hook {
                        native_interop::unhook_win_event(h);
                    }
                    PostQuitMessage(0);
                }
                IDM_RESET_POSITION => {
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.tray_offset = 0;
                        }
                    }
                    save_state_settings();
                    position_at_taskbar();
                }
                IDM_START_WITH_WINDOWS => {
                    set_startup_enabled(!is_startup_enabled());
                }
                IDM_ALWAYS_ON_TOP => {
                    let always_on_top = {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.always_on_top = !s.always_on_top;
                            Some(s.always_on_top)
                        } else {
                            None
                        }
                    };
                    if let Some(always_on_top) = always_on_top {
                        save_state_settings();
                        apply_always_on_top(hwnd, always_on_top);
                    }
                }
                IDM_DISPLAY_BASIS_USED | IDM_DISPLAY_BASIS_REMAINING => {
                    let new_basis = if id == IDM_DISPLAY_BASIS_USED {
                        DisplayBasis::UsedPercentage
                    } else {
                        DisplayBasis::RemainingAllowance
                    };
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.display_basis = new_basis;
                            refresh_usage_texts(s);
                        }
                    }
                    save_state_settings();
                    render_layered();
                    sync_tray_icons(hwnd);
                }
                IDM_DISPLAY_DENSITY_COMPACT
                | IDM_DISPLAY_DENSITY_STANDARD
                | IDM_DISPLAY_DENSITY_DETAILED => {
                    // Not yet consumed by any drawing code, so unlike the
                    // display-basis handler above there is nothing to
                    // recompute or redraw here — just persist the choice.
                    if let Some(new_density) = display_density_for_menu_id(id) {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.display_density = new_density;
                        }
                    }
                    save_state_settings();
                }
                IDM_SHORT_WINDOW_VISIBILITY_ALWAYS
                | IDM_SHORT_WINDOW_VISIBILITY_WARNING_ONLY
                | IDM_SHORT_WINDOW_VISIBILITY_HIDDEN => {
                    if let Some(new_visibility) = short_window_visibility_for_menu_id(id) {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.short_window_visibility = new_visibility;
                        }
                    }
                    save_state_settings();
                }
                IDM_SHORT_WINDOW_ALERT_SENSITIVITY_SENSITIVE
                | IDM_SHORT_WINDOW_ALERT_SENSITIVITY_STANDARD
                | IDM_SHORT_WINDOW_ALERT_SENSITIVITY_RELAXED => {
                    if let Some(new_sensitivity) = short_window_alert_sensitivity_for_menu_id(id) {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.short_window_alert_sensitivity = new_sensitivity;
                        }
                    }
                    save_state_settings();
                }
                IDM_FREQ_1MIN | IDM_FREQ_5MIN | IDM_FREQ_15MIN | IDM_FREQ_1HOUR => {
                    let new_interval = match id {
                        IDM_FREQ_1MIN => POLL_1_MIN,
                        IDM_FREQ_5MIN => POLL_5_MIN,
                        IDM_FREQ_15MIN => POLL_15_MIN,
                        IDM_FREQ_1HOUR => POLL_1_HOUR,
                        _ => POLL_15_MIN,
                    };
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.poll_interval_ms = new_interval;
                        }
                    }
                    save_state_settings();
                    // Reset the poll timer with the new interval
                    SetTimer(hwnd, TIMER_POLL, new_interval, None);
                }
                IDM_MODEL_CLAUDE_CODE | IDM_MODEL_CODEX => {
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            match id {
                                IDM_MODEL_CLAUDE_CODE => {
                                    if s.show_codex || s.show_antigravity || !s.show_claude_code {
                                        s.show_claude_code = !s.show_claude_code;
                                    }
                                }
                                IDM_MODEL_CODEX => {
                                    if s.show_claude_code || s.show_antigravity || !s.show_codex {
                                        s.show_codex = !s.show_codex;
                                    }
                                }
                                _ => {}
                            }
                            s.session_state = CellState::Loading;
                            s.weekly_state = CellState::Loading;
                            s.codex_session_state = CellState::Loading;
                            s.codex_weekly_state = CellState::Loading;
                            s.antigravity_session_state = CellState::Loading;
                            s.antigravity_weekly_state = CellState::Loading;
                            refresh_usage_texts(s);
                        }
                    }
                    save_state_settings();
                    position_at_taskbar();
                    render_layered();
                    sync_tray_icons(hwnd);
                    let sh = SendHwnd::from_hwnd(hwnd);
                    std::thread::spawn(move || {
                        do_poll(sh);
                    });
                }
                #[cfg(feature = "antigravity")]
                IDM_MODEL_ANTIGRAVITY => {
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            if s.show_claude_code || s.show_codex || !s.show_antigravity {
                                s.show_antigravity = !s.show_antigravity;
                            }
                            s.session_state = CellState::Loading;
                            s.weekly_state = CellState::Loading;
                            s.codex_session_state = CellState::Loading;
                            s.codex_weekly_state = CellState::Loading;
                            s.antigravity_session_state = CellState::Loading;
                            s.antigravity_weekly_state = CellState::Loading;
                            refresh_usage_texts(s);
                        }
                    }
                    save_state_settings();
                    position_at_taskbar();
                    render_layered();
                    sync_tray_icons(hwnd);
                    let sh = SendHwnd::from_hwnd(hwnd);
                    std::thread::spawn(move || {
                        do_poll(sh);
                    });
                }
                IDM_LANG_SYSTEM
                | IDM_LANG_ENGLISH
                | IDM_LANG_DUTCH
                | IDM_LANG_SPANISH
                | IDM_LANG_FRENCH
                | IDM_LANG_GERMAN
                | IDM_LANG_JAPANESE
                | IDM_LANG_KOREAN
                | IDM_LANG_TRADITIONAL_CHINESE
                | IDM_LANG_SIMPLIFIED_CHINESE
                | IDM_LANG_RUSSIAN
                | IDM_LANG_PORTUGUESE_BRAZIL => {
                    let language_override = match id {
                        IDM_LANG_SYSTEM => None,
                        IDM_LANG_ENGLISH => Some(LanguageId::English),
                        IDM_LANG_DUTCH => Some(LanguageId::Dutch),
                        IDM_LANG_SPANISH => Some(LanguageId::Spanish),
                        IDM_LANG_FRENCH => Some(LanguageId::French),
                        IDM_LANG_GERMAN => Some(LanguageId::German),
                        IDM_LANG_JAPANESE => Some(LanguageId::Japanese),
                        IDM_LANG_KOREAN => Some(LanguageId::Korean),
                        IDM_LANG_TRADITIONAL_CHINESE => Some(LanguageId::TraditionalChinese),
                        IDM_LANG_SIMPLIFIED_CHINESE => Some(LanguageId::SimplifiedChinese),
                        IDM_LANG_RUSSIAN => Some(LanguageId::Russian),
                        IDM_LANG_PORTUGUESE_BRAZIL => Some(LanguageId::PortugueseBrazil),
                        _ => None,
                    };
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            apply_language_to_state(s, language_override);
                        }
                    }
                    save_state_settings();
                    render_layered();
                }
                id if id == tray_icon::IDM_TOGGLE_WIDGET => {
                    toggle_widget_visibility(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        _ if msg == WM_APP_TRAY => {
            match tray_icon::handle_message(lparam) {
                tray_icon::TrayAction::ToggleWidget => {
                    toggle_widget_visibility(hwnd);
                }
                tray_icon::TrayAction::ShowContextMenu => {
                    show_context_menu(hwnd);
                }
                tray_icon::TrayAction::None => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let hook = {
                let state = lock_state();
                state.as_ref().and_then(|s| s.win_event_hook)
            };
            if let Some(h) = hook {
                native_interop::unhook_win_event(h);
            }
            tray_icon::remove_all(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn show_context_menu(hwnd: HWND) {
    unsafe {
        let (
            current_interval,
            strings,
            language,
            language_override,
            install_channel,
            update_status,
            widget_visible,
            always_on_top,
            show_claude_code,
            show_codex,
            show_antigravity,
            display_basis,
            display_density,
            short_window_visibility,
            short_window_alert_sensitivity,
        ) = {
            let state = lock_state();
            match state.as_ref() {
                Some(s) => (
                    s.poll_interval_ms,
                    s.language.strings(),
                    s.language,
                    s.language_override,
                    s.install_channel,
                    s.update_status.clone(),
                    s.widget_visible,
                    s.always_on_top,
                    s.show_claude_code,
                    s.show_codex,
                    s.show_antigravity,
                    s.display_basis,
                    s.display_density,
                    s.short_window_visibility,
                    s.short_window_alert_sensitivity,
                ),
                None => (
                    POLL_15_MIN,
                    LanguageId::English.strings(),
                    LanguageId::English,
                    None,
                    InstallChannel::Portable,
                    UpdateStatus::Idle,
                    true,
                    false,
                    true,
                    false,
                    false,
                    DisplayBasis::default(),
                    DisplayDensity::default(),
                    ShortWindowVisibility::default(),
                    ShortWindowAlertSensitivity::default(),
                ),
            }
        };

        let menu = CreatePopupMenu().unwrap();

        let refresh_str = native_interop::wide_str(strings.refresh);
        let _ = AppendMenuW(
            menu,
            MENU_ITEM_FLAGS(0),
            1,
            PCWSTR::from_raw(refresh_str.as_ptr()),
        );

        // Update Frequency submenu
        let freq_menu = CreatePopupMenu().unwrap();
        let freq_items: [(u16, u32, &str); 4] = [
            (IDM_FREQ_1MIN, POLL_1_MIN, strings.one_minute),
            (IDM_FREQ_5MIN, POLL_5_MIN, strings.five_minutes),
            (IDM_FREQ_15MIN, POLL_15_MIN, strings.fifteen_minutes),
            (IDM_FREQ_1HOUR, POLL_1_HOUR, strings.one_hour),
        ];
        for (id, interval, label) in freq_items {
            let label_str = native_interop::wide_str(label);
            let flags = if interval == current_interval {
                MF_CHECKED
            } else {
                MENU_ITEM_FLAGS(0)
            };
            let _ = AppendMenuW(
                freq_menu,
                flags,
                id as usize,
                PCWSTR::from_raw(label_str.as_ptr()),
            );
        }

        let freq_label = native_interop::wide_str(strings.update_frequency);
        let _ = AppendMenuW(
            menu,
            MF_POPUP,
            freq_menu.0 as usize,
            PCWSTR::from_raw(freq_label.as_ptr()),
        );

        // Models submenu
        let models_menu = CreatePopupMenu().unwrap();
        let claude_model = native_interop::wide_str(strings.claude_code_model);
        let claude_flags = if show_claude_code {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            models_menu,
            claude_flags,
            IDM_MODEL_CLAUDE_CODE as usize,
            PCWSTR::from_raw(claude_model.as_ptr()),
        );

        let codex_model = native_interop::wide_str(strings.codex_model);
        let codex_flags = if show_codex {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            models_menu,
            codex_flags,
            IDM_MODEL_CODEX as usize,
            PCWSTR::from_raw(codex_model.as_ptr()),
        );

        #[cfg(feature = "antigravity")]
        {
            let antigravity_model = native_interop::wide_str(strings.antigravity_model);
            let antigravity_flags = if show_antigravity {
                MF_CHECKED
            } else {
                MENU_ITEM_FLAGS(0)
            };
            let _ = AppendMenuW(
                models_menu,
                antigravity_flags,
                IDM_MODEL_ANTIGRAVITY as usize,
                PCWSTR::from_raw(antigravity_model.as_ptr()),
            );
        }

        let models_label = native_interop::wide_str(strings.models);
        let _ = AppendMenuW(
            menu,
            MF_POPUP,
            models_menu.0 as usize,
            PCWSTR::from_raw(models_label.as_ptr()),
        );

        // Settings submenu
        let settings_menu = CreatePopupMenu().unwrap();

        let startup_str = native_interop::wide_str(strings.start_with_windows);
        let startup_flags = if is_startup_enabled() {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            settings_menu,
            startup_flags,
            IDM_START_WITH_WINDOWS as usize,
            PCWSTR::from_raw(startup_str.as_ptr()),
        );

        let always_on_top_str = native_interop::wide_str(strings.always_on_top);
        let always_on_top_flags = if always_on_top {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            settings_menu,
            always_on_top_flags,
            IDM_ALWAYS_ON_TOP as usize,
            PCWSTR::from_raw(always_on_top_str.as_ptr()),
        );

        let reset_pos_str = native_interop::wide_str(strings.reset_position);
        let _ = AppendMenuW(
            settings_menu,
            MENU_ITEM_FLAGS(0),
            IDM_RESET_POSITION as usize,
            PCWSTR::from_raw(reset_pos_str.as_ptr()),
        );

        let language_menu = CreatePopupMenu().unwrap();
        let system_label = native_interop::wide_str(strings.system_default);
        let system_flags = if language_override.is_none() {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            language_menu,
            system_flags,
            IDM_LANG_SYSTEM as usize,
            PCWSTR::from_raw(system_label.as_ptr()),
        );

        for language in LanguageId::ALL {
            let id = match language {
                LanguageId::English => IDM_LANG_ENGLISH,
                LanguageId::Dutch => IDM_LANG_DUTCH,
                LanguageId::Spanish => IDM_LANG_SPANISH,
                LanguageId::French => IDM_LANG_FRENCH,
                LanguageId::German => IDM_LANG_GERMAN,
                LanguageId::Japanese => IDM_LANG_JAPANESE,
                LanguageId::Korean => IDM_LANG_KOREAN,
                LanguageId::TraditionalChinese => IDM_LANG_TRADITIONAL_CHINESE,
                LanguageId::SimplifiedChinese => IDM_LANG_SIMPLIFIED_CHINESE,
                LanguageId::Russian => IDM_LANG_RUSSIAN,
                LanguageId::PortugueseBrazil => IDM_LANG_PORTUGUESE_BRAZIL,
            };
            let label_str = native_interop::wide_str(language.native_name());
            let flags = if language_override == Some(language) {
                MF_CHECKED
            } else {
                MENU_ITEM_FLAGS(0)
            };
            let _ = AppendMenuW(
                language_menu,
                flags,
                id as usize,
                PCWSTR::from_raw(label_str.as_ptr()),
            );
        }

        let language_label = native_interop::wide_str(strings.language);
        let _ = AppendMenuW(
            settings_menu,
            MF_POPUP,
            language_menu.0 as usize,
            PCWSTR::from_raw(language_label.as_ptr()),
        );

        // Display basis submenu: mutually exclusive, radio-style, same
        // pattern as the language submenu above.
        let display_basis_menu = CreatePopupMenu().unwrap();

        let used_percentage_str = native_interop::wide_str(strings.used_percentage);
        let used_percentage_flags = if display_basis == DisplayBasis::UsedPercentage {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            display_basis_menu,
            used_percentage_flags,
            IDM_DISPLAY_BASIS_USED as usize,
            PCWSTR::from_raw(used_percentage_str.as_ptr()),
        );

        let remaining_allowance_str = native_interop::wide_str(strings.remaining_allowance);
        let remaining_allowance_flags = if display_basis == DisplayBasis::RemainingAllowance {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            display_basis_menu,
            remaining_allowance_flags,
            IDM_DISPLAY_BASIS_REMAINING as usize,
            PCWSTR::from_raw(remaining_allowance_str.as_ptr()),
        );

        let display_basis_label = native_interop::wide_str(strings.usage_display_basis);
        let _ = AppendMenuW(
            settings_menu,
            MF_POPUP,
            display_basis_menu.0 as usize,
            PCWSTR::from_raw(display_basis_label.as_ptr()),
        );

        // Display density submenu: mutually exclusive, radio-style, same
        // pattern as the display-basis submenu above. Not yet connected to
        // any drawing code — see AUM-PACE-GUIDANCE-01's later units.
        let display_density_menu = CreatePopupMenu().unwrap();
        let display_density_items: [(u16, DisplayDensity, &str); 3] = [
            (
                IDM_DISPLAY_DENSITY_COMPACT,
                DisplayDensity::Compact,
                strings.display_density_compact,
            ),
            (
                IDM_DISPLAY_DENSITY_STANDARD,
                DisplayDensity::Standard,
                strings.standard_level,
            ),
            (
                IDM_DISPLAY_DENSITY_DETAILED,
                DisplayDensity::Detailed,
                strings.display_density_detailed,
            ),
        ];
        for (id, value, label) in display_density_items {
            let label_str = native_interop::wide_str(label);
            let flags = if value == display_density {
                MF_CHECKED
            } else {
                MENU_ITEM_FLAGS(0)
            };
            let _ = AppendMenuW(
                display_density_menu,
                flags,
                id as usize,
                PCWSTR::from_raw(label_str.as_ptr()),
            );
        }
        let display_density_label = native_interop::wide_str(strings.display_density);
        let _ = AppendMenuW(
            settings_menu,
            MF_POPUP,
            display_density_menu.0 as usize,
            PCWSTR::from_raw(display_density_label.as_ptr()),
        );

        // Short-window (5h) visibility submenu.
        let short_window_visibility_menu = CreatePopupMenu().unwrap();
        let short_window_visibility_items: [(u16, ShortWindowVisibility, &str); 3] = [
            (
                IDM_SHORT_WINDOW_VISIBILITY_ALWAYS,
                ShortWindowVisibility::Always,
                strings.short_window_visibility_always,
            ),
            (
                IDM_SHORT_WINDOW_VISIBILITY_WARNING_ONLY,
                ShortWindowVisibility::WarningOnly,
                strings.short_window_visibility_warning_only,
            ),
            (
                IDM_SHORT_WINDOW_VISIBILITY_HIDDEN,
                ShortWindowVisibility::Hidden,
                strings.short_window_visibility_hidden,
            ),
        ];
        for (id, value, label) in short_window_visibility_items {
            let label_str = native_interop::wide_str(label);
            let flags = if value == short_window_visibility {
                MF_CHECKED
            } else {
                MENU_ITEM_FLAGS(0)
            };
            let _ = AppendMenuW(
                short_window_visibility_menu,
                flags,
                id as usize,
                PCWSTR::from_raw(label_str.as_ptr()),
            );
        }
        let short_window_visibility_label =
            native_interop::wide_str(strings.short_window_visibility);
        let _ = AppendMenuW(
            settings_menu,
            MF_POPUP,
            short_window_visibility_menu.0 as usize,
            PCWSTR::from_raw(short_window_visibility_label.as_ptr()),
        );

        // Short-window (5h) alert sensitivity submenu.
        let short_window_alert_sensitivity_menu = CreatePopupMenu().unwrap();
        let short_window_alert_sensitivity_items: [(u16, ShortWindowAlertSensitivity, &str); 3] = [
            (
                IDM_SHORT_WINDOW_ALERT_SENSITIVITY_SENSITIVE,
                ShortWindowAlertSensitivity::Sensitive,
                strings.short_window_alert_sensitivity_sensitive,
            ),
            (
                IDM_SHORT_WINDOW_ALERT_SENSITIVITY_STANDARD,
                ShortWindowAlertSensitivity::Standard,
                strings.standard_level,
            ),
            (
                IDM_SHORT_WINDOW_ALERT_SENSITIVITY_RELAXED,
                ShortWindowAlertSensitivity::Relaxed,
                strings.short_window_alert_sensitivity_relaxed,
            ),
        ];
        for (id, value, label) in short_window_alert_sensitivity_items {
            let label_str = native_interop::wide_str(label);
            let flags = if value == short_window_alert_sensitivity {
                MF_CHECKED
            } else {
                MENU_ITEM_FLAGS(0)
            };
            let _ = AppendMenuW(
                short_window_alert_sensitivity_menu,
                flags,
                id as usize,
                PCWSTR::from_raw(label_str.as_ptr()),
            );
        }
        let short_window_alert_sensitivity_label =
            native_interop::wide_str(strings.short_window_alert_sensitivity);
        let _ = AppendMenuW(
            settings_menu,
            MF_POPUP,
            short_window_alert_sensitivity_menu.0 as usize,
            PCWSTR::from_raw(short_window_alert_sensitivity_label.as_ptr()),
        );

        #[cfg(feature = "self-update")]
        {
            let _ = AppendMenuW(settings_menu, MF_SEPARATOR, 0, PCWSTR::null());

            let version_label =
                version_action_label(strings, language, install_channel, &update_status);
            let version_str = native_interop::wide_str(&version_label);
            let version_flags = if matches!(
                update_status,
                UpdateStatus::Checking | UpdateStatus::Applying
            ) {
                MF_GRAYED
            } else {
                MENU_ITEM_FLAGS(0)
            };
            let _ = AppendMenuW(
                settings_menu,
                version_flags,
                IDM_VERSION_ACTION as usize,
                PCWSTR::from_raw(version_str.as_ptr()),
            );
        }

        let settings_label = native_interop::wide_str(strings.settings);
        let _ = AppendMenuW(
            menu,
            MF_POPUP,
            settings_menu.0 as usize,
            PCWSTR::from_raw(settings_label.as_ptr()),
        );

        let widget_label = native_interop::wide_str(strings.show_widget);
        let widget_flags = if widget_visible {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            menu,
            widget_flags,
            tray_icon::IDM_TOGGLE_WIDGET as usize,
            PCWSTR::from_raw(widget_label.as_ptr()),
        );

        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

        let exit_str = native_interop::wide_str(strings.exit);
        let _ = AppendMenuW(
            menu,
            MENU_ITEM_FLAGS(0),
            2,
            PCWSTR::from_raw(exit_str.as_ptr()),
        );

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
        let _ = DestroyMenu(menu);
    }
}

/// Paint for non-embedded fallback (normal WM_PAINT path)
fn paint(hdc: HDC, hwnd: HWND) {
    let (
        is_dark,
        strings,
        display_basis,
        session_pct,
        session_text,
        weekly_pct,
        weekly_text,
        codex_session_pct,
        codex_session_text,
        codex_weekly_pct,
        codex_weekly_text,
        antigravity_session_pct,
        antigravity_session_text,
        antigravity_weekly_pct,
        antigravity_weekly_text,
        show_claude_code,
        show_codex,
        show_antigravity,
    ) = {
        let state = lock_state();
        match state.as_ref() {
            Some(s) => (
                s.is_dark,
                s.language.strings(),
                s.display_basis,
                s.session_percent,
                s.session_text.clone(),
                s.weekly_percent,
                s.weekly_text.clone(),
                s.codex_session_percent,
                s.codex_session_text.clone(),
                s.codex_weekly_percent,
                s.codex_weekly_text.clone(),
                s.antigravity_session_percent,
                s.antigravity_session_text.clone(),
                s.antigravity_weekly_percent,
                s.antigravity_weekly_text.clone(),
                s.show_claude_code,
                s.show_codex,
                s.show_antigravity,
            ),
            None => return,
        }
    };

    let accent = claude_accent_color();
    let codex_accent = codex_accent_color(is_dark);
    let antigravity_accent = antigravity_accent_color();
    let track = if is_dark {
        Color::from_hex("#444444")
    } else {
        Color::from_hex("#AAAAAA")
    };
    let text_color = if is_dark {
        Color::from_hex("#888888")
    } else {
        Color::from_hex("#404040")
    };
    let bg_color = if is_dark {
        Color::from_hex("#1C1C1C")
    } else {
        Color::from_hex("#F3F3F3")
    };

    unsafe {
        let mut client_rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut client_rect);
        let width = client_rect.right - client_rect.left;
        let height = client_rect.bottom - client_rect.top;

        if width <= 0 || height <= 0 {
            return;
        }

        let mem_dc = CreateCompatibleDC(hdc);
        let mem_bmp = CreateCompatibleBitmap(hdc, width, height);
        let old_bmp = SelectObject(mem_dc, mem_bmp);

        paint_content(
            mem_dc,
            width,
            height,
            is_dark,
            &bg_color,
            &text_color,
            &accent,
            &track,
            strings,
            display_basis,
            session_pct,
            &session_text,
            weekly_pct,
            &weekly_text,
            codex_session_pct,
            &codex_session_text,
            codex_weekly_pct,
            &codex_weekly_text,
            antigravity_session_pct,
            &antigravity_session_text,
            antigravity_weekly_pct,
            &antigravity_weekly_text,
            show_claude_code,
            show_codex,
            show_antigravity,
            &codex_accent,
            &antigravity_accent,
        );

        let _ = BitBlt(hdc, 0, 0, width, height, mem_dc, 0, 0, SRCCOPY);

        SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(mem_bmp);
        let _ = DeleteDC(mem_dc);
    }
}

/// Draws the provider-name header row: one text label per active provider
/// column, aligned with the same `model_x` positions `draw_row` uses for its
/// bars, so a provider is identifiable by its full localized name rather
/// than by accent color or an invented abbreviation.
fn draw_provider_header_row(
    hdc: HDC,
    x: i32,
    y: i32,
    text_color: &Color,
    strings: Strings,
    show_claude_code: bool,
    show_codex: bool,
    show_antigravity: bool,
) {
    let active_models = active_model_count(show_claude_code, show_codex, show_antigravity);
    let segment_count = row_bar_segment_count(active_models);
    let column_width = model_usage_width(segment_count);

    unsafe {
        let _ = SetTextColor(hdc, COLORREF(text_color.to_colorref()));
        let mut model_x = x + sc(LABEL_WIDTH) + sc(LABEL_RIGHT_MARGIN);
        if show_claude_code {
            draw_header_label(hdc, model_x, y, column_width, strings.claude_code_model);
            model_x += column_width + sc(MODEL_RIGHT_MARGIN);
        }
        if show_codex {
            draw_header_label(hdc, model_x, y, column_width, strings.codex_model);
            model_x += column_width + sc(MODEL_RIGHT_MARGIN);
        }
        if show_antigravity {
            draw_header_label(hdc, model_x, y, column_width, strings.antigravity_model);
        }
    }
}

fn draw_header_label(hdc: HDC, x: i32, y: i32, width: i32, label: &str) {
    unsafe {
        let mut label_wide: Vec<u16> = label.encode_utf16().collect();
        let mut label_rect = RECT {
            left: x,
            top: y,
            right: x + width,
            bottom: y + sc(HEADER_ROW_H),
        };
        let _ = DrawTextW(
            hdc,
            &mut label_wide,
            &mut label_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );
    }
}

/// Draws the display-basis label ("Used %" / "Remaining Allowance", already
/// resolved by the caller) spanning the full row width. It sits above the
/// provider-name header row and isn't column-constrained, since it applies
/// to every column at once rather than identifying a single provider.
fn draw_basis_label_row(
    hdc: HDC,
    x: i32,
    y: i32,
    right_edge: i32,
    text_color: &Color,
    label: &str,
) {
    unsafe {
        let _ = SetTextColor(hdc, COLORREF(text_color.to_colorref()));
        let mut label_wide: Vec<u16> = label.encode_utf16().collect();
        let mut label_rect = RECT {
            left: x,
            top: y,
            right: right_edge,
            bottom: y + sc(HEADER_ROW_H),
        };
        let _ = DrawTextW(
            hdc,
            &mut label_wide,
            &mut label_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );
    }
}

fn draw_row(
    hdc: HDC,
    x: i32,
    y: i32,
    is_dark: bool,
    text_color: &Color,
    label: &str,
    claude_percent: Option<f64>,
    claude_text: &str,
    codex_percent: Option<f64>,
    codex_text: &str,
    antigravity_percent: Option<f64>,
    antigravity_text: &str,
    show_claude_code: bool,
    show_codex: bool,
    show_antigravity: bool,
    claude_accent: &Color,
    codex_accent: &Color,
    antigravity_accent: &Color,
    track: &Color,
) {
    let seg_h = sc(SEGMENT_H);
    let active_models = active_model_count(show_claude_code, show_codex, show_antigravity);
    let segment_count = row_bar_segment_count(active_models);
    let use_model_text_colors = active_models > 1;
    let claude_value_color = if use_model_text_colors {
        claude_usage_text_color(is_dark)
    } else {
        *text_color
    };
    let codex_value_color = if use_model_text_colors {
        codex_usage_text_color(is_dark)
    } else {
        *text_color
    };
    let antigravity_value_color = if use_model_text_colors {
        antigravity_usage_text_color(is_dark)
    } else {
        *text_color
    };

    unsafe {
        let _ = SetTextColor(hdc, COLORREF(text_color.to_colorref()));
        let mut label_wide: Vec<u16> = label.encode_utf16().collect();
        let mut label_rect = RECT {
            left: x,
            top: y,
            right: x + sc(LABEL_WIDTH),
            bottom: y + seg_h,
        };
        let _ = DrawTextW(
            hdc,
            &mut label_wide,
            &mut label_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );

        let mut model_x = x + sc(LABEL_WIDTH) + sc(LABEL_RIGHT_MARGIN);
        if show_claude_code {
            draw_usage_bar(
                hdc,
                model_x,
                y,
                segment_count,
                claude_percent,
                claude_text,
                claude_accent,
                track,
                &claude_value_color,
            );
            model_x += model_usage_width(segment_count) + sc(MODEL_RIGHT_MARGIN);
        }
        if show_codex {
            draw_usage_bar(
                hdc,
                model_x,
                y,
                segment_count,
                codex_percent,
                codex_text,
                codex_accent,
                track,
                &codex_value_color,
            );
            model_x += model_usage_width(segment_count) + sc(MODEL_RIGHT_MARGIN);
        }
        if show_antigravity {
            draw_usage_bar(
                hdc,
                model_x,
                y,
                segment_count,
                antigravity_percent,
                antigravity_text,
                antigravity_accent,
                track,
                &antigravity_value_color,
            );
        }
    }
}

fn model_usage_width(segment_count: i32) -> i32 {
    (sc(SEGMENT_W) + sc(SEGMENT_GAP)) * segment_count - sc(SEGMENT_GAP)
        + sc(BAR_RIGHT_MARGIN)
        + sc(TEXT_WIDTH)
}

/// `percent` is `None` for any cell without a real current value (loading,
/// error, unconfigured, not-available). In that case no segment — neither
/// filled nor empty track — is drawn, so the bar area is left blank rather
/// than rendering what would look like an ordinary 0% bar; the status word
/// in `text` is the only thing shown for that cell. This is the one place
/// `CellDisplay::bar_percent` actually reaches the screen.
fn draw_usage_bar(
    hdc: HDC,
    bar_x: i32,
    y: i32,
    segment_count: i32,
    percent: Option<f64>,
    text: &str,
    accent: &Color,
    track: &Color,
    text_color: &Color,
) {
    let seg_w = sc(SEGMENT_W);
    let seg_h = sc(SEGMENT_H);
    let seg_gap = sc(SEGMENT_GAP);
    let corner_r = sc(CORNER_RADIUS);

    unsafe {
        if let Some(percent) = percent {
            let percent_clamped = percent.clamp(0.0, 100.0);
            let segment_percent = 100.0 / segment_count as f64;

            for i in 0..segment_count {
                let seg_x = bar_x + i * (seg_w + seg_gap);
                let seg_start = (i as f64) * segment_percent;
                let seg_end = seg_start + segment_percent;

                let seg_rect = RECT {
                    left: seg_x,
                    top: y,
                    right: seg_x + seg_w,
                    bottom: y + seg_h,
                };

                if percent_clamped >= seg_end {
                    draw_rounded_rect(hdc, &seg_rect, accent, corner_r);
                } else if percent_clamped <= seg_start {
                    draw_rounded_rect(hdc, &seg_rect, track, corner_r);
                } else {
                    draw_rounded_rect(hdc, &seg_rect, track, corner_r);
                    let fraction = (percent_clamped - seg_start) / segment_percent;
                    let fill_width = (seg_w as f64 * fraction) as i32;
                    if fill_width > 0 {
                        let fill_rect = RECT {
                            left: seg_x,
                            top: y,
                            right: seg_x + fill_width,
                            bottom: y + seg_h,
                        };
                        let rgn = CreateRoundRectRgn(
                            seg_rect.left,
                            seg_rect.top,
                            seg_rect.right + 1,
                            seg_rect.bottom + 1,
                            corner_r * 2,
                            corner_r * 2,
                        );
                        let _ = SelectClipRgn(hdc, rgn);
                        let brush = CreateSolidBrush(COLORREF(accent.to_colorref()));
                        FillRect(hdc, &fill_rect, brush);
                        let _ = DeleteObject(brush);
                        let _ = SelectClipRgn(hdc, HRGN::default());
                        let _ = DeleteObject(rgn);
                    }
                }
            }
        }

        let text_x = bar_x + segment_count * (seg_w + seg_gap) - seg_gap + sc(BAR_RIGHT_MARGIN);
        let mut text_wide: Vec<u16> = text.encode_utf16().collect();
        let mut text_rect = RECT {
            left: text_x,
            top: y,
            right: text_x + sc(TEXT_WIDTH),
            bottom: y + seg_h,
        };
        let _ = SetTextColor(hdc, COLORREF(text_color.to_colorref()));
        let _ = DrawTextW(
            hdc,
            &mut text_wide,
            &mut text_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );
    }
}

fn draw_rounded_rect(hdc: HDC, rect: &RECT, color: &Color, radius: i32) {
    unsafe {
        let brush = CreateSolidBrush(COLORREF(color.to_colorref()));
        let rgn = CreateRoundRectRgn(
            rect.left,
            rect.top,
            rect.right + 1,
            rect.bottom + 1,
            radius * 2,
            radius * 2,
        );
        let _ = FillRgn(hdc, rgn, brush);
        let _ = DeleteObject(rgn);
        let _ = DeleteObject(brush);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_sits_above_a_normal_bottom_taskbar() {
        // work area 0..1040, popup height 46
        assert_eq!(compute_popup_y(0, 1040, 46), 994);
    }

    #[test]
    fn popup_sits_above_a_small_bottom_taskbar() {
        // work area 0..1050 (taskbar shorter than the popup itself)
        assert_eq!(compute_popup_y(0, 1050, 46), 1004);
    }

    #[test]
    fn popup_y_is_correct_on_a_negative_coordinate_monitor() {
        // e.g. a secondary monitor positioned above/left of the primary
        assert_eq!(compute_popup_y(-1040, -40, 46), -86);
    }

    #[test]
    fn popup_taller_than_work_area_clamps_to_work_area_top() {
        // Physically cannot fit above the taskbar; work_area_top wins as a
        // last resort, even though the popup then extends past
        // work_area_bottom (unavoidable given the height constraint).
        let y = compute_popup_y(1030, 1040, 46);
        assert_eq!(y, 1030);
    }

    #[test]
    fn popup_bottom_never_exceeds_work_area_bottom_in_normal_cases() {
        for (top, bottom, height) in [(0, 1040, 46), (0, 1050, 46), (-1040, -40, 46)] {
            let y = compute_popup_y(top, bottom, height);
            assert!(y + height <= bottom);
        }
    }

    #[test]
    fn popup_x_within_work_area_is_unchanged() {
        assert_eq!(clamp_popup_x(500, 0, 1920, 300), 500);
    }

    #[test]
    fn popup_x_clamps_to_left_edge_of_work_area() {
        assert_eq!(clamp_popup_x(-50, 0, 1920, 300), 0);
    }

    #[test]
    fn popup_x_clamps_to_right_edge_of_work_area() {
        assert_eq!(clamp_popup_x(1800, 0, 1920, 300), 1620);
    }

    #[test]
    fn popup_wider_than_work_area_clamps_to_left_without_panicking() {
        // popup_width (2000) > work area width (1920): must not panic on
        // clamp(min, max) with min > max, and must fall back to the left edge.
        assert_eq!(clamp_popup_x(500, 0, 1920, 2000), 0);
    }

    #[test]
    fn drag_handle_hit_area_includes_x_zero_at_96_dpi() {
        assert!(is_drag_handle_point(0, 40));
    }

    #[test]
    fn drag_handle_hit_area_includes_x_nine_at_96_dpi() {
        assert!(is_drag_handle_point(9, 40));
    }

    #[test]
    fn drag_handle_hit_area_excludes_x_ten_at_96_dpi() {
        assert!(!is_drag_handle_point(10, 40));
    }

    #[test]
    fn drag_handle_hit_area_excludes_points_outside_vertical_range() {
        assert!(!is_drag_handle_point(5, 25));
        assert!(!is_drag_handle_point(5, 51));
    }

    #[test]
    fn settings_default_has_always_on_top_disabled() {
        assert!(!SettingsFile::default().always_on_top);
    }

    #[test]
    fn settings_without_always_on_top_field_defaults_to_false() {
        let settings: SettingsFile =
            serde_json::from_str("{}").expect("legacy settings should deserialize");
        assert!(!settings.always_on_top);
    }

    #[test]
    fn settings_with_always_on_top_true_deserializes_true() {
        let settings: SettingsFile =
            serde_json::from_str(r#"{"always_on_top":true}"#).expect("settings should deserialize");
        assert!(settings.always_on_top);
    }

    #[test]
    fn settings_serialization_includes_always_on_top() {
        let settings = SettingsFile {
            always_on_top: true,
            ..SettingsFile::default()
        };
        let value = serde_json::to_value(&settings).expect("settings should serialize");
        assert_eq!(value["always_on_top"], serde_json::json!(true));
    }

    #[test]
    fn display_basis_default_is_used_percentage() {
        assert_eq!(DisplayBasis::default(), DisplayBasis::UsedPercentage);
    }

    #[test]
    fn display_value_used_percentage_passes_through() {
        assert_eq!(display_value(DisplayBasis::UsedPercentage, 64.0), 64.0);
    }

    #[test]
    fn display_value_remaining_allowance_is_the_complement() {
        assert_eq!(display_value(DisplayBasis::RemainingAllowance, 64.0), 36.0);
    }

    #[test]
    fn display_value_clamps_outside_zero_to_hundred() {
        assert_eq!(display_value(DisplayBasis::UsedPercentage, -5.0), 0.0);
        assert_eq!(display_value(DisplayBasis::UsedPercentage, 150.0), 100.0);
        assert_eq!(display_value(DisplayBasis::RemainingAllowance, -5.0), 100.0);
        assert_eq!(display_value(DisplayBasis::RemainingAllowance, 150.0), 0.0);
    }

    #[test]
    fn render_cell_normal_zero_percent_is_some_zero_not_none() {
        let strings = LanguageId::English.strings();
        let section = UsageSection {
            percentage: 0.0,
            resets_at: None,
        };
        let display = render_cell(
            CellState::Ok,
            Some(&section),
            DisplayBasis::UsedPercentage,
            strings,
        );
        // A real 0% must be distinguishable from "no value at all": Some(0.0),
        // never None, so the bar can legitimately render as empty for a
        // genuine zero without being confused with a failed/loading cell.
        assert_eq!(display.bar_percent, Some(0.0));
    }

    #[test]
    fn render_cell_non_ok_states_never_produce_a_bar_percent() {
        let strings = LanguageId::English.strings();
        for state in [
            CellState::Loading,
            CellState::FetchFailed,
            CellState::Retrying,
            CellState::NotConfigured,
            CellState::NotAvailable,
        ] {
            let display = render_cell(state, None, DisplayBasis::UsedPercentage, strings);
            assert_eq!(display.bar_percent, None);
        }
    }

    #[test]
    fn legacy_settings_without_display_basis_default_to_used_percentage() {
        let settings: SettingsFile =
            serde_json::from_str("{}").expect("legacy settings should deserialize");
        assert_eq!(settings.display_basis, DisplayBasis::UsedPercentage);
    }

    #[test]
    fn settings_with_unrecognized_display_basis_falls_back_without_failing_the_whole_file() {
        let settings: SettingsFile =
            serde_json::from_str(r#"{"display_basis":"some_future_value","tray_offset":7}"#)
                .expect("an unrecognized display_basis must not fail the whole settings file");
        assert_eq!(settings.display_basis, DisplayBasis::UsedPercentage);
        assert_eq!(settings.tray_offset, 7);
    }

    #[test]
    fn settings_serialization_uses_snake_case_display_basis() {
        let settings = SettingsFile {
            display_basis: DisplayBasis::RemainingAllowance,
            ..SettingsFile::default()
        };
        let value = serde_json::to_value(&settings).expect("settings should serialize");
        assert_eq!(
            value["display_basis"],
            serde_json::json!("remaining_allowance")
        );
    }

    // ── AUM-PACE-GUIDANCE-01: display settings (type/default/persistence) ──

    #[test]
    fn display_density_default_is_standard() {
        assert_eq!(DisplayDensity::default(), DisplayDensity::Standard);
    }

    #[test]
    fn short_window_visibility_default_is_warning_only() {
        assert_eq!(
            ShortWindowVisibility::default(),
            ShortWindowVisibility::WarningOnly
        );
    }

    #[test]
    fn short_window_alert_sensitivity_default_is_standard() {
        assert_eq!(
            ShortWindowAlertSensitivity::default(),
            ShortWindowAlertSensitivity::Standard
        );
    }

    #[test]
    fn display_density_serializes_to_expected_snake_case() {
        assert_eq!(
            serde_json::to_value(DisplayDensity::Compact).unwrap(),
            serde_json::json!("compact")
        );
        assert_eq!(
            serde_json::to_value(DisplayDensity::Standard).unwrap(),
            serde_json::json!("standard")
        );
        assert_eq!(
            serde_json::to_value(DisplayDensity::Detailed).unwrap(),
            serde_json::json!("detailed")
        );
    }

    #[test]
    fn short_window_visibility_serializes_to_expected_snake_case() {
        assert_eq!(
            serde_json::to_value(ShortWindowVisibility::Always).unwrap(),
            serde_json::json!("always")
        );
        assert_eq!(
            serde_json::to_value(ShortWindowVisibility::WarningOnly).unwrap(),
            serde_json::json!("warning_only")
        );
        assert_eq!(
            serde_json::to_value(ShortWindowVisibility::Hidden).unwrap(),
            serde_json::json!("hidden")
        );
    }

    #[test]
    fn short_window_alert_sensitivity_serializes_to_expected_snake_case() {
        assert_eq!(
            serde_json::to_value(ShortWindowAlertSensitivity::Sensitive).unwrap(),
            serde_json::json!("sensitive")
        );
        assert_eq!(
            serde_json::to_value(ShortWindowAlertSensitivity::Standard).unwrap(),
            serde_json::json!("standard")
        );
        assert_eq!(
            serde_json::to_value(ShortWindowAlertSensitivity::Relaxed).unwrap(),
            serde_json::json!("relaxed")
        );
    }

    #[test]
    fn pace_settings_deserialize_from_expected_snake_case_strings() {
        let density: DisplayDensity =
            serde_json::from_value(serde_json::json!("detailed")).unwrap();
        assert_eq!(density, DisplayDensity::Detailed);

        let visibility: ShortWindowVisibility =
            serde_json::from_value(serde_json::json!("hidden")).unwrap();
        assert_eq!(visibility, ShortWindowVisibility::Hidden);

        let sensitivity: ShortWindowAlertSensitivity =
            serde_json::from_value(serde_json::json!("relaxed")).unwrap();
        assert_eq!(sensitivity, ShortWindowAlertSensitivity::Relaxed);
    }

    #[test]
    fn legacy_settings_without_pace_display_keys_deserialize_successfully() {
        let settings: SettingsFile = serde_json::from_str("{}")
            .expect("legacy settings without the new keys should still deserialize");
        assert_eq!(settings.display_density, DisplayDensity::Standard);
        assert_eq!(
            settings.short_window_visibility,
            ShortWindowVisibility::WarningOnly
        );
        assert_eq!(
            settings.short_window_alert_sensitivity,
            ShortWindowAlertSensitivity::Standard
        );
    }

    #[test]
    fn new_format_settings_load_the_saved_pace_display_values() {
        let json = r#"{
            "display_density": "detailed",
            "short_window_visibility": "always",
            "short_window_alert_sensitivity": "relaxed"
        }"#;
        let settings: SettingsFile =
            serde_json::from_str(json).expect("new-format settings should deserialize");
        assert_eq!(settings.display_density, DisplayDensity::Detailed);
        assert_eq!(
            settings.short_window_visibility,
            ShortWindowVisibility::Always
        );
        assert_eq!(
            settings.short_window_alert_sensitivity,
            ShortWindowAlertSensitivity::Relaxed
        );
    }

    #[test]
    fn settings_serialization_includes_all_pace_display_keys() {
        let value =
            serde_json::to_value(SettingsFile::default()).expect("settings should serialize");
        assert_eq!(value["display_density"], serde_json::json!("standard"));
        assert_eq!(
            value["short_window_visibility"],
            serde_json::json!("warning_only")
        );
        assert_eq!(
            value["short_window_alert_sensitivity"],
            serde_json::json!("standard")
        );
    }

    #[test]
    fn pace_display_settings_round_trip_through_serialization() {
        let settings = SettingsFile {
            display_density: DisplayDensity::Compact,
            short_window_visibility: ShortWindowVisibility::Hidden,
            short_window_alert_sensitivity: ShortWindowAlertSensitivity::Sensitive,
            ..SettingsFile::default()
        };
        let json = serde_json::to_string(&settings).expect("settings should serialize");
        let round_tripped: SettingsFile =
            serde_json::from_str(&json).expect("round trip should deserialize");
        assert_eq!(round_tripped.display_density, DisplayDensity::Compact);
        assert_eq!(
            round_tripped.short_window_visibility,
            ShortWindowVisibility::Hidden
        );
        assert_eq!(
            round_tripped.short_window_alert_sensitivity,
            ShortWindowAlertSensitivity::Sensitive
        );
    }

    #[test]
    fn existing_display_basis_survives_round_trip_alongside_new_pace_settings() {
        let settings = SettingsFile {
            display_basis: DisplayBasis::RemainingAllowance,
            display_density: DisplayDensity::Detailed,
            ..SettingsFile::default()
        };
        let json = serde_json::to_string(&settings).expect("settings should serialize");
        let round_tripped: SettingsFile =
            serde_json::from_str(&json).expect("round trip should deserialize");
        assert_eq!(
            round_tripped.display_basis,
            DisplayBasis::RemainingAllowance
        );
        assert_eq!(round_tripped.display_density, DisplayDensity::Detailed);
    }

    #[test]
    fn other_existing_settings_are_not_lost_when_pace_settings_are_present() {
        let settings = SettingsFile {
            tray_offset: 42,
            taskbar_index: 3,
            show_codex: true,
            show_claude_code: false,
            display_density: DisplayDensity::Compact,
            ..SettingsFile::default()
        };
        let json = serde_json::to_string(&settings).expect("settings should serialize");
        let round_tripped: SettingsFile =
            serde_json::from_str(&json).expect("round trip should deserialize");
        assert_eq!(round_tripped.tray_offset, 42);
        assert_eq!(round_tripped.taskbar_index, 3);
        assert!(round_tripped.show_codex);
        assert!(!round_tripped.show_claude_code);
    }

    #[test]
    fn settings_with_unrecognized_display_density_falls_back_without_failing_the_whole_file() {
        let settings: SettingsFile =
            serde_json::from_str(r#"{"display_density":"ultra_compact","tray_offset":9}"#)
                .expect("an unrecognized display_density must not fail the whole settings file");
        assert_eq!(settings.display_density, DisplayDensity::Standard);
        assert_eq!(settings.tray_offset, 9);
    }

    #[test]
    fn settings_with_unrecognized_short_window_visibility_falls_back_without_failing_the_whole_file(
    ) {
        let settings: SettingsFile =
            serde_json::from_str(r#"{"short_window_visibility":"sometimes","tray_offset":9}"#)
                .expect(
                    "an unrecognized short_window_visibility must not fail the whole settings file",
                );
        assert_eq!(
            settings.short_window_visibility,
            ShortWindowVisibility::WarningOnly
        );
        assert_eq!(settings.tray_offset, 9);
    }

    #[test]
    fn settings_with_unrecognized_short_window_alert_sensitivity_falls_back_without_failing_the_whole_file(
    ) {
        let settings: SettingsFile = serde_json::from_str(
            r#"{"short_window_alert_sensitivity":"extreme","tray_offset":9}"#,
        )
        .expect(
            "an unrecognized short_window_alert_sensitivity must not fail the whole settings file",
        );
        assert_eq!(
            settings.short_window_alert_sensitivity,
            ShortWindowAlertSensitivity::Standard
        );
        assert_eq!(settings.tray_offset, 9);
    }

    #[test]
    fn one_unrecognized_pace_display_value_does_not_affect_the_other_two() {
        let settings: SettingsFile = serde_json::from_str(
            r#"{"display_density":"bogus","short_window_visibility":"hidden","short_window_alert_sensitivity":"relaxed"}"#,
        )
        .expect("an unrecognized display_density must not fail the whole settings file");
        assert_eq!(settings.display_density, DisplayDensity::Standard);
        assert_eq!(
            settings.short_window_visibility,
            ShortWindowVisibility::Hidden
        );
        assert_eq!(
            settings.short_window_alert_sensitivity,
            ShortWindowAlertSensitivity::Relaxed
        );
    }

    // ── AUM-PACE-GUIDANCE-01: settings menu (IDs, mapping, localization) ───

    #[test]
    fn display_density_for_menu_id_maps_each_known_id() {
        assert_eq!(
            display_density_for_menu_id(IDM_DISPLAY_DENSITY_COMPACT),
            Some(DisplayDensity::Compact)
        );
        assert_eq!(
            display_density_for_menu_id(IDM_DISPLAY_DENSITY_STANDARD),
            Some(DisplayDensity::Standard)
        );
        assert_eq!(
            display_density_for_menu_id(IDM_DISPLAY_DENSITY_DETAILED),
            Some(DisplayDensity::Detailed)
        );
    }

    #[test]
    fn short_window_visibility_for_menu_id_maps_each_known_id() {
        assert_eq!(
            short_window_visibility_for_menu_id(IDM_SHORT_WINDOW_VISIBILITY_ALWAYS),
            Some(ShortWindowVisibility::Always)
        );
        assert_eq!(
            short_window_visibility_for_menu_id(IDM_SHORT_WINDOW_VISIBILITY_WARNING_ONLY),
            Some(ShortWindowVisibility::WarningOnly)
        );
        assert_eq!(
            short_window_visibility_for_menu_id(IDM_SHORT_WINDOW_VISIBILITY_HIDDEN),
            Some(ShortWindowVisibility::Hidden)
        );
    }

    #[test]
    fn short_window_alert_sensitivity_for_menu_id_maps_each_known_id() {
        assert_eq!(
            short_window_alert_sensitivity_for_menu_id(
                IDM_SHORT_WINDOW_ALERT_SENSITIVITY_SENSITIVE
            ),
            Some(ShortWindowAlertSensitivity::Sensitive)
        );
        assert_eq!(
            short_window_alert_sensitivity_for_menu_id(IDM_SHORT_WINDOW_ALERT_SENSITIVITY_STANDARD),
            Some(ShortWindowAlertSensitivity::Standard)
        );
        assert_eq!(
            short_window_alert_sensitivity_for_menu_id(IDM_SHORT_WINDOW_ALERT_SENSITIVITY_RELAXED),
            Some(ShortWindowAlertSensitivity::Relaxed)
        );
    }

    #[test]
    fn unknown_menu_id_maps_to_none_for_each_pace_display_group() {
        assert_eq!(display_density_for_menu_id(9999), None);
        assert_eq!(short_window_visibility_for_menu_id(9999), None);
        assert_eq!(short_window_alert_sensitivity_for_menu_id(9999), None);
    }

    #[test]
    fn new_pace_display_menu_ids_are_pairwise_distinct() {
        let ids = [
            IDM_DISPLAY_DENSITY_COMPACT,
            IDM_DISPLAY_DENSITY_STANDARD,
            IDM_DISPLAY_DENSITY_DETAILED,
            IDM_SHORT_WINDOW_VISIBILITY_ALWAYS,
            IDM_SHORT_WINDOW_VISIBILITY_WARNING_ONLY,
            IDM_SHORT_WINDOW_VISIBILITY_HIDDEN,
            IDM_SHORT_WINDOW_ALERT_SENSITIVITY_SENSITIVE,
            IDM_SHORT_WINDOW_ALERT_SENSITIVITY_STANDARD,
            IDM_SHORT_WINDOW_ALERT_SENSITIVITY_RELAXED,
        ];
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(
                    ids[i], ids[j],
                    "duplicate pace-display menu ID at indices {i} and {j}"
                );
            }
        }
    }

    #[test]
    fn wm_command_menu_ids_are_globally_unique() {
        // Every value dispatched through the main window's `WM_COMMAND`
        // handler, across both `window.rs` and `tray_icon.rs` (the tray
        // icon's own action reaches the same handler via
        // `tray_icon::IDM_TOGGLE_WIDGET`). `1`/`2` are the literal, unnamed
        // IDs `show_context_menu` uses directly for "Refresh"/"Exit" — kept
        // here as plain values since they aren't named constants.
        let mut ids: Vec<u16> = vec![
            1,
            2,
            IDM_FREQ_1MIN,
            IDM_FREQ_5MIN,
            IDM_FREQ_15MIN,
            IDM_FREQ_1HOUR,
            IDM_START_WITH_WINDOWS,
            IDM_ALWAYS_ON_TOP,
            IDM_RESET_POSITION,
            IDM_LANG_SYSTEM,
            IDM_LANG_ENGLISH,
            IDM_LANG_DUTCH,
            IDM_LANG_SPANISH,
            IDM_LANG_FRENCH,
            IDM_LANG_GERMAN,
            IDM_LANG_JAPANESE,
            IDM_LANG_KOREAN,
            IDM_LANG_TRADITIONAL_CHINESE,
            IDM_LANG_RUSSIAN,
            IDM_LANG_PORTUGUESE_BRAZIL,
            IDM_LANG_SIMPLIFIED_CHINESE,
            IDM_MODEL_CLAUDE_CODE,
            IDM_MODEL_CODEX,
            IDM_DISPLAY_BASIS_USED,
            IDM_DISPLAY_BASIS_REMAINING,
            IDM_DISPLAY_DENSITY_COMPACT,
            IDM_DISPLAY_DENSITY_STANDARD,
            IDM_DISPLAY_DENSITY_DETAILED,
            IDM_SHORT_WINDOW_VISIBILITY_ALWAYS,
            IDM_SHORT_WINDOW_VISIBILITY_WARNING_ONLY,
            IDM_SHORT_WINDOW_VISIBILITY_HIDDEN,
            IDM_SHORT_WINDOW_ALERT_SENSITIVITY_SENSITIVE,
            IDM_SHORT_WINDOW_ALERT_SENSITIVITY_STANDARD,
            IDM_SHORT_WINDOW_ALERT_SENSITIVITY_RELAXED,
            tray_icon::IDM_TOGGLE_WIDGET,
        ];
        #[cfg(feature = "self-update")]
        ids.push(IDM_VERSION_ACTION);
        #[cfg(feature = "antigravity")]
        ids.push(IDM_MODEL_ANTIGRAVITY);

        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(
                    ids[i], ids[j],
                    "duplicate WM_COMMAND menu ID {} at indices {i} and {j}",
                    ids[i]
                );
            }
        }
    }

    #[test]
    fn all_languages_have_non_empty_pace_display_menu_strings() {
        for language in LanguageId::ALL {
            let strings = language.strings();
            assert!(!strings.display_density.is_empty());
            assert!(!strings.display_density_compact.is_empty());
            assert!(!strings.standard_level.is_empty());
            assert!(!strings.display_density_detailed.is_empty());
            assert!(!strings.short_window_visibility.is_empty());
            assert!(!strings.short_window_visibility_always.is_empty());
            assert!(!strings.short_window_visibility_warning_only.is_empty());
            assert!(!strings.short_window_visibility_hidden.is_empty());
            assert!(!strings.short_window_alert_sensitivity.is_empty());
            assert!(!strings.short_window_alert_sensitivity_sensitive.is_empty());
            assert!(!strings.short_window_alert_sensitivity_relaxed.is_empty());
        }
    }

    #[test]
    fn countdown_text_is_none_when_resets_at_is_none() {
        let strings = LanguageId::English.strings();
        assert_eq!(countdown_text(None, strings), None);
    }

    #[test]
    fn countdown_text_uses_now_word_after_reset_has_passed() {
        let strings = LanguageId::English.strings();
        let past = SystemTime::now() - Duration::from_secs(5);
        assert_eq!(
            countdown_text(Some(past), strings),
            Some(strings.now.to_string())
        );
    }

    #[test]
    fn countdown_text_just_under_one_hour_shows_minutes() {
        let strings = LanguageId::English.strings();
        let reset = SystemTime::now() + Duration::from_secs(3599);
        let text = countdown_text(Some(reset), strings).unwrap();
        assert!(text.ends_with(strings.minute_suffix));
    }

    #[test]
    fn countdown_text_just_over_one_hour_shows_hours() {
        let strings = LanguageId::English.strings();
        let reset = SystemTime::now() + Duration::from_secs(3605);
        let text = countdown_text(Some(reset), strings).unwrap();
        assert!(text.ends_with(strings.hour_suffix));
    }

    fn usage_data_with_session_percent(percentage: f64) -> UsageData {
        let mut usage = UsageData::default();
        usage.set_session(UsageSection {
            percentage,
            resets_at: None,
        });
        usage
    }

    #[test]
    fn merge_successful_providers_updates_only_this_polls_successes() {
        // Previous poll cached Claude at 10% and Codex at 20%.
        let mut cached: Option<AppUsageData> = Some({
            let mut data = AppUsageData::default();
            data.claude_code = Some(usage_data_with_session_percent(10.0));
            data.codex = Some(usage_data_with_session_percent(20.0));
            data
        });

        // This poll: Claude succeeds fresh at 70%, Codex errors (transient).
        let report = poller::PollReport {
            claude_code: poller::ProviderPollOutcome::Success {
                source: poller::ProviderPollSource::AnthropicOauthUsage,
                attempted_at: SystemTime::now(),
                acquired_at: SystemTime::now(),
                usage: usage_data_with_session_percent(70.0),
            },
            codex: poller::ProviderPollOutcome::Error {
                source: poller::ProviderPollSource::ChatgptWhamUsage,
                attempted_at: SystemTime::now(),
                error: poller::PollError::RequestFailed,
            },
            antigravity: poller::ProviderPollOutcome::Disabled,
        };

        merge_successful_providers(&mut cached, &report);
        let merged = cached.expect("merge must not drop the cache");
        let claude = merged
            .claude_code
            .as_ref()
            .expect("claude succeeded this poll");
        assert_eq!(claude.session.percentage, 70.0);

        // Codex didn't succeed this poll, so its cache is left as-is (still
        // the previous 20%) — merge only overwrites providers that actually
        // succeeded this round.
        let codex = merged
            .codex
            .as_ref()
            .expect("previous Codex cache should remain untouched by merge");
        assert_eq!(codex.session.percentage, 20.0);

        // Rendering with the states this same report would produce (as
        // do_poll does) must show Claude's fresh value, and must never show
        // a bar for Codex — even though a real (stale) Codex section exists
        // in the cache and is explicitly passed in here, `CellState::Retrying`
        // (derived from the same report's `Error` outcome) must make
        // `render_cell` ignore it rather than display the old 20% as current.
        let strings = LanguageId::English.strings();
        let (claude_session_state, _) = poll_cell_states(&report.claude_code);
        let claude_display = render_cell(
            claude_session_state,
            Some(&claude.session),
            DisplayBasis::UsedPercentage,
            strings,
        );
        assert_eq!(claude_display.bar_percent, Some(70.0));

        let (codex_session_state, _) = poll_cell_states(&report.codex);
        assert_eq!(codex_session_state, CellState::Retrying);
        let codex_display = render_cell(
            codex_session_state,
            Some(&codex.session),
            DisplayBasis::UsedPercentage,
            strings,
        );
        assert_eq!(codex_display.bar_percent, None);
        assert_eq!(codex_display.text, strings.retrying);
    }

    // ── remaining_secs_at ──────────────────────────────────────────────

    #[test]
    fn remaining_secs_at_is_none_without_resets_at() {
        assert_eq!(remaining_secs_at(None, SystemTime::now()), None);
    }

    #[test]
    fn remaining_secs_at_is_none_when_reset_is_in_the_past() {
        let now = SystemTime::now();
        let past = now - Duration::from_secs(5);
        assert_eq!(remaining_secs_at(Some(past), now), None);
    }

    #[test]
    fn remaining_secs_at_is_zero_when_reset_is_exactly_now() {
        let now = SystemTime::now();
        assert_eq!(remaining_secs_at(Some(now), now), Some(0));
    }

    // ── elapsed_secs_in_window ─────────────────────────────────────────

    #[test]
    fn elapsed_secs_in_window_at_exact_window_length_is_zero() {
        assert_eq!(
            elapsed_secs_in_window(WEEKLY_WINDOW_SECS, WEEKLY_WINDOW_SECS),
            Some(0)
        );
    }

    #[test]
    fn elapsed_secs_in_window_within_tolerance_past_window_length_is_zero() {
        assert_eq!(
            elapsed_secs_in_window(
                WEEKLY_WINDOW_SECS + WINDOW_TIME_TOLERANCE_SECS,
                WEEKLY_WINDOW_SECS
            ),
            Some(0)
        );
    }

    #[test]
    fn elapsed_secs_in_window_beyond_tolerance_is_none() {
        assert_eq!(
            elapsed_secs_in_window(
                WEEKLY_WINDOW_SECS + WINDOW_TIME_TOLERANCE_SECS + 1,
                WEEKLY_WINDOW_SECS
            ),
            None
        );
    }

    // ── weekly_pace_status ─────────────────────────────────────────────

    #[test]
    fn weekly_pace_status_before_judging_window_is_judging() {
        assert_eq!(
            weekly_pace_status(PACE_JUDGING_MIN_ELAPSED_SECS - 1, WEEKLY_WINDOW_SECS, 50.0),
            WeeklyPaceStatus::Judging
        );
    }

    #[test]
    fn weekly_pace_status_at_judging_boundary_exits_judging() {
        assert_ne!(
            weekly_pace_status(PACE_JUDGING_MIN_ELAPSED_SECS, WEEKLY_WINDOW_SECS, 0.0),
            WeeklyPaceStatus::Judging
        );
    }

    #[test]
    fn weekly_pace_status_under_pace_boundary() {
        let elapsed = WEEKLY_WINDOW_SECS / 2; // elapsed% = 50
        let used_at_boundary = 50.0 + PACE_UNDER_PACE_MAX_PT; // pace_diff == -10.0
        assert_eq!(
            weekly_pace_status(elapsed, WEEKLY_WINDOW_SECS, used_at_boundary),
            WeeklyPaceStatus::UnderPace
        );
        assert_eq!(
            weekly_pace_status(elapsed, WEEKLY_WINDOW_SECS, used_at_boundary + 0.1),
            WeeklyPaceStatus::OnTrack
        );
    }

    #[test]
    fn weekly_pace_status_on_track_upper_boundary() {
        let elapsed = WEEKLY_WINDOW_SECS / 2;
        let used_at_boundary = 50.0 + PACE_ON_TRACK_MAX_PT; // pace_diff == 10.0
        assert_eq!(
            weekly_pace_status(elapsed, WEEKLY_WINDOW_SECS, used_at_boundary),
            WeeklyPaceStatus::OnTrack
        );
        assert_eq!(
            weekly_pace_status(elapsed, WEEKLY_WINDOW_SECS, used_at_boundary + 0.1),
            WeeklyPaceStatus::SlightlyOverpacing
        );
    }

    #[test]
    fn weekly_pace_status_slightly_overpacing_upper_boundary() {
        let elapsed = WEEKLY_WINDOW_SECS / 2;
        let used_at_boundary = 50.0 + PACE_SLIGHTLY_OVER_MAX_PT; // pace_diff == 25.0
        assert_eq!(
            weekly_pace_status(elapsed, WEEKLY_WINDOW_SECS, used_at_boundary),
            WeeklyPaceStatus::SlightlyOverpacing
        );
        assert_eq!(
            weekly_pace_status(elapsed, WEEKLY_WINDOW_SECS, used_at_boundary + 0.1),
            WeeklyPaceStatus::Overpacing
        );
    }

    #[test]
    fn weekly_pace_status_used_zero_percent() {
        let elapsed = WEEKLY_WINDOW_SECS / 2;
        assert_eq!(
            weekly_pace_status(elapsed, WEEKLY_WINDOW_SECS, 0.0),
            WeeklyPaceStatus::UnderPace
        );
    }

    #[test]
    fn weekly_pace_status_used_hundred_percent() {
        let elapsed = WEEKLY_WINDOW_SECS / 2;
        assert_eq!(
            weekly_pace_status(elapsed, WEEKLY_WINDOW_SECS, 100.0),
            WeeklyPaceStatus::Overpacing
        );
    }

    // ── future_pace_guidance ───────────────────────────────────────────

    #[test]
    fn future_pace_guidance_uses_per_day_at_or_above_threshold() {
        let guidance = future_pace_guidance(60.0, FUTURE_PACE_HOURLY_THRESHOLD_SECS)
            .expect("remaining_secs > 0 must produce guidance");
        assert_eq!(guidance.unit, FuturePaceUnit::PerDay);
        assert!((guidance.value - 40.0).abs() < 1e-9);
    }

    #[test]
    fn future_pace_guidance_uses_per_hour_below_threshold() {
        let guidance = future_pace_guidance(60.0, FUTURE_PACE_HOURLY_THRESHOLD_SECS - 1)
            .expect("remaining_secs > 0 must produce guidance");
        assert_eq!(guidance.unit, FuturePaceUnit::PerHour);
    }

    #[test]
    fn future_pace_guidance_is_none_when_remaining_is_zero() {
        assert_eq!(future_pace_guidance(50.0, 0), None);
    }

    // ── ShortWindowAlertSensitivity::thresholds ─────────────────────────

    #[test]
    fn short_window_sensitivity_thresholds_match_spec() {
        let sensitive = ShortWindowAlertSensitivity::Sensitive.thresholds();
        assert_eq!(sensitive.grace_secs, 20 * 60);
        assert_eq!(sensitive.min_used_percent, 30.0);
        assert_eq!(sensitive.exhaustion_lead_secs, 30 * 60);

        let standard = ShortWindowAlertSensitivity::Standard.thresholds();
        assert_eq!(standard.grace_secs, 30 * 60);
        assert_eq!(standard.min_used_percent, 40.0);
        assert_eq!(standard.exhaustion_lead_secs, 45 * 60);

        let relaxed = ShortWindowAlertSensitivity::Relaxed.thresholds();
        assert_eq!(relaxed.grace_secs, 45 * 60);
        assert_eq!(relaxed.min_used_percent, 50.0);
        assert_eq!(relaxed.exhaustion_lead_secs, 75 * 60);
    }

    // ── short_window_is_overpacing ───────────────────────────────────────

    #[test]
    fn short_window_just_before_grace_is_never_overpacing() {
        let sensitivity = ShortWindowAlertSensitivity::Standard;
        let t = sensitivity.thresholds();
        assert!(!short_window_is_overpacing(
            t.grace_secs - 1,
            100_000,
            99.0,
            sensitivity
        ));
    }

    #[test]
    fn short_window_grace_boundary_allows_evaluation() {
        let sensitivity = ShortWindowAlertSensitivity::Standard;
        let t = sensitivity.thresholds();
        let elapsed = t.grace_secs; // exactly at the grace boundary
        let used = 50.0;
        let secs_to_exhaustion = (100.0 - used) * elapsed as f64 / used;
        let remaining_at_boundary = secs_to_exhaustion as u64 + t.exhaustion_lead_secs;

        assert!(short_window_is_overpacing(
            elapsed,
            remaining_at_boundary,
            used,
            sensitivity
        ));
        assert!(!short_window_is_overpacing(
            elapsed,
            remaining_at_boundary - 1,
            used,
            sensitivity
        ));
    }

    #[test]
    fn short_window_min_used_percent_boundary() {
        let sensitivity = ShortWindowAlertSensitivity::Standard;
        let t = sensitivity.thresholds();
        let elapsed = t.grace_secs + 100; // comfortably past grace

        assert!(!short_window_is_overpacing(
            elapsed,
            100_000,
            t.min_used_percent - 0.1,
            sensitivity
        ));

        let used = t.min_used_percent;
        let secs_to_exhaustion = (100.0 - used) * elapsed as f64 / used;
        let remaining = secs_to_exhaustion as u64 + t.exhaustion_lead_secs;
        assert!(short_window_is_overpacing(
            elapsed,
            remaining,
            used,
            sensitivity
        ));
    }

    #[test]
    fn short_window_exhaustion_lead_boundary() {
        let sensitivity = ShortWindowAlertSensitivity::Standard;
        let t = sensitivity.thresholds();
        let elapsed = t.grace_secs + 600;
        let used = 60.0;
        let secs_to_exhaustion = (100.0 - used) * elapsed as f64 / used;
        let remaining_at_lead = secs_to_exhaustion as u64 + t.exhaustion_lead_secs;

        assert!(short_window_is_overpacing(
            elapsed,
            remaining_at_lead,
            used,
            sensitivity
        ));
        assert!(!short_window_is_overpacing(
            elapsed,
            remaining_at_lead - 1,
            used,
            sensitivity
        ));
    }

    #[test]
    fn short_window_hundred_percent_used_lead_boundary() {
        let sensitivity = ShortWindowAlertSensitivity::Standard;
        let t = sensitivity.thresholds();
        let elapsed = t.grace_secs + 1;

        assert!(short_window_is_overpacing(
            elapsed,
            t.exhaustion_lead_secs,
            100.0,
            sensitivity
        ));
        assert!(!short_window_is_overpacing(
            elapsed,
            t.exhaustion_lead_secs - 1,
            100.0,
            sensitivity
        ));
    }

    #[test]
    fn short_window_each_sensitivity_agrees_with_its_own_thresholds() {
        for sensitivity in [
            ShortWindowAlertSensitivity::Sensitive,
            ShortWindowAlertSensitivity::Standard,
            ShortWindowAlertSensitivity::Relaxed,
        ] {
            let t = sensitivity.thresholds();
            let elapsed = t.grace_secs + 60;
            let used = t.min_used_percent + 10.0;
            let secs_to_exhaustion = (100.0 - used) * elapsed as f64 / used;
            let remaining_at_boundary = secs_to_exhaustion as u64 + t.exhaustion_lead_secs;

            assert!(
                short_window_is_overpacing(elapsed, remaining_at_boundary, used, sensitivity),
                "{sensitivity:?} should be overpacing exactly at its own lead boundary"
            );
            assert!(
                !short_window_is_overpacing(elapsed, remaining_at_boundary - 1, used, sensitivity),
                "{sensitivity:?} should not be overpacing just inside its own lead boundary"
            );
        }
    }
}
