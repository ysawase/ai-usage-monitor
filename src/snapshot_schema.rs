use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

use crate::models::{UsageData, UsageSection};
use crate::poller::{PollError, PollReport, ProviderPollOutcome, ProviderPollSource};
use crate::snapshot_store::MachineId;

const SCHEMA_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SnapshotConversionError {
    TimestampBeforeUnixEpoch,
    TimestampMillisOverflow,
    UnsupportedProviderSource,
}

pub(crate) fn snapshot_from_poll_report(
    machine_id: &MachineId,
    report: &PollReport,
    captured_at: SystemTime,
) -> Result<SnapshotV1, SnapshotConversionError> {
    let captured_at = system_time_to_unix_millis(captured_at)?;
    let poll_started_at = earliest_attempted_at(report)
        .map(system_time_to_unix_millis)
        .transpose()?
        .unwrap_or(captured_at);
    let providers = Providers::new(
        provider_snapshot_from_outcome(&report.claude_code)?,
        provider_snapshot_from_outcome(&report.codex)?,
        provider_snapshot_from_outcome(&report.antigravity)?,
    );

    Ok(SnapshotV1::new(
        machine_id.as_str().to_string(),
        captured_at,
        poll_started_at,
        captured_at,
        providers,
    ))
}

fn provider_snapshot_from_outcome(
    outcome: &ProviderPollOutcome,
) -> Result<ProviderSnapshot, SnapshotConversionError> {
    match outcome {
        ProviderPollOutcome::Disabled => Ok(ProviderSnapshot::disabled()),
        ProviderPollOutcome::Success {
            source,
            attempted_at,
            acquired_at,
            usage,
        } => Ok(ProviderSnapshot::success(
            provider_source(*source)?,
            system_time_to_unix_millis(*attempted_at)?,
            system_time_to_unix_millis(*acquired_at)?,
            provider_usage(usage)?,
        )),
        ProviderPollOutcome::Error {
            source,
            attempted_at,
            error,
        } => Ok(ProviderSnapshot::error(
            Some(provider_source(*source)?),
            system_time_to_unix_millis(*attempted_at)?,
            provider_error(*error),
        )),
    }
}

fn provider_source(source: ProviderPollSource) -> Result<ProviderSource, SnapshotConversionError> {
    match source {
        ProviderPollSource::AnthropicOauthUsage => Ok(ProviderSource::AnthropicOauthUsage),
        ProviderPollSource::ChatgptWhamUsage => Ok(ProviderSource::ChatgptWhamUsage),
        ProviderPollSource::AntigravityQuotaUsage => {
            Err(SnapshotConversionError::UnsupportedProviderSource)
        }
    }
}

fn provider_error(error: PollError) -> ProviderError {
    let code = match error {
        PollError::AuthRequired => ProviderErrorCode::AuthRequired,
        PollError::NoCredentials => ProviderErrorCode::NoCredentials,
        PollError::TokenExpired => ProviderErrorCode::TokenExpired,
        PollError::RequestFailed => ProviderErrorCode::RequestFailed,
    };

    ProviderError::from_code(code)
}

fn provider_usage(usage: &UsageData) -> Result<ProviderUsage, SnapshotConversionError> {
    let session = usage
        .session_available()
        .then(|| usage_window(&usage.session))
        .transpose()?;
    let weekly = usage
        .weekly_available()
        .then(|| usage_window(&usage.weekly))
        .transpose()?;

    Ok(ProviderUsage::new(session, weekly))
}

fn usage_window(section: &UsageSection) -> Result<UsageWindow, SnapshotConversionError> {
    let resets_at = section
        .resets_at
        .map(system_time_to_unix_millis)
        .transpose()?;

    Ok(UsageWindow::new(Some(section.percentage), resets_at))
}

fn earliest_attempted_at(report: &PollReport) -> Option<SystemTime> {
    [
        attempted_at(&report.claude_code),
        attempted_at(&report.codex),
        attempted_at(&report.antigravity),
    ]
    .into_iter()
    .flatten()
    .min()
}

fn attempted_at(outcome: &ProviderPollOutcome) -> Option<SystemTime> {
    match outcome {
        ProviderPollOutcome::Disabled => None,
        ProviderPollOutcome::Success { attempted_at, .. }
        | ProviderPollOutcome::Error { attempted_at, .. } => Some(*attempted_at),
    }
}

