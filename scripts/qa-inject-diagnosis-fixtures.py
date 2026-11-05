"""Inject synthetic provider failures ONLY into ephemeral CI checkout (never commit)."""
import os
from pathlib import Path

if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_OS") != "Windows":
    raise SystemExit("Refusing fixture injection outside Windows GitHub Actions runner")
file = Path("src/window.rs")
src = file.read_text(encoding="utf-8")
def replace_exact(old, new):
    global src
    n = src.count(old)
    if n != 1:
        raise SystemExit(f"Unexpected fixture insertion point ({n} matches)")
    src = src.replace(old, new)
replace_exact(
    "fn show_provider_diagnosis(hwnd: HWND) {",
    """// This constant is injected into an ephemeral Windows CI checkout only.
const WM_QA_PROVIDER_DIAG: u32 = 0x805A;
const WM_QA_CTA_QUERY: u32 = 0x805B;

fn show_provider_diagnosis(hwnd: HWND) {"""
)
replace_exact(
    "    match msg {\n        // The popup has no native sizing frame.",
    """    match msg {
        WM_QA_PROVIDER_DIAG => {
            // No test hook exists in any committed product source tree.
            if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true")
                || std::env::var("AUM_QA_DIAG").as_deref() != Ok("isolated-fixture-v1")
            {
                return LRESULT(0);
            }
            let fixture = wparam.0 as u32;
            if !(1..=9).contains(&fixture) {
                return LRESULT(0);
            }
            {
                let mut guard = lock_state();
                let Some(s) = guard.as_mut() else {
                    return LRESULT(0);
                };
                s.language = LanguageId::English;
                s.popup_layout = PopupLayout::Standard;
                s.short_window_visibility = if fixture == 4 {
                    ShortWindowVisibility::Hidden
                } else {
                    ShortWindowVisibility::Always
                };
                s.show_claude_code = fixture != 8;
                s.show_codex = fixture != 3 && fixture != 7 && fixture != 9;
                s.show_antigravity = false;
                s.show_github_copilot = false;
                s.show_vercel_ai_gateway = false;
                s.data = None;
                s.session_state = if fixture == 1 || fixture == 4 || fixture == 5 || fixture == 6 || fixture == 9 {
                    CellState::FetchFailed
                } else {
                    CellState::Ok
                };
                s.weekly_state = if fixture == 7 || fixture == 9 {
                    CellState::AuthenticationExpired
                } else if fixture == 2 || fixture == 5 {
                    CellState::FetchFailed
                } else {
                    CellState::Ok
                };
                s.codex_session_state = CellState::Ok;
                s.codex_weekly_state = if fixture == 8 {
                    CellState::AuthenticationExpired
                } else if fixture == 3 {
                    CellState::FetchFailed
                } else {
                    CellState::Ok
                };
                refresh_usage_texts(s);
            }
            // Fixture 6 arms a visible session-only failure but does NOT
            // directly open a diagnosis. The QA harness must click the real
            // product help icon to exercise the user input dispatch path.
            if fixture <= 5 {
                show_provider_diagnosis(hwnd);
            } else {
                let _ = InvalidateRect(hwnd, None, false);
                if fixture >= 7 {
                    // Match production's WM_APP_USAGE_UPDATED sequence:
                    // resize the real popup before layered rendering.
                    sync_usage_geometry_if_needed(hwnd);
                    render_layered();
                }
            }
            LRESULT(0)
        }
        // The popup has no native sizing frame."""
)

# These replacements are only applied to the ephemeral Windows Actions
# checkout, never the committed product source or installed Store app.
replace_exact(
    "    fn launch(self, hwnd: HWND) {\n        match self {",
    """    fn launch(self, hwnd: HWND) {
        // QA-only kill switch: no external auth, console or browser launch.
        if std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true")
            && std::env::var("AUM_QA_DIAG").as_deref() == Ok("isolated-fixture-v1")
            && matches!(self, AuthAction::ClaudeLogin | AuthAction::CodexLogin)
        {
            let label = match self {
                AuthAction::ClaudeLogin => "qa_claude_login_dispatch_blocked",
                AuthAction::CodexLogin => "qa_codex_login_dispatch_blocked",
                _ => unreachable!(),
            };
            show_info_message(hwnd, "QA auth dispatch intercepted", label);
            return;
        }
        match self {"""
)
replace_exact(
    "        WM_QA_PROVIDER_DIAG => {",
    """        WM_QA_CTA_QUERY => {
            if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true")
                || std::env::var("AUM_QA_DIAG").as_deref() != Ok("isolated-fixture-v1")
            {
                return LRESULT(0);
            }
            let guard = lock_state();
            let Some(s) = guard.as_ref() else { return LRESULT(0); };
            let query = wparam.0 as usize;
            let hit = s.auth_cta_hit_targets.get(query / 16);
            let value = match query % 16 {
                1 => s.auth_cta_hit_targets.len() as isize,
                2 => hit.map(|h| (h.rect.left + h.rect.right) as isize / 2).unwrap_or(0),
                3 => hit.map(|h| (h.rect.top + h.rect.bottom) as isize / 2).unwrap_or(0),
                4 => hit.map(|h| (s.hovered_auth_cta_rect == Some(h.rect)) as isize).unwrap_or(0),
                5 => hit.map(|h| match h.action {
                    AuthAction::ClaudeLogin => 1,
                    AuthAction::CodexLogin => 2,
                    AuthAction::ProviderDiagnosis(QuotaFamilyId::Claude, CellState::FetchFailed) => 3,
                    _ => 0,
                }).unwrap_or(0),
                6 => hit.map(|h| h.rect.left as isize).unwrap_or(0),
                7 => hit.map(|h| h.rect.top as isize).unwrap_or(0),
                8 => hit.map(|h| h.rect.right as isize).unwrap_or(0),
                9 => hit.map(|h| h.rect.bottom as isize).unwrap_or(0),
                _ => 0,
            };
            LRESULT(value)
        }
        WM_QA_PROVIDER_DIAG => {"""
)
file.write_text(src, encoding="utf-8", newline="\n")
print("QA_PROVIDER_FIXTURES_INJECTED_EPHEMERALLY")
