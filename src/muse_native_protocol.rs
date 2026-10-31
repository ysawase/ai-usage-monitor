//! Browser Native Messaging **protocol contract** for the personal Muse PoC.
//!
//! No network I/O, browser access, registry changes, credential handling,
//! pairing, cache writes, or actual native-messaging host is implemented here.
//! All authorization and validation must precede any future side effects.
use crate::muse_bridge::{MuseConnectionState, MuseReading};
use serde::Deserialize;
use std::io::{self, Read, Write};

/// Deliberately much smaller than the browser protocol's maximum input frame.
pub const MUSE_NATIVE_FRAME_LIMIT: usize = 4096;
pub const MUSE_NATIVE_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
pub enum MuseNativeError {
    MissingTrustAnchor,
    UnauthorizedOrigin,
    InvalidFrame,
    UnsupportedVersion,
    InvalidObservation,
}

#[derive(Debug)]
pub enum MuseNativeRequest {
    Ping,
    /// Parsed strictly through the existing normalized observation contract.
    /// This is not proof that Muse provided the values.
    Observation(MuseReading),
    Disconnect,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    protocol_version: u32,
    command: Command,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
enum Command {
    Ping,
    Observation(serde_json::Value),
    Disconnect,
}

/// The signed, store-published extension's 32-character Chromium ID must be
/// determined before enabling a host. No default, wildcard or user-entered
/// arbitrary extension origin is trusted by this PoC.
pub fn is_trusted_extension_origin(origin: &str, trusted_extension_id: &str) -> bool {
    if trusted_extension_id.len() != 32
        || !trusted_extension_id.bytes().all(|byte| (b'a'..=b'p').contains(&byte))
    {
        return false;
    }
    let expected = format!("chrome-extension://{trusted_extension_id}");
    origin == expected || origin == format!("{expected}/")
}

/// A missing trusted ID is a hard stop, not a permit-all development mode.
pub fn validate_request(
    message: &[u8],
    browser_origin: &str,
    trusted_extension_id: Option<&str>,
    now_unix: u64,
) -> Result<MuseNativeRequest, MuseNativeError> {
    let id = trusted_extension_id.ok_or(MuseNativeError::MissingTrustAnchor)?;
    if !is_trusted_extension_origin(browser_origin, id) {
        return Err(MuseNativeError::UnauthorizedOrigin);
    }
    if message.is_empty() || message.len() > MUSE_NATIVE_FRAME_LIMIT {
        return Err(MuseNativeError::InvalidFrame);
    }
    let request: Envelope =
        serde_json::from_slice(message).map_err(|_| MuseNativeError::InvalidFrame)?;
    if request.protocol_version != MUSE_NATIVE_PROTOCOL_VERSION {
        return Err(MuseNativeError::UnsupportedVersion);
    }
    match request.command {
        Command::Ping => Ok(MuseNativeRequest::Ping),
        Command::Disconnect => Ok(MuseNativeRequest::Disconnect),
        Command::Observation(value) => {
            // Strict unknown-field rejection and range/freshness handling
            // remain owned by muse_bridge. Never log or persist raw values.
            let text =
                serde_json::to_string(&value).map_err(|_| MuseNativeError::InvalidObservation)?;
            let reading = MuseReading::from_json(&text, now_unix);
            if reading.state == MuseConnectionState::Invalid {
                Err(MuseNativeError::InvalidObservation)
            } else {
                Ok(MuseNativeRequest::Observation(reading))
            }
        }
    }
}

/// Read one length-prefixed frame using the Chromium native-message framing.
/// 0 bytes before a header is clean EOF; a partial header/body is an error.
/// Bounds are checked *before* allocating the message body.
pub fn read_native_frame<R: Read>(input: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut prefix = [0_u8; 4];
    if input.read(&mut prefix[..1])? == 0 {
        return Ok(None);
    }
    input.read_exact(&mut prefix[1..])?;
    let size = u32::from_ne_bytes(prefix) as usize;
    if size == 0 || size > MUSE_NATIVE_FRAME_LIMIT {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid native frame size"));
    }
    let mut bytes = vec![0_u8; size];
    input.read_exact(&mut bytes)?;
    Ok(Some(bytes))
}

/// Only use this writer for protocol responses with a verified schema.
/// Writing a raw Muse response or other user data to stdout is prohibited.
pub fn write_native_frame<W: Write>(output: &mut W, message: &[u8]) -> io::Result<()> {
    if message.is_empty() || message.len() > MUSE_NATIVE_FRAME_LIMIT {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid native frame size"));
    }
    output.write_all(&(message.len() as u32).to_ne_bytes())?;
    output.write_all(message)?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, ErrorKind};

    const TRUSTED_ID: &str = "abcdefghijklmnopabcdefghijklmnop";

    fn origin() -> String {
        format!("chrome-extension://{TRUSTED_ID}")
    }

    fn quota(percentage: &str) -> String {
        format!(
            r#"{{"protocol_version":1,"command":{{"type":"observation","payload":{{"schema_version":1,"source":"muse_browser_bridge","observed_at_unix":1000,"plan":"Muse Test","weekly_used_percent":{percentage},"weekly_resets_at_unix":2000}}}}}}"#
        )
    }