fn system_time_to_unix_millis(time: SystemTime) -> Result<u64, SnapshotConversionError> {
    let duration = time
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SnapshotConversionError::TimestampBeforeUnixEpoch)?;
    duration_to_unix_millis(duration)
}

fn duration_to_unix_millis(duration: Duration) -> Result<u64, SnapshotConversionError> {
    u64::try_from(duration.as_millis())
        .map_err(|_| SnapshotConversionError::TimestampMillisOverflow)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotV1 {
    schema_version: SchemaVersion,
    timestamp_unit: TimestampUnit,
    machine_id: String,
    generated_at: u64,
    poll_started_at: u64,
    poll_finished_at: u64,
    providers: Providers,
}

impl SnapshotV1 {
    pub fn new(
        machine_id: String,
        generated_at: u64,
        poll_started_at: u64,
        poll_finished_at: u64,
        providers: Providers,
    ) -> Self {
        Self {
            schema_version: SchemaVersion,
            timestamp_unit: TimestampUnit::UnixMs,
            machine_id,
            generated_at,
            poll_started_at,
            poll_finished_at,
            providers,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SchemaVersion;

impl Serialize for SchemaVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(SCHEMA_VERSION)
    }
}

impl<'de> Deserialize<'de> for SchemaVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u8::deserialize(deserializer)?;
        if value == SCHEMA_VERSION {
            Ok(Self)
        } else {
            Err(de::Error::custom("unsupported snapshot schema version"))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TimestampUnit {
    UnixMs,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Providers {
    claude_code: ProviderSnapshot,
    codex: ProviderSnapshot,
    antigravity: ProviderSnapshot,
}

impl Providers {
    pub fn new(
        claude_code: ProviderSnapshot,
        codex: ProviderSnapshot,
        antigravity: ProviderSnapshot,
    ) -> Self {
        Self {
            claude_code,
            codex,
            antigravity,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderSnapshot {
    requested: bool,
    status: ProviderStatus,
    source: Option<ProviderSource>,
    attempted_at: Option<u64>,
    acquired_at: Option<u64>,
    last_success_at: Option<u64>,
    stale: bool,
    usage: Option<ProviderUsage>,
    error: Option<ProviderError>,
}

impl ProviderSnapshot {
    pub fn success(
        source: ProviderSource,
        attempted_at: u64,
        acquired_at: u64,
        usage: ProviderUsage,
    ) -> Self {
        Self {
            requested: true,
            status: ProviderStatus::Success,
            source: Some(source),
            attempted_at: Some(attempted_at),
            acquired_at: Some(acquired_at),
            last_success_at: Some(acquired_at),
            stale: false,
            usage: Some(usage),
            error: None,
        }
    }

    pub fn error(source: Option<ProviderSource>, attempted_at: u64, error: ProviderError) -> Self {
        Self {
            requested: true,
            status: ProviderStatus::Error,
            source,
            attempted_at: Some(attempted_at),
            acquired_at: None,
            last_success_at: None,
            stale: false,
            usage: None,
            error: Some(error),
        }
    }

    pub fn stale(
        source: Option<ProviderSource>,
        attempted_at: u64,
        last_success_at: u64,
        usage: ProviderUsage,
        error: ProviderError,
    ) -> Self {
        Self {
            requested: true,
            status: ProviderStatus::Stale,
            source,
            attempted_at: Some(attempted_at),
            acquired_at: Some(last_success_at),
            last_success_at: Some(last_success_at),
            stale: true,
            usage: Some(usage),
            error: Some(error),
        }
    }

    pub fn disabled() -> Self {
        Self {
            requested: false,
            status: ProviderStatus::Disabled,
            source: None,
            attempted_at: None,
            acquired_at: None,
            last_success_at: None,
            stale: false,
            usage: None,
            error: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    Success,
    Error,
    Stale,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSource {
    AnthropicOauthUsage,
    ChatgptWhamUsage,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderUsage {
    session: Option<UsageWindow>,
    weekly: Option<UsageWindow>,
}

impl ProviderUsage {
    pub fn new(session: Option<UsageWindow>, weekly: Option<UsageWindow>) -> Self {
        Self { session, weekly }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    used_percent: Option<f64>,
    resets_at: Option<u64>,
}

impl UsageWindow {
    pub fn new(used_percent: Option<f64>, resets_at: Option<u64>) -> Self {
        Self {
            used_percent,
            resets_at,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderError {
    code: ProviderErrorCode,
    message: String,
    retryable: bool,
}

impl ProviderError {
    pub fn from_code(code: ProviderErrorCode) -> Self {
        let (message, retryable) = match code {
            ProviderErrorCode::AuthRequired => ("Authentication required", false),
            ProviderErrorCode::NoCredentials => ("Credentials not found", false),
            ProviderErrorCode::TokenExpired => ("Token expired", false),
            ProviderErrorCode::RequestFailed => ("Provider request failed", true),
        };

        Self {
            code,
            message: message.to_string(),
            retryable,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorCode {
    AuthRequired,
    NoCredentials,
    TokenExpired,
    RequestFailed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn usage() -> ProviderUsage {
        ProviderUsage::new(
            Some(UsageWindow::new(Some(42.5), Some(1_725_000_000_000))),
            Some(UsageWindow::new(Some(63.0), None)),
        )
    }

    fn snapshot_with(claude_code: ProviderSnapshot) -> SnapshotV1 {
        SnapshotV1::new(
            "home".to_string(),
            1_725_000_000_200,
            1_725_000_000_000,
            1_725_000_000_100,
            Providers::new(
                claude_code,
                ProviderSnapshot::disabled(),
                ProviderSnapshot::disabled(),
            ),
        )
    }

    fn at_millis(milliseconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(milliseconds)
    }

    fn machine_id() -> MachineId {
        MachineId::parse("home").expect("test machine ID should be valid")
    }

    fn poll_usage(
        session: Option<(f64, Option<SystemTime>)>,
        weekly: Option<(f64, Option<SystemTime>)>,
    ) -> UsageData {
        let mut usage = UsageData::default();
        if let Some((percentage, resets_at)) = session {
            usage.set_session(UsageSection {
                percentage,
                resets_at,
            });
        }
        if let Some((percentage, resets_at)) = weekly {
            usage.set_weekly(UsageSection {
                percentage,
                resets_at,
            });
        }
        usage
    }

    fn success_outcome(source: ProviderPollSource, usage: UsageData) -> ProviderPollOutcome {
        ProviderPollOutcome::Success {
            source,
            attempted_at: at_millis(1_725_000_000_000),
            acquired_at: at_millis(1_725_000_000_100),
            usage,
        }
    }

    fn error_outcome(source: ProviderPollSource, error: PollError) -> ProviderPollOutcome {
        ProviderPollOutcome::Error {
            source,
            attempted_at: at_millis(1_725_000_000_000),
            error,
        }
    }

    fn poll_report(claude_code: ProviderPollOutcome, codex: ProviderPollOutcome) -> PollReport {
        PollReport {
            claude_code,
            codex,
            antigravity: ProviderPollOutcome::Disabled,
        }
    }

    fn converted_value(report: &PollReport) -> Value {
        let snapshot =
            snapshot_from_poll_report(&machine_id(), report, at_millis(1_725_000_000_200))
                .expect("report should convert");
        serde_json::to_value(snapshot).expect("snapshot should serialize")
    }

    #[test]
    fn schema_constants_are_fixed() {
        let snapshot = snapshot_with(ProviderSnapshot::disabled());
        let mut value = serde_json::to_value(snapshot).expect("snapshot should serialize");

        assert_eq!(value["schema_version"], json!(1));
        assert_eq!(value["timestamp_unit"], json!("unix_ms"));

        value["schema_version"] = json!(2);
        assert!(serde_json::from_value::<SnapshotV1>(value).is_err());
    }

    #[test]
    fn success_serialization_has_usage_without_error() {
        let provider = ProviderSnapshot::success(
            ProviderSource::AnthropicOauthUsage,
            1_725_000_000_000,
            1_725_000_000_100,
            usage(),
        );
        let value =
            serde_json::to_value(snapshot_with(provider)).expect("snapshot should serialize");
        let provider = &value["providers"]["claude_code"];

        assert_eq!(provider["requested"], json!(true));
        assert_eq!(provider["status"], json!("success"));
        assert_eq!(provider["stale"], json!(false));
        assert!(provider["usage"].is_object());
        assert_eq!(provider["error"], Value::Null);
    }

    #[test]
    fn error_serialization_has_error_without_usage() {
        let provider = ProviderSnapshot::error(
            Some(ProviderSource::ChatgptWhamUsage),
            1_725_000_000_000,
            ProviderError::from_code(ProviderErrorCode::RequestFailed),
        );
        let value =
            serde_json::to_value(snapshot_with(provider)).expect("snapshot should serialize");
        let provider = &value["providers"]["claude_code"];

        assert_eq!(provider["status"], json!("error"));
        assert_eq!(provider["stale"], json!(false));
        assert_eq!(provider["usage"], Value::Null);
        assert_eq!(provider["error"]["code"], json!("request_failed"));
    }

    #[test]
    fn stale_serialization_keeps_usage_and_error() {
        let provider = ProviderSnapshot::stale(
            Some(ProviderSource::AnthropicOauthUsage),
            1_725_000_000_000,
            1_724_999_000_000,
            usage(),
            ProviderError::from_code(ProviderErrorCode::AuthRequired),
        );
        let value =
            serde_json::to_value(snapshot_with(provider)).expect("snapshot should serialize");
        let provider = &value["providers"]["claude_code"];

        assert_eq!(provider["status"], json!("stale"));
        assert_eq!(provider["stale"], json!(true));
        assert!(provider["usage"].is_object());
        assert!(provider["error"].is_object());
    }

    #[test]
    fn disabled_serialization_has_no_attempt_or_result() {
        let value = serde_json::to_value(snapshot_with(ProviderSnapshot::disabled()))
            .expect("snapshot should serialize");
        let provider = &value["providers"]["claude_code"];

        assert_eq!(provider["requested"], json!(false));
        assert_eq!(provider["status"], json!("disabled"));
        assert_eq!(provider["source"], Value::Null);
        assert_eq!(provider["attempted_at"], Value::Null);
        assert_eq!(provider["acquired_at"], Value::Null);
        assert_eq!(provider["last_success_at"], Value::Null);
        assert_eq!(provider["stale"], json!(false));
        assert_eq!(provider["usage"], Value::Null);
        assert_eq!(provider["error"], Value::Null);
    }

    #[test]
    fn enums_serialize_as_snake_case() {
        assert_eq!(
            serde_json::to_value(ProviderStatus::Success).unwrap(),
            json!("success")
        );
        assert_eq!(
            serde_json::to_value(ProviderStatus::Error).unwrap(),
            json!("error")
        );
        assert_eq!(
            serde_json::to_value(ProviderStatus::Stale).unwrap(),
            json!("stale")
        );
        assert_eq!(
            serde_json::to_value(ProviderStatus::Disabled).unwrap(),
            json!("disabled")
        );
        assert_eq!(
            serde_json::to_value(ProviderSource::AnthropicOauthUsage).unwrap(),
            json!("anthropic_oauth_usage")
        );
        assert_eq!(
            serde_json::to_value(ProviderSource::ChatgptWhamUsage).unwrap(),
            json!("chatgpt_wham_usage")
        );
        assert_eq!(
            serde_json::to_value(ProviderErrorCode::AuthRequired).unwrap(),
            json!("auth_required")
        );
        assert_eq!(
            serde_json::to_value(ProviderErrorCode::NoCredentials).unwrap(),
            json!("no_credentials")
        );
        assert_eq!(
            serde_json::to_value(ProviderErrorCode::TokenExpired).unwrap(),
            json!("token_expired")
        );
        assert_eq!(
            serde_json::to_value(ProviderErrorCode::RequestFailed).unwrap(),
            json!("request_failed")
        );
    }

    #[test]
    fn missing_windows_and_values_serialize_as_null() {
        let usage = ProviderUsage::new(Some(UsageWindow::new(None, None)), None);
        let provider = ProviderSnapshot::success(
            ProviderSource::AnthropicOauthUsage,
            1_725_000_000_000,
            1_725_000_000_100,
            usage,
        );
        let value =
            serde_json::to_value(snapshot_with(provider)).expect("snapshot should serialize");
        let usage = &value["providers"]["claude_code"]["usage"];

        assert_eq!(usage["session"]["used_percent"], Value::Null);
        assert_eq!(usage["session"]["resets_at"], Value::Null);
        assert_eq!(usage["weekly"], Value::Null);
    }

    #[test]
    fn snapshot_round_trips_through_json() {
        let snapshot = snapshot_with(ProviderSnapshot::success(
            ProviderSource::AnthropicOauthUsage,
            1_725_000_000_000,
            1_725_000_000_100,
            usage(),
        ));
        let json = serde_json::to_string(&snapshot).expect("snapshot should serialize");
        let decoded: SnapshotV1 = serde_json::from_str(&json).expect("snapshot should deserialize");

        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn serialized_error_has_only_safe_fields() {
        let error = ProviderError::from_code(ProviderErrorCode::RequestFailed);
        let value = serde_json::to_value(error).expect("error should serialize");

        assert_eq!(
            value,
            json!({
                "code": "request_failed",
                "message": "Provider request failed",
                "retryable": true
            })
        );
    }

    #[test]
    fn claude_success_maps_both_windows() {
        let report = poll_report(
            success_outcome(
                ProviderPollSource::AnthropicOauthUsage,
                poll_usage(
                    Some((42.5, Some(at_millis(1_725_000_100_000)))),
                    Some((63.0, None)),
                ),
            ),
            ProviderPollOutcome::Disabled,
        );
        let value = converted_value(&report);
        let provider = &value["providers"]["claude_code"];

        assert_eq!(provider["status"], json!("success"));
        assert_eq!(provider["source"], json!("anthropic_oauth_usage"));
        assert_eq!(provider["attempted_at"], json!(1_725_000_000_000_u64));
        assert_eq!(provider["acquired_at"], json!(1_725_000_000_100_u64));
        assert_eq!(provider["usage"]["session"]["used_percent"], json!(42.5));
        assert_eq!(
            provider["usage"]["session"]["resets_at"],
            json!(1_725_000_100_000_u64)
        );
        assert_eq!(provider["usage"]["weekly"]["used_percent"], json!(63.0));
        assert_eq!(provider["usage"]["weekly"]["resets_at"], Value::Null);
        assert_eq!(provider["error"], Value::Null);
    }

    #[test]
    fn claude_missing_session_maps_to_null() {
        let report = poll_report(
            success_outcome(
                ProviderPollSource::AnthropicOauthUsage,
                poll_usage(None, Some((20.0, None))),
            ),
            ProviderPollOutcome::Disabled,
        );
        let value = converted_value(&report);
        let usage = &value["providers"]["claude_code"]["usage"];

        assert_eq!(usage["session"], Value::Null);
        assert_eq!(usage["weekly"]["used_percent"], json!(20.0));
    }

    #[test]
    fn claude_missing_weekly_maps_to_null() {
        let report = poll_report(
            success_outcome(
                ProviderPollSource::AnthropicOauthUsage,
                poll_usage(Some((20.0, None)), None),
            ),
            ProviderPollOutcome::Disabled,
        );
        let value = converted_value(&report);
        let usage = &value["providers"]["claude_code"]["usage"];

        assert_eq!(usage["session"]["used_percent"], json!(20.0));
        assert_eq!(usage["weekly"], Value::Null);
    }

    #[test]
    fn claude_actual_zero_remains_an_available_window() {
        let report = poll_report(
            success_outcome(
                ProviderPollSource::AnthropicOauthUsage,
                poll_usage(Some((0.0, None)), None),
            ),
            ProviderPollOutcome::Disabled,
        );
        let value = converted_value(&report);
        let session = &value["providers"]["claude_code"]["usage"]["session"];

        assert!(session.is_object());
        assert_eq!(session["used_percent"], json!(0.0));
    }

    #[test]
    fn claude_error_maps_to_safe_schema_error() {
        let report = poll_report(
            error_outcome(
                ProviderPollSource::AnthropicOauthUsage,
                PollError::AuthRequired,
            ),
            ProviderPollOutcome::Disabled,
        );
        let value = converted_value(&report);
        let provider = &value["providers"]["claude_code"];

        assert_eq!(provider["status"], json!("error"));
        assert_eq!(provider["usage"], Value::Null);
        assert_eq!(
            provider["error"],
            json!({
                "code": "auth_required",
                "message": "Authentication required",
                "retryable": false
            })
        );
    }

    #[test]
    fn claude_disabled_maps_to_disabled() {
        let report = poll_report(ProviderPollOutcome::Disabled, ProviderPollOutcome::Disabled);
        let value = converted_value(&report);
        let provider = &value["providers"]["claude_code"];

        assert_eq!(provider["requested"], json!(false));
        assert_eq!(provider["status"], json!("disabled"));
        assert_eq!(provider["attempted_at"], Value::Null);
        assert_eq!(provider["usage"], Value::Null);
        assert_eq!(provider["error"], Value::Null);
    }

    #[test]
    fn codex_success_maps_both_windows() {
        let report = poll_report(
            ProviderPollOutcome::Disabled,
            success_outcome(
                ProviderPollSource::ChatgptWhamUsage,
                poll_usage(
                    Some((12.0, Some(at_millis(1_725_000_200_000)))),
                    Some((34.0, Some(at_millis(1_725_000_300_000)))),
                ),
            ),
        );
        let value = converted_value(&report);
        let provider = &value["providers"]["codex"];

        assert_eq!(provider["status"], json!("success"));
        assert_eq!(provider["source"], json!("chatgpt_wham_usage"));
        assert_eq!(provider["usage"]["session"]["used_percent"], json!(12.0));
        assert_eq!(provider["usage"]["weekly"]["used_percent"], json!(34.0));
    }

    #[test]
    fn codex_missing_window_maps_to_null() {
        let report = poll_report(
            ProviderPollOutcome::Disabled,
            success_outcome(
                ProviderPollSource::ChatgptWhamUsage,
                poll_usage(None, Some((34.0, None))),
            ),
        );
        let value = converted_value(&report);
        let usage = &value["providers"]["codex"]["usage"];

        assert_eq!(usage["session"], Value::Null);
        assert_eq!(usage["weekly"]["used_percent"], json!(34.0));
    }

    #[test]
    fn codex_actual_zero_remains_an_available_window() {
        let report = poll_report(
            ProviderPollOutcome::Disabled,
            success_outcome(
                ProviderPollSource::ChatgptWhamUsage,
                poll_usage(None, Some((0.0, None))),
            ),
        );
        let value = converted_value(&report);
        let weekly = &value["providers"]["codex"]["usage"]["weekly"];

        assert!(weekly.is_object());
        assert_eq!(weekly["used_percent"], json!(0.0));
    }

    #[test]
    fn codex_error_maps_to_safe_schema_error() {
        let report = poll_report(
            ProviderPollOutcome::Disabled,
            error_outcome(
                ProviderPollSource::ChatgptWhamUsage,
                PollError::RequestFailed,
            ),
        );
        let value = converted_value(&report);
        let provider = &value["providers"]["codex"];

        assert_eq!(provider["status"], json!("error"));
        assert_eq!(provider["usage"], Value::Null);
        assert_eq!(
            provider["error"],
            json!({
                "code": "request_failed",
                "message": "Provider request failed",
                "retryable": true
            })
        );
    }

    #[test]
    fn codex_disabled_maps_to_disabled() {
        let report = poll_report(ProviderPollOutcome::Disabled, ProviderPollOutcome::Disabled);
        let value = converted_value(&report);
        let provider = &value["providers"]["codex"];

        assert_eq!(provider["requested"], json!(false));
        assert_eq!(provider["status"], json!("disabled"));
        assert_eq!(provider["source"], Value::Null);
    }

    #[test]
    fn machine_id_and_supplied_capture_time_are_preserved() {
        let report = poll_report(ProviderPollOutcome::Disabled, ProviderPollOutcome::Disabled);
        let machine_id = MachineId::parse("central").expect("machine ID should be valid");
        let snapshot =
            snapshot_from_poll_report(&machine_id, &report, at_millis(1_800_000_000_123))
                .expect("report should convert");
        let value = serde_json::to_value(snapshot).expect("snapshot should serialize");

        assert_eq!(value["machine_id"], json!("central"));
        assert_eq!(value["generated_at"], json!(1_800_000_000_123_u64));
        assert_eq!(value["poll_started_at"], json!(1_800_000_000_123_u64));
        assert_eq!(value["poll_finished_at"], json!(1_800_000_000_123_u64));
    }

    #[test]
    fn normal_and_missing_reset_times_remain_distinct() {
        let report = poll_report(
            success_outcome(
                ProviderPollSource::AnthropicOauthUsage,
                poll_usage(
                    Some((1.0, Some(at_millis(1_725_000_100_000)))),
                    Some((2.0, None)),
                ),
            ),
            ProviderPollOutcome::Disabled,
        );
        let value = converted_value(&report);
        let usage = &value["providers"]["claude_code"]["usage"];

        assert_eq!(usage["session"]["resets_at"], json!(1_725_000_100_000_u64));
        assert_eq!(usage["weekly"]["resets_at"], Value::Null);
    }

    #[test]
    fn captured_time_before_unix_epoch_is_rejected() {
        let report = poll_report(ProviderPollOutcome::Disabled, ProviderPollOutcome::Disabled);
        let before_epoch = UNIX_EPOCH
            .checked_sub(Duration::from_millis(1))
            .expect("one millisecond before epoch should be representable");

        assert_eq!(
            snapshot_from_poll_report(&machine_id(), &report, before_epoch),
            Err(SnapshotConversionError::TimestampBeforeUnixEpoch)
        );
    }

    #[test]
    fn reset_time_before_unix_epoch_is_rejected() {
        let before_epoch = UNIX_EPOCH
            .checked_sub(Duration::from_millis(1))
            .expect("one millisecond before epoch should be representable");
        let report = poll_report(
            success_outcome(
                ProviderPollSource::AnthropicOauthUsage,
                poll_usage(Some((1.0, Some(before_epoch))), None),
            ),
            ProviderPollOutcome::Disabled,
        );

        assert_eq!(
            snapshot_from_poll_report(&machine_id(), &report, at_millis(1_725_000_000_200)),
            Err(SnapshotConversionError::TimestampBeforeUnixEpoch)
        );
    }

    #[test]
    fn millisecond_overflow_is_rejected_without_panicking() {
        assert_eq!(
            duration_to_unix_millis(Duration::from_secs(u64::MAX)),
            Err(SnapshotConversionError::TimestampMillisOverflow)
        );
    }

    #[test]
    fn conversion_borrows_report_so_ui_conversion_remains_available() {
        let report = poll_report(
            success_outcome(
                ProviderPollSource::AnthropicOauthUsage,
                poll_usage(Some((7.0, None)), None),
            ),
            error_outcome(
                ProviderPollSource::ChatgptWhamUsage,
                PollError::RequestFailed,
            ),
        );

        snapshot_from_poll_report(&machine_id(), &report, at_millis(1_725_000_000_200))
            .expect("report should convert by reference");
        let app_usage = report
            .into_app_usage_data()
            .expect("partial success should remain successful for the UI");

        assert_eq!(
            app_usage
                .claude_code
                .expect("Claude usage should remain")
                .session
                .percentage,
            7.0
        );
        assert!(app_usage.codex.is_none());
    }

    #[test]
    fn every_poll_error_maps_to_a_stable_safe_error() {
        let cases = [
            (
                PollError::AuthRequired,
                "auth_required",
                "Authentication required",
                false,
            ),
            (
                PollError::NoCredentials,
                "no_credentials",
                "Credentials not found",
                false,
            ),
            (
                PollError::TokenExpired,
                "token_expired",
                "Token expired",
                false,
            ),
            (
                PollError::RequestFailed,
                "request_failed",
                "Provider request failed",
                true,
            ),
        ];

        for (poll_error, code, message, retryable) in cases {
            let report = poll_report(
                error_outcome(ProviderPollSource::AnthropicOauthUsage, poll_error),
                ProviderPollOutcome::Disabled,
            );
            let value = converted_value(&report);
            let error = &value["providers"]["claude_code"]["error"];

            assert_eq!(
                error,
                &json!({
                    "code": code,
                    "message": message,
                    "retryable": retryable
                })
            );
            let serialized = error.to_string().to_ascii_lowercase();
            for forbidden in [
                "access_token",
                "refresh_token",
                "account",
                "credential",
                "authorization",
                "response body",
            ] {
                assert!(
                    !serialized.contains(forbidden),
                    "{forbidden} must not be serialized"
                );
            }
        }
    }

    #[test]
    fn non_disabled_antigravity_is_rejected_by_schema_v1() {
        let report = PollReport {
            claude_code: ProviderPollOutcome::Disabled,
            codex: ProviderPollOutcome::Disabled,
            antigravity: success_outcome(
                ProviderPollSource::AntigravityQuotaUsage,
                poll_usage(Some((1.0, None)), None),
            ),
        };

        assert_eq!(
            snapshot_from_poll_report(&machine_id(), &report, at_millis(1_725_000_000_200)),
            Err(SnapshotConversionError::UnsupportedProviderSource)
        );
    }
}
