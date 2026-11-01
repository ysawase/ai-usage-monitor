//! Mock-only Muse weekly-usage contract. No HTTP client, cookies, secrets,
//! browser sessions, Next.js action discovery, or filesystem writes.
//! This is intentionally NOT linked into the shipped monitor.

pub mod session_contract;

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct Usage {
    pub plan: String,
    pub weekly_used_percent: f64,
    /// Unix epoch seconds. None means the provider did not give a reset time.
    pub reset_at_unix_seconds: Option<i64>,
    pub optional_tokens_left: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    AuthenticationRequired,
    ActionChanged,
    NetworkFailure,
    TimedOut,
    Oversize,
    InvalidResponse,
    NoQuota,
}

pub fn classify_http(status: u16, has_expired_action_marker: bool) -> Result<(), Unavailable> {
    match status {
        200 => Ok(()),
        301 | 302 | 303 | 307 | 308 | 401 | 403 => Err(Unavailable::AuthenticationRequired),
        404 if has_expired_action_marker => Err(Unavailable::ActionChanged),
        _ => Err(Unavailable::NetworkFailure),
    }
}

/// Contract accepts only a bounded, synthetic React Flight text fixture.
/// This function makes no requests to the Muse service.
pub fn parse_synthetic_flight(raw: &str) -> Result<Usage, Unavailable> {
    if raw.len() > 65_536 {
        return Err(Unavailable::Oversize);
    }

    let mut subscription: Option<Value> = None;
    for line in raw.lines() {
        let Some((id, data)) = line.split_once(':') else { continue };
        if id.is_empty() || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        if !data.starts_with('{') {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(data) else { continue };
        if value.get("success") == Some(&Value::Bool(true)) {
            if subscription.is_some() {
                return Err(Unavailable::InvalidResponse);
            }
            subscription = value.get("subscription").cloned();
        }
    }
    let subscription = subscription.ok_or(Unavailable::NoQuota)?;
    let plan = subscription
        .pointer("/tier/name")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty() && s.len() <= 64)
        .ok_or(Unavailable::InvalidResponse)?
        .to_owned();
    let percent = subscription
        .pointer("/usage/percentUsed")
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && (0.0..=100.0).contains(n))
        .ok_or(Unavailable::InvalidResponse)?;

    let reset = match subscription.pointer("/usage/resetsAt") {
        None | Some(Value::Null) => None,
        Some(Value::Number(num)) => {
            let n = num.as_i64().ok_or(Unavailable::InvalidResponse)?;
            // Reject invalid timestamps instead of manufacturing dates.
            if !(946_684_800..=4_102_444_800).contains(&n) {
                return Err(Unavailable::InvalidResponse);
            }
            Some(n)
        }
        _ => return Err(Unavailable::InvalidResponse),
    };

    let optional_tokens_left = subscription
        .get("usageRowValueLabel")
        .and_then(Value::as_str)
        .and_then(|label| {
            let marker = " tokens left)";
            let end = label.find(marker)?;
            let start = label[..end].rfind('(')? + 1;
            let token_text = &label[start..end + marker.len() - 1];
            (token_text.len() <= 72 && !token_text.is_empty()).then(|| token_text.to_owned())
        });

    Ok(Usage {
        plan,
        weekly_used_percent: percent,
        reset_at_unix_seconds: reset,
        optional_tokens_left,
    })
}

/// A deterministic bounded action-discovery state machine. Network behavior
/// is intentionally not implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionState {
    Cached,
    RediscoverOnce,
    Unavailable,
}

pub fn next_action_state(state: ActionState, stale: bool) -> ActionState {
    match (state, stale) {
        (ActionState::Cached, true) => ActionState::RediscoverOnce,
        (ActionState::RediscoverOnce, true) => ActionState::Unavailable,
        (s, _) => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flight(plan: &str, percent: &str, reset: &str) -> String {
        format!(
            "0:{missing}\n1:{{\"success\":true,\"subscription\":{{\"tier\":{{\"name\":\"{plan}\"}},\"usage\":{{\"percentUsed\":{percent},\"resetsAt\":{reset}}}}}}}\n",
            missing = "{}"
        )
    }

    #[test]
    fn free_power_maximum_and_fractional_percent() {
        for (plan, used) in [("Muse Free", "0"), ("Power", "28.5"), ("Maximum", "100")] {
            let parsed = parse_synthetic_flight(&flight(plan, used, "1790634838")).unwrap();
            assert_eq!(parsed.plan, plan);
            assert_eq!(parsed.weekly_used_percent, used.parse::<f64>().unwrap());
            assert_eq!(parsed.reset_at_unix_seconds, Some(1_790_634_838));
        }
    }

    #[test]
    fn missing_optional_reset_is_unknown() {
        let parsed = parse_synthetic_flight(&flight("Muse Free", "2", "null")).unwrap();
        assert_eq!(parsed.reset_at_unix_seconds, None);
    }

    #[test]
    fn invalid_percent_and_reset_fail_closed() {
        for bad in ["-1", "101", "\"42\"", "null", "true"] {
            assert_eq!(
                parse_synthetic_flight(&flight("Muse Free", bad, "1790634838")),
                Err(Unavailable::InvalidResponse)
            );
        }
        assert_eq!(
            parse_synthetic_flight(&flight("Muse Free", "2", "999")),
            Err(Unavailable::InvalidResponse)
        );
    }

    #[test]
    fn malformed_missing_duplicate_or_oversized_is_unavailable() {
        assert_eq!(parse_synthetic_flight("not-a-flight"), Err(Unavailable::NoQuota));
        assert_eq!(parse_synthetic_flight(&"x".repeat(65_537)), Err(Unavailable::Oversize));
        let duplicated = format!("{}{}", flight("Muse Free", "2", "1790634838"), flight("Power", "3", "1790634838"));
        assert_eq!(parse_synthetic_flight(&duplicated), Err(Unavailable::InvalidResponse));
    }

    #[test]
    fn authentication_stale_action_and_transport_are_separate() {
        for status in [301, 302, 303, 307, 308, 401, 403] {
            assert_eq!(classify_http(status, false), Err(Unavailable::AuthenticationRequired));
        }
        assert_eq!(classify_http(404, true), Err(Unavailable::ActionChanged));
        assert_eq!(classify_http(404, false), Err(Unavailable::NetworkFailure));
        assert_eq!(classify_http(500, false), Err(Unavailable::NetworkFailure));
        assert_eq!(next_action_state(ActionState::Cached, true), ActionState::RediscoverOnce);
        assert_eq!(next_action_state(ActionState::RediscoverOnce, true), ActionState::Unavailable);
    }

    #[test]
    fn paid_token_detail_does_not_change_weekly_percent() {
        let body = r#"1:{"success":true,"subscription":{"tier":{"name":"Power"},"usage":{"percentUsed":3,"resetsAt":1790634838},"usageRowValueLabel":"3% used (2.8B tokens left)"}}"#;
        let parsed = parse_synthetic_flight(body).unwrap();
        assert_eq!(parsed.optional_tokens_left.as_deref(), Some("2.8B tokens left"));
        assert_eq!(parsed.weekly_used_percent, 3.0);
    }
}
