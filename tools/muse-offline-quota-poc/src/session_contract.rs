//! Synthetic state gate for the offline Muse quota experiment.
//! No real session, browser API, Cookie access or network transport.

use crate::{parse_synthetic_flight, Unavailable, Usage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestLoginState {
    SignedOut,
    SignedIn,
    Expired,
}

#[derive(Debug, Clone, Copy)]
pub struct TestReadConditions {
    pub signed_in: TestLoginState,
    pub user_approved_this_read: bool,
    pub fixture_is_trusted: bool,
}

/// The caller supplies a synthetic fixture, never a real service response.
pub fn read_fixture_with_test_gate(
    conditions: TestReadConditions,
    synthetic_fixture: &str,
) -> Result<Usage, Unavailable> {
    if conditions.signed_in != TestLoginState::SignedIn
        || !conditions.user_approved_this_read
    {
        return Err(Unavailable::AuthenticationRequired);
    }
    if !conditions.fixture_is_trusted {
        return Err(Unavailable::InvalidResponse);
    }
    parse_synthetic_flight(synthetic_fixture)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"1:{"success":true,"subscription":{"tier":{"name":"Muse Free"},"usage":{"percentUsed":2,"resetsAt":1790634838}}}"#;

    fn conditions() -> TestReadConditions {
        TestReadConditions {
            signed_in: TestLoginState::SignedIn,
            user_approved_this_read: true,
            fixture_is_trusted: true,
        }
    }

    #[test]
    fn fake_login_and_consent_allow_offline_fixture_parse() {
        let data = read_fixture_with_test_gate(conditions(), FIXTURE).unwrap();
        assert_eq!(data.plan, "Muse Free");
        assert_eq!(data.weekly_used_percent, 2.0);
        assert_eq!(data.reset_at_unix_seconds, Some(1790634838));
    }

    #[test]
    fn fake_signed_out_or_expired_never_returns_usage() {
        for state in [TestLoginState::SignedOut, TestLoginState::Expired] {
            let mut c = conditions();
            c.signed_in = state;
            assert_eq!(
                read_fixture_with_test_gate(c, FIXTURE),
                Err(Unavailable::AuthenticationRequired)
            );
        }
    }

    #[test]
    fn no_user_consent_or_untrusted_fixture_fails_closed() {
        let mut c = conditions();
        c.user_approved_this_read = false;
        assert_eq!(
            read_fixture_with_test_gate(c, FIXTURE),
            Err(Unavailable::AuthenticationRequired)
        );
        let mut c = conditions();
        c.fixture_is_trusted = false;
        assert_eq!(
            read_fixture_with_test_gate(c, FIXTURE),
            Err(Unavailable::InvalidResponse)
        );
    }
}
