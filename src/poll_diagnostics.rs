use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const MAX_LOG_BYTES: u64 = 256 * 1024;

static LOG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) fn log_path() -> PathBuf {
    let root = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);

    root.join("ClaudeCodeUsageMonitor")
        .join("provider-poll.log")
}

/// Writes only caller-supplied sanitized provider-poll metadata.
/// This logger must never receive credentials, tokens, API keys,
/// response bodies, prompts, or quota payload values.
pub(crate) fn append_sanitized(line: &str) {
    let lock = LOG_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

    let path = log_path();
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }

    let should_truncate = fs::metadata(&path)
        .map(|m| m.len() >= MAX_LOG_BYTES)
        .unwrap_or(false);

    let mut options = OpenOptions::new();
    options.create(true).write(true);

    if should_truncate {
        options.truncate(true);
    } else {
        options.append(true);
    }

    if let Ok(mut file) = options.open(path) {
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_poll_log_uses_dedicated_filename() {
        assert_eq!(
            log_path().file_name().and_then(|name| name.to_str()),
            Some("provider-poll.log")
        );
    }
}
