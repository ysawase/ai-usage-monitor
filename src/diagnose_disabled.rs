pub fn is_enabled() -> bool {
    false
}

pub fn log(_message: impl AsRef<str>) {}

pub fn log_error(_context: &str, _error: impl std::fmt::Display) {}
