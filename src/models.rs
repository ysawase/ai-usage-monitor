use std::time::SystemTime;

#[derive(Clone, Debug, Default)]
pub struct UsageSection {
    pub percentage: f64,
    pub resets_at: Option<SystemTime>,
}

#[derive(Clone, Debug, Default)]
pub struct UsageData {
    pub session: UsageSection,
    pub weekly: UsageSection,
    session_available: bool,
    weekly_available: bool,
}

impl UsageData {
    pub(crate) fn set_session(&mut self, section: UsageSection) {
        self.session = section;
        self.session_available = true;
    }

    pub(crate) fn set_weekly(&mut self, section: UsageSection) {
        self.weekly = section;
        self.weekly_available = true;
    }

    pub(crate) fn session_available(&self) -> bool {
        self.session_available
    }

    pub(crate) fn weekly_available(&self) -> bool {
        self.weekly_available
    }
}

#[derive(Clone, Debug, Default)]
pub struct AppUsageData {
    pub claude_code: Option<UsageData>,
    pub codex: Option<UsageData>,
    pub antigravity: Option<UsageData>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_usage_has_no_available_windows() {
        let usage = UsageData::default();

        assert!(!usage.session_available());
        assert!(!usage.weekly_available());
        assert_eq!(usage.session.percentage, 0.0);
        assert_eq!(usage.weekly.percentage, 0.0);
    }

    #[test]
    fn setters_replace_sections_and_mark_them_available() {
        let mut usage = UsageData::default();
        usage.set_session(UsageSection {
            percentage: 0.0,
            resets_at: None,
        });
        usage.set_weekly(UsageSection {
            percentage: 42.0,
            resets_at: None,
        });

        assert!(usage.session_available());
        assert!(usage.weekly_available());
        assert_eq!(usage.session.percentage, 0.0);
        assert_eq!(usage.weekly.percentage, 42.0);
    }
}
