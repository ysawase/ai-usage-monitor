//! Muse Native Messaging host transport (experimental, non-distributed).
//!
//! The executable intentionally runs with NO trusted extension ID and NO
//! user consent. Only tests inject those values; it is impossible for an
//! environment variable, message body, or user-supplied CLI flag to enable
//! access to Muse. No file/cache/registry/network operations occur here.
use crate::muse_bridge::MuseConnectionState;
use crate::muse_native_protocol::{
    read_native_frame, validate_request, write_native_frame, MuseNativeRequest,
    MUSE_NATIVE_PROTOCOL_VERSION,
};
use serde::Serialize;
use std::io::{self, Read, Write};

/// Authorization must originate from a future, explicit in-app setup flow.
/// Both the store-issued extension ID and user opt-in are mandatory.
#[derive(Clone, Copy)]
pub struct HostAuthorization<'a> {
    pub trusted_extension_id: Option<&'a str>,
    pub user_consented: bool,
}

impl HostAuthorization<'static> {
    /// The ONLY authorization used by the actual executable in this PoC.
    pub const fn disabled() -> Self {
        Self {
            trusted_extension_id: None,
            user_consented: false,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum HostOutcome {
    NoMessage,
    Rejected,
    TransportReady,
    ValidatedOnly,
}

#[derive(Serialize)]
struct HostReply {
    protocol_version: u32,
    status: &'static str,
}

fn reply<W: Write>(output: &mut W, status: &'static str) -> io::Result<()> {
    let body = serde_json::to_vec(&HostReply {
        protocol_version: MUSE_NATIVE_PROTOCOL_VERSION,
        status,
    })?;
    write_native_frame(output, &body)
}

/// Handle one browser-native frame. Browser origin is supplied *out of band*
/// by Chrome/Edge on argv[1], not by the untrusted JSON message.
/// The current PoC intentionally never writes a Muse observation to cache.
/// A successful normalized observation is only a protocol validation result.
pub fn serve_one<R: Read, W: Write>(
    input: &mut R,
    output: &mut W,
    browser_supplied_origin: &str,
    authorization: HostAuthorization<'_>,
    now_unix: u64,
) -> io::Result<HostOutcome> {
    if !authorization.user_consented || authorization.trusted_extension_id.is_none() {
        reply(output, "not_configured")?;
        return Ok(HostOutcome::Rejected);
    }
    // Do NOT read untrusted stdin or allocate its declared body length until
    // we know both opt-in and an allowlisted caller identity exist.
    let Some(trusted_id) = authorization.trusted_extension_id else {
        return Ok(HostOutcome::Rejected);
    };
    if !crate::muse_native_protocol::is_trusted_extension_origin(
        browser_supplied_origin,
        trusted_id,
    ) {
        reply(output, "unauthorized")?;
        return Ok(HostOutcome::Rejected);
    }
    let Some(body) = read_native_frame(input)? else {
        return Ok(HostOutcome::NoMessage);
    };
    let request = match validate_request(&body, browser_supplied_origin, Some(trusted_id), now_unix)
    {
        Ok(request) => request,
        Err(_) => {
            reply(output, "invalid_message")?;
            return Ok(HostOutcome::Rejected);
        }
    };
    match request {
        MuseNativeRequest::Ping => {
            reply(output, "transport_ready")?;
            Ok(HostOutcome::TransportReady)
        }
        MuseNativeRequest::Observation(reading) => {
            // Explicitly disclose there is no cache write and no live Muse
            // connection yet. An expired reading must never become current.
            if reading.state == MuseConnectionState::Current {
                reply(output, "validated_not_saved")?;
                Ok(HostOutcome::ValidatedOnly)
            } else {
                reply(output, "stale_not_saved")?;
                Ok(HostOutcome::Rejected)
            }
        }
        MuseNativeRequest::Disconnect => {
            // No pairing/cache state is created by this host. Disconnect is
            // acknowledged as a no-op, not presented as a completed revoke.
            reply(output, "no_pairing_to_revoke")?;
            Ok(HostOutcome::Rejected)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const ID: &str = "abcdefghijklmnopabcdefghijklmnop";
    const ORIGIN: &str = "chrome-extension://abcdefghijklmnopabcdefghijklmnop";

    fn input_frame(json: &str) -> Cursor<Vec<u8>> {
        let mut bytes = Vec::new();
        write_native_frame(&mut bytes, json.as_bytes()).unwrap();
        Cursor::new(bytes)
    }

    fn execute(content: &str, origin: &str, auth: HostAuthorization<'_>) -> (HostOutcome, String) {
        let mut incoming = input_frame(content);
        let mut outgoing = Vec::new();
        let result = serve_one(&mut incoming, &mut outgoing, origin, auth, 1010).unwrap();
        let response = read_native_frame(&mut Cursor::new(outgoing))
            .unwrap()
            .expect("a single protocol response");
        (result, String::from_utf8(response).unwrap())
    }

    #[test]
    fn disabled_by_default_even_for_valid_origin_and_ping() {
        let (outcome, response) = execute(
            r#"{"protocol_version":1,"command":{"type":"ping"}}"#,
            ORIGIN,
            HostAuthorization::disabled(),
        );
        assert_eq!(outcome, HostOutcome::Rejected);
        assert!(response.contains("not_configured"));
        assert!(!response.contains("weekly_used_percent"));
    }

    #[test]
    fn missing_explicit_opt_in_cannot_be_replaced_by_extension_id() {
        let (outcome, response) = execute(
            r#"{"protocol_version":1,"command":{"type":"ping"}}"#,
            ORIGIN,
            HostAuthorization {
                trusted_extension_id: Some(ID),
                user_consented: false,
            },
        );
        assert_eq!(outcome, HostOutcome::Rejected);
        assert!(response.contains("not_configured"));
    }

    #[test]
    fn wrong_caller_origin_cannot_request_a_message() {
        let (outcome, response) = execute(
            r#"{"protocol_version":1,"command":{"type":"ping"}}"#,
            "chrome-extension://bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            HostAuthorization {
                trusted_extension_id: Some(ID),
                user_consented: true,
            },
        );
        assert_eq!(outcome, HostOutcome::Rejected);
        assert!(response.contains("unauthorized"));
    }

    #[test]
    fn synthetic_transport_uses_only_sanitized_acknowledgements() {
        let auth = HostAuthorization {
            trusted_extension_id: Some(ID),
            user_consented: true,
        };
        let ping = r#"{"protocol_version":1,"command":{"type":"ping"}}"#;
        let (outcome, response) = execute(ping, ORIGIN, auth);
        assert_eq!(outcome, HostOutcome::TransportReady);
        assert!(response.contains("transport_ready"));

        let reading = r#"{"protocol_version":1,"command":{"type":"observation","payload":{"schema_version":1,"source":"muse_browser_bridge","observed_at_unix":1000,"plan":"Muse Synthetic","weekly_used_percent":33,"weekly_resets_at_unix":2000}}}"#;
        let (outcome, response) = execute(reading, ORIGIN, auth);
        assert_eq!(outcome, HostOutcome::ValidatedOnly);
        assert!(response.contains("validated_not_saved"));
        assert!(!response.contains("Muse Synthetic"));
        assert!(!response.contains("33"));

        let disconnect = r#"{"protocol_version":1,"command":{"type":"disconnect"}}"#;
        let (outcome, response) = execute(disconnect, ORIGIN, auth);
        assert_eq!(outcome, HostOutcome::Rejected);
        assert!(response.contains("no_pairing_to_revoke"));
    }

    #[test]
    fn bad_or_old_data_never_claims_to_be_saved() {
        let auth = HostAuthorization {
            trusted_extension_id: Some(ID),
            user_consented: true,
        };
        let malformed = r#"{"protocol_version":1,"command":{"type":"observation","payload":{"cookie":"SECRET"}}}"#;
        let (outcome, response) = execute(malformed, ORIGIN, auth);
        assert_eq!(outcome, HostOutcome::Rejected);
        assert!(response.contains("invalid_message"));
        assert!(!response.contains("SECRET"));

        let old = r#"{"protocol_version":1,"command":{"type":"observation","payload":{"schema_version":1,"source":"muse_browser_bridge","observed_at_unix":1,"plan":"Muse Synthetic","weekly_used_percent":33,"weekly_resets_at_unix":2000}}}"#;
        let (outcome, response) = execute(old, ORIGIN, auth);
        assert_eq!(outcome, HostOutcome::Rejected);
        assert!(response.contains("stale_not_saved"));
    }

    #[test]
    fn authorized_but_empty_input_does_not_invent_data() {
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        assert_eq!(
            serve_one(
                &mut input,
                &mut output,
                ORIGIN,
                HostAuthorization {
                    trusted_extension_id: Some(ID),
                    user_consented: true,
                },
                1010,
            )
            .unwrap(),
            HostOutcome::NoMessage
        );
        assert!(output.is_empty());
    }
}
