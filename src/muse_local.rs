//! Personal-local Muse quota bridge, opt-in experiment only.
//!
//! This module never opens browser profiles, reads login credentials,
//! sends network requests or calls undocumented Muse endpoints directly.
//! Only a same-Windows-user helper owns the Muse WebView2 session.
//! The helper must be launched with --muse-local-bridge and enabled manually.
//! Production Microsoft Store builds do not enable this feature.

use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::models::{UsageData, UsageSection};

pub const PIPE_PATH: &str = r"\\.\pipe\SolidailyMuseQuotaLocalV1";
pub const MAX_REPLY_BYTES: u64 = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuseBridgeError {
    Unavailable,
    InvalidReply,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct MuseReading {
    pub used_percent: f64,
    pub resets_at_epoch_seconds: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MuseWireReply {
    schema_version: u32,
    status: String,
    used_percent: Option<f64>,
    resets_at_epoch_seconds: Option<i64>,
}

pub fn parse_sanitized_reply(bytes: &[u8]) -> Result<MuseReading, MuseBridgeError> {
    if bytes.is_empty() || bytes.len() > MAX_REPLY_BYTES as usize {
        return Err(MuseBridgeError::InvalidReply);
    }
    let message: MuseWireReply =
        serde_json::from_slice(bytes).map_err(|_| MuseBridgeError::InvalidReply)?;
    if message.schema_version != 1 {
        return Err(MuseBridgeError::InvalidReply);
    }
    if message.status == "unavailable"
        && message.used_percent.is_none()
        && message.resets_at_epoch_seconds.is_none()
    {
        return Err(MuseBridgeError::Unavailable);
    }
    if message.status != "success" {
        return Err(MuseBridgeError::InvalidReply);
    }
    let pct = message.used_percent.ok_or(MuseBridgeError::InvalidReply)?;
    if !pct.is_finite() || !(0.0..=100.0).contains(&pct) {
        return Err(MuseBridgeError::InvalidReply);
    }
    let reset = match message.resets_at_epoch_seconds {
        Some(value) if (946_684_800..=4_102_444_800).contains(&value) => Some(value as u64),
        Some(_) => return Err(MuseBridgeError::InvalidReply),
        None => None,
    };
    Ok(MuseReading {
        used_percent: pct,
        resets_at_epoch_seconds: reset,
    })
}

pub fn usage_from_reading(reading: MuseReading) -> UsageData {
    let mut usage = UsageData::default();
    let reset = reading
        .resets_at_epoch_seconds
        .and_then(|s| UNIX_EPOCH.checked_add(Duration::from_secs(s)));
    usage.set_weekly(UsageSection {
        percentage: reading.used_percent,
        resets_at: reset,
    });
    usage
}

/// One explicit request only: the caller (existing Refresh/timer) owns cadence.
/// A missing helper returns unavailable; do not show a stale value as fresh.
#[cfg(windows)]
pub fn read() -> Result<MuseReading, MuseBridgeError> {
    use std::fs::OpenOptions;
    // Named pipe ACL is CurrentUserOnly on the helper. Never carry cookies
    // or payloads between processes. No HTTP / profile reading in Rust.
    let mut pipe = OpenOptions::new()
        .read(true)
        .write(true)
        .open(PIPE_PATH)
        .map_err(|_| MuseBridgeError::Unavailable)?;
    pipe.write_all(b"READ\n")
        .map_err(|_| MuseBridgeError::Unavailable)?;
    let mut reply = Vec::new();
    pipe.take(MAX_REPLY_BYTES + 1)
        .read_to_end(&mut reply)
        .map_err(|_| MuseBridgeError::Unavailable)?;
    parse_sanitized_reply(&reply)
}

#[cfg(not(windows))]
pub fn read() -> Result<MuseReading, MuseBridgeError> {
    Err(MuseBridgeError::Unavailable)
}

pub fn poll() -> Result<UsageData, MuseBridgeError> {
    read().map(usage_from_reading)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_weekly_reading_is_parsed_without_session_or_identity() {
        let reading = parse_sanitized_reply(
            br#"{"schema_version":1,"status":"success","used_percent":7,"resets_at_epoch_seconds":1794584040}"#
        ).unwrap();
        assert_eq!(reading.used_percent, 7.0);
        assert_eq!(reading.resets_at_epoch_seconds, Some(1794584040));
        let usage = usage_from_reading(reading);
        assert!(!usage.session_available());
        assert!(usage.weekly_available());
        assert_eq!(usage.weekly.percentage, 7.0);
        assert_eq!(
            usage.weekly.resets_at,
            SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(1794584040))
        );
    }

    #[test]
    fn no_session_is_unavailable_not_zero() {
        let response = br#"{"schema_version":1,"status":"unavailable","used_percent":null,"resets_at_epoch_seconds":null}"#;
        assert_eq!(
            parse_sanitized_reply(response),
            Err(MuseBridgeError::Unavailable)
        );
    }

    #[test]
    fn malformed_or_untrusted_replies_fail_closed() {
        let cases: &[&[u8]] = &[
            b"",
            br#"{"schema_version":2,"status":"success","used_percent":7,"resets_at_epoch_seconds":null}"#,
            br#"{"schema_version":1,"status":"success","used_percent":-1,"resets_at_epoch_seconds":null}"#,
            br#"{"schema_version":1,"status":"success","used_percent":101,"resets_at_epoch_seconds":null}"#,
            br#"{"schema_version":1,"status":"success","used_percent":7,"resets_at_epoch_seconds":1}"#,
            br#"{"schema_version":1,"status":"success","used_percent":7,"resets_at_epoch_seconds":null,"Cookie":"secret"}"#,
            br#"{"schema_version":1,"status":"success","used_percent":null,"resets_at_epoch_seconds":null}"#,
        ];
        for input in cases {
            assert_eq!(
                parse_sanitized_reply(input),
                Err(MuseBridgeError::InvalidReply)
            );
        }
        assert_eq!(
            parse_sanitized_reply(&vec![b'x'; 513]),
            Err(MuseBridgeError::InvalidReply)
        );
    }
}