    #[test]
    fn framing_round_trip_and_clean_eof() {
        let payload = "日本語 Muse".as_bytes();
        let mut bytes = Vec::new();
        write_native_frame(&mut bytes, payload).unwrap();
        let mut cursor = Cursor::new(bytes);
        assert_eq!(read_native_frame(&mut cursor).unwrap().unwrap(), payload);
        assert!(read_native_frame(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn framing_rejects_large_zero_and_truncated_messages() {
        for size in [0_u32, 4097_u32] {
            let mut cursor = Cursor::new(size.to_ne_bytes());
            assert_eq!(
                read_native_frame(&mut cursor).unwrap_err().kind(),
                ErrorKind::InvalidData
            );
        }
        for bytes in [vec![1_u8], vec![2, 0, 0, 0, 0]] {
            let mut cursor = Cursor::new(bytes);
            assert_eq!(
                read_native_frame(&mut cursor).unwrap_err().kind(),
                ErrorKind::UnexpectedEof
            );
        }
        assert!(write_native_frame(&mut Vec::new(), &vec![0; 4097]).is_err());
    }

    #[test]
    fn no_extension_identity_means_no_connection() {
        let ping = br#"{"protocol_version":1,"command":{"type":"ping"}}"#;
        assert_eq!(
            validate_request(ping, &origin(), None, 1010).unwrap_err(),
            MuseNativeError::MissingTrustAnchor
        );
        for wrong in [
            "chrome-extension://evil",
            "https://muse.ai",
            "chrome-extension://abcdefghijklmnopabcdefghijklmnop.evil",
            "chrome-extension://abcdefghijklmnopabcdefghijklmnop/extra",
        ] {
            assert_eq!(
                validate_request(ping, wrong, Some(TRUSTED_ID), 1010).unwrap_err(),
                MuseNativeError::UnauthorizedOrigin
            );
        }
        assert!(!is_trusted_extension_origin(&origin(), "invalid"));
        assert!(!is_trusted_extension_origin(&origin(), "ABCDEFGHIJKLMNOPABCDEFGHIJKLMNOP"));
        assert!(is_trusted_extension_origin(&origin(), TRUSTED_ID));
        assert!(is_trusted_extension_origin(&format!("{}/", origin()), TRUSTED_ID));
    }

    #[test]
    fn ping_disconnect_and_observation_are_distinct() {
        for (type_name, expected) in [("ping", "ping"), ("disconnect", "disconnect")] {
            let message = format!(
                r#"{{"protocol_version":1,"command":{{"type":"{type_name}"}}}}"#
            );
            let result = validate_request(message.as_bytes(), &origin(), Some(TRUSTED_ID), 1010)
                .unwrap();
            match (result, expected) {
                (MuseNativeRequest::Ping, "ping") => {}
                (MuseNativeRequest::Disconnect, "disconnect") => {}
                _ => panic!("incorrect command"),
            }
        }
        match validate_request(quota("0").as_bytes(), &origin(), Some(TRUSTED_ID), 1010)
            .unwrap()
        {
            MuseNativeRequest::Observation(reading) => {
                assert_eq!(reading.state, MuseConnectionState::Current);
                assert_eq!(reading.weekly_used_percent, Some(0.0));
            }
            _ => panic!("expected normalized observation"),
        }
    }

    #[test]
    fn malformed_unknown_and_secret_bearing_input_is_rejected() {
        let bad = [
            r#"{"protocol_version":2,"command":{"type":"ping"}}"#,
            r#"{"protocol_version":1,"command":{"type":"do_something"}}"#,
            r#"{"protocol_version":1,"command":{"type":"ping"},"cookie":"SECRET"}"#,
        ];
        for (i, data) in bad.iter().enumerate() {
            let err = validate_request(data.as_bytes(), &origin(), Some(TRUSTED_ID), 1010)
                .unwrap_err();
            if i == 0 {
                assert_eq!(err, MuseNativeError::UnsupportedVersion);
            } else {
                assert_eq!(err, MuseNativeError::InvalidFrame);
            }
        }
        for content in [
            quota("101"),
            quota("-1"),
            quota("35").replace(r#""plan":"Muse Test""#, r#""plan":"Muse Test","cookie":"SECRET""#),
            quota("35").replace(r#""source":"muse_browser_bridge""#, r#""source":"spoofed""#),
        ] {
            assert_eq!(
                validate_request(content.as_bytes(), &origin(), Some(TRUSTED_ID), 1010)
                    .unwrap_err(),
                MuseNativeError::InvalidObservation
            );
        }
    }

    #[test]
    fn expired_reading_cannot_be_presented_as_current() {
        let request =
            validate_request(quota("15").as_bytes(), &origin(), Some(TRUSTED_ID), 1600).unwrap();
        match request {
            MuseNativeRequest::Observation(reading) => {
                assert_eq!(reading.state, MuseConnectionState::Stale);
                assert_eq!(reading.weekly_used_percent, None);
            }
            _ => panic!("expected observation"),
        }
    }
}
