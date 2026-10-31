//! Personal Muse (muse.ai, NOT Muse Code) normalized quota bridge.
//! This module does no network I/O and never accepts raw server responses,
//! cookies, bearer tokens, chat content, or browser credentials.
use serde::{Deserialize, Serialize};

pub const MUSE_OBSERVATION_SCHEMA: u32 = 1;
pub const MUSE_MAX_AGE_SECONDS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MuseConnectionState {
    NotConnected,
    Current,
    Stale,
    Invalid,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MuseObservation {
    schema_version: u32,
    source: String,
    observed_at_unix: u64,
    plan: String,
    weekly_used_percent: f64,
    weekly_resets_at_unix: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MuseReading {
    pub state: MuseConnectionState,
    pub plan: Option<String>,
    pub weekly_used_percent: Option<f64>,
    pub weekly_resets_at_unix: Option<u64>,
    pub observed_at_unix: Option<u64>,
}
impl MuseReading {
    pub fn not_connected() -> Self {
        Self {
            state: MuseConnectionState::NotConnected,
            plan: None,
            weekly_used_percent: None,
            weekly_resets_at_unix: None,
            observed_at_unix: None,
        }
    }
    fn invalid() -> Self {
        Self {
            state: MuseConnectionState::Invalid,
            ..Self::not_connected()
        }
    }
    pub fn from_json(input: &str, now_unix: u64) -> Self {
        if input.len() > 4096 {
            return Self::invalid();
        }
        let Ok(v) = serde_json::from_str::<MuseObservation>(input) else {
            return Self::invalid();
        };
        if v.schema_version != MUSE_OBSERVATION_SCHEMA
            || v.source != "muse_browser_bridge"
            || v.plan.is_empty()
            || v.plan.len() > 80
            || v.plan.chars().any(char::is_control)
            || !v.weekly_used_percent.is_finite()
            || !(0.0..=100.0).contains(&v.weekly_used_percent)
            || v.observed_at_unix > now_unix.saturating_add(30)
            || v.weekly_resets_at_unix == Some(0)
        {
            return Self::invalid();
        }
        let fresh = now_unix.saturating_sub(v.observed_at_unix) <= MUSE_MAX_AGE_SECONDS
            && v.weekly_resets_at_unix
                .map_or(true, |reset| reset > now_unix);
        Self {
            state: if fresh {
                MuseConnectionState::Current
            } else {
                MuseConnectionState::Stale
            },
            plan: Some(v.plan),
            // Stale values are NOT currently available usage, including 0%.
            weekly_used_percent: if fresh {
                Some(v.weekly_used_percent)
            } else {
                None
            },
            weekly_resets_at_unix: v.weekly_resets_at_unix,
            observed_at_unix: Some(v.observed_at_unix),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn example(used: &str, observed: u64, reset: &str) -> String {
        format!(
            r#"{{"schema_version":1,"source":"muse_browser_bridge","observed_at_unix":{observed},"plan":"Muse Free","weekly_used_percent":{used},"weekly_resets_at_unix":{reset}}}"#
        )
    }
    #[test]
    fn zero_is_available() {
        let r = MuseReading::from_json(&example("0", 1000, "2000"), 1010);
        assert_eq!(r.state, MuseConnectionState::Current);
        assert_eq!(r.weekly_used_percent, Some(0.0));
    }
    #[test]
    fn missing_reset_is_not_invented() {
        let r = MuseReading::from_json(&example("20", 1000, "null"), 1010);
        assert_eq!(r.state, MuseConnectionState::Current);
        assert_eq!(r.weekly_resets_at_unix, None);
    }
    #[test]
    fn stale_or_expired_does_not_show_usage() {
        for r in [
            MuseReading::from_json(&example("15", 1000, "2000"), 1301),
            MuseReading::from_json(&example("15", 1000, "1010"), 1010),
        ] {
            assert_eq!(r.state, MuseConnectionState::Stale);
            assert_eq!(r.weekly_used_percent, None);
        }
    }
    #[test]
    fn rejects_invalid_percent_future_timestamp_and_extra_fields() {
        for pct in ["-1", "101", "null", "\"40\""] {
            assert_eq!(
                MuseReading::from_json(&example(pct, 1000, "2000"), 1010).state,
                MuseConnectionState::Invalid
            );
        }
        assert_eq!(
            MuseReading::from_json(&example("5", 1100, "2000"), 1010).state,
            MuseConnectionState::Invalid
        );
        let with_secret = example("5", 1000, "2000").replace('}', ",\"cookie\":\"SECRET\"}");
        assert_eq!(
            MuseReading::from_json(&with_secret, 1010).state,
            MuseConnectionState::Invalid
        );
    }
}
