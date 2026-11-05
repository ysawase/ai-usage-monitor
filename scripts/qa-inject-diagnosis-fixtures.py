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
            if !(1..=6).contains(&fixture) {
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
                s.show_claude_code = true;
                s.show_codex = fixture != 3;
                s.show_antigravity = false;
                s.show_github_copilot = false;
                s.show_vercel_ai_gateway = false;
                s.data = None;
                s.session_state = if fixture == 1 || fixture == 4 || fixture == 5 || fixture == 6 {
                    CellState::FetchFailed
                } else {
                    CellState::Ok
                };
                s.weekly_state = if fixture == 2 || fixture == 5 {
                    CellState::FetchFailed
                } else {
                    CellState::Ok
                };
                s.codex_session_state = CellState::Ok;
                s.codex_weekly_state = if fixture == 3 {
                    CellState::FetchFailed
                } else {
                    CellState::Ok
                };
                refresh_usage_texts(s);
            }
            // Fixture 6 arms a visible session-only failure but does NOT
            // directly open a diagnosis. The QA harness must click the real
            // product help icon to exercise the user input dispatch path.
            if fixture != 6 {
                show_provider_diagnosis(hwnd);
            } else {
                let _ = windows::Win32::UI::WindowsAndMessaging::InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        // The popup has no native sizing frame."""
)
file.write_text(src, encoding="utf-8", newline="\n")
print("QA_PROVIDER_FIXTURES_INJECTED_EPHEMERALLY")
