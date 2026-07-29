use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

const SCHEMA_VERSION: u8 = 1;

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
}
