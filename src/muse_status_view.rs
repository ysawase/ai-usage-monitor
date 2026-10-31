//! Read-only Muse observation cache and status presentation.
//! This does not access muse.ai or credentials. Its input is a normalized
//! observation supplied by a separate, not-yet-released companion.
use crate::muse_bridge::{MuseConnectionState, MuseReading};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

pub fn read_observation(path: &Path, now_unix: u64) -> MuseReading {
    let contents = match fs::metadata(path) {
        Ok(metadata) if metadata.len() <= 4096 => fs::read_to_string(path),
        Ok(_) => return MuseReading::from_json("", now_unix),
        Err(error) if error.kind() == ErrorKind::NotFound => return MuseReading::not_connected(),
        Err(_) => return MuseReading::from_json("", now_unix),
    };
    match contents {
        Ok(contents) => MuseReading::from_json(&contents, now_unix),
        Err(_) => MuseReading::from_json("", now_unix),
    }
}

/// Format the exact message the GUI presents. `formatted_reset` is the
/// caller's locale/time-zone-aware rendering of an optional reset instant.
/// Never guess the reset date or turn missing/stale usage into a 0% value.
pub fn status_message(reading: &MuseReading, japanese: bool, formatted_reset: Option<&str>) -> String {
    match reading.state {
        MuseConnectionState::NotConnected => {
            if japanese {
                "Museは未接続です。ブラウザとの連携はまだ試験段階で、この画面からログインや接続はできません。".to_string()
            } else {
                "Muse is not connected. The browser companion is experimental; sign-in and connection are not available from this screen.".to_string()
            }
        }
        MuseConnectionState::Invalid => {
            if japanese {
                "Museの連携データを確認できません。使用率0%ではありません。".to_string()
            } else {
                "Muse connection data could not be validated. This does not mean 0% usage.".to_string()
            }
        }
        MuseConnectionState::Stale => {
            if japanese {
                "Museの取得データは古くなっています。現在の利用率として表示しません。".to_string()
            } else {
                "The Muse observation is stale and cannot be displayed as current usage.".to_string()
            }
        }
        MuseConnectionState::Current => {
            let used = reading.weekly_used_percent
                .map(|value| format!("{value:.0}%"))
                .unwrap_or_else(|| if japanese { "取得不可".to_string() } else { "Unavailable".to_string() });
            let plan = reading.plan.as_deref().unwrap_or("Unknown");
            let reset = formatted_reset.unwrap_or(if japanese { "不明" } else { "Unknown" });
            if japanese {
                format!("プラン: {plan}\n週間使用率: {used}\nリセット日時: {reset}\n\nこれはローカル連携データです。Metaの公式API取得を意味しません。")
            } else {
                format!("Plan: {plan}\nWeekly used: {used}\nReset: {reset}\n\nLocal companion data; not an official Meta API integration.")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct TempObservation(PathBuf);
    impl TempObservation {
        fn new() -> Self {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("aum-muse-test-{}-{id}.json", std::process::id()));
            Self(path)
        }
        fn write(&self, body: &str) {
            fs::write(&self.0, body).expect("write isolated fixture");
        }
    }
    impl Drop for TempObservation {
        fn drop(&mut self) { let _ = fs::remove_file(&self.0); }
    }
    fn observation(seen: u64, percentage: i32, reset: &str) -> String {
        format!(r#"{{"schema_version":1,"source":"muse_browser_bridge","observed_at_unix":{seen},"plan":"Muse Free","weekly_used_percent":{percentage},"weekly_resets_at_unix":{reset}}}"#)
    }

    #[test]
    fn missing_file_is_not_connected() {
        let fixture = TempObservation::new();
        let value = read_observation(&fixture.0, 1010);
        assert_eq!(value.state, MuseConnectionState::NotConnected);
        assert!(status_message(&value, true, None).contains("未接続"));
    }
    #[test]
    fn valid_zero_is_current_and_displayable() {
        let fixture = TempObservation::new();
        fixture.write(&observation(1000, 0, "2000"));
        let value = read_observation(&fixture.0, 1010);
        assert_eq!(value.state, MuseConnectionState::Current);
        let display = status_message(&value, true, Some("日時サンプル"));
        assert!(display.contains("週間使用率: 0%"));
        assert!(display.contains("プラン: Muse Free"));
        assert!(display.contains("リセット日時: 日時サンプル"));
    }
    #[test]
    fn missing_reset_is_not_invented() {
        let fixture = TempObservation::new();
        fixture.write(&observation(1000, 20, "null"));
        let value = read_observation(&fixture.0, 1010);
        assert_eq!(value.state, MuseConnectionState::Current);
        assert!(status_message(&value, false, None).contains("Reset: Unknown"));
    }
    #[test]
    fn expired_observation_is_not_current_usage() {
        let fixture = TempObservation::new();
        fixture.write(&observation(1000, 35, "2000"));
        let value = read_observation(&fixture.0, 1400);
        assert_eq!(value.state, MuseConnectionState::Stale);
        let display = status_message(&value, false, None);
        assert!(display.contains("stale"));
        assert!(!display.contains("35%"));
    }
    #[test]
    fn extra_secret_field_is_invalid() {
        let fixture = TempObservation::new();
        fixture.write(&observation(1000, 35, "2000").replace('}', ",\"cookie\":\"SECRET\"}"));
        let value = read_observation(&fixture.0, 1010);
        assert_eq!(value.state, MuseConnectionState::Invalid);
        assert!(!status_message(&value, false, None).contains("SECRET"));
    }
    #[test]
    fn oversized_cache_is_invalid() {
        let fixture = TempObservation::new();
        fixture.write(&"A".repeat(4097));
        assert_eq!(read_observation(&fixture.0, 1010).state, MuseConnectionState::Invalid);
    }
    #[test]
    fn directory_is_not_misrepresented_as_disconnected() {
        let directory = std::env::temp_dir();
        let value = read_observation(&directory, 1010);
        assert_ne!(value.state, MuseConnectionState::NotConnected);
    }
}
