use std::{
    fmt, fs,
    path::{Path, PathBuf},
    str,
};

const APPLICATION_DIRECTORY: &str = "ClaudeCodeUsageMonitor";
const USAGE_DIRECTORY: &str = "usage";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MachineId(String);

impl MachineId {
    pub(crate) fn parse(value: &str) -> Result<Self, InvalidMachineId> {
        let bytes = value.as_bytes();
        if !(1..=32).contains(&bytes.len()) {
            return Err(InvalidMachineId);
        }

        let first = bytes[0];
        if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
            return Err(InvalidMachineId);
        }

        if !bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-' || *byte == b'_'
        }) {
            return Err(InvalidMachineId);
        }

        Ok(Self(value.to_string()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InvalidMachineId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MachineIdReadError {
    NotConfigured,
    ReadFailed,
    InvalidEncoding,
    InvalidFormat,
    InvalidMachineId,
}

impl fmt::Display for MachineIdReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::NotConfigured => "machine ID is not configured",
            Self::ReadFailed => "unable to read machine ID",
            Self::InvalidEncoding => "machine ID file is not valid UTF-8",
            Self::InvalidFormat => "invalid machine ID file format",
            Self::InvalidMachineId => "invalid machine ID",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for MachineIdReadError {}

pub(crate) fn read_machine_id(local_data_root: &Path) -> Result<MachineId, MachineIdReadError> {
    let bytes = match fs::read(machine_id_file_path(local_data_root)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(MachineIdReadError::NotConfigured);
        }
        Err(_) => return Err(MachineIdReadError::ReadFailed),
    };
    let content = str::from_utf8(&bytes).map_err(|_| MachineIdReadError::InvalidEncoding)?;
    if content.starts_with('\u{feff}') {
        return Err(MachineIdReadError::InvalidFormat);
    }

    let value = if let Some(value) = content.strip_suffix("\r\n") {
        value
    } else if let Some(value) = content.strip_suffix('\n') {
        value
    } else {
        content
    };
    if value.is_empty() || value.contains('\r') || value.contains('\n') {
        return Err(MachineIdReadError::InvalidFormat);
    }

    MachineId::parse(value).map_err(|_| MachineIdReadError::InvalidMachineId)
}

fn machine_id_file_path(local_data_root: &Path) -> PathBuf {
    local_data_root
        .join(APPLICATION_DIRECTORY)
        .join("machine_id.txt")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SnapshotPaths {
    machine_id_file: PathBuf,
    current_snapshot: PathBuf,
    history: PathBuf,
}

impl SnapshotPaths {
    pub(crate) fn new(local_data_root: &Path, machine_id: &MachineId) -> Self {
        let application_root = local_data_root.join(APPLICATION_DIRECTORY);
        let machine_usage_root = application_root
            .join(USAGE_DIRECTORY)
            .join(machine_id.as_str());

        Self {
            machine_id_file: machine_id_file_path(local_data_root),
            current_snapshot: machine_usage_root.join("current.json"),
            history: machine_usage_root.join("history.jsonl"),
        }
    }

    pub(crate) fn machine_id_file(&self) -> &Path {
        &self.machine_id_file
    }

    pub(crate) fn current_snapshot(&self) -> &Path {
        &self.current_snapshot
    }

    pub(crate) fn history(&self) -> &Path {
        &self.history
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestRoot {
        path: PathBuf,
    }

    impl TestRoot {
        fn new(case: &str) -> Self {
            let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ai-usage-monitor-snapshot-store-{}-{case}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(path.join(APPLICATION_DIRECTORY))
                .expect("test application directory should be created");
            Self { path }
        }

        fn local_data_root(&self) -> &Path {
            &self.path
        }

        fn machine_id_file(&self) -> PathBuf {
            machine_id_file_path(&self.path)
        }

        fn write_machine_id(&self, bytes: &[u8]) {
            fs::write(self.machine_id_file(), bytes).expect("test machine ID should be written");
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn injected_root() -> PathBuf {
        PathBuf::from(r"C:\local-data-root")
    }

    #[test]
    fn accepts_named_machine_ids() {
        for value in ["home", "central", "koiwa"] {
            assert_eq!(
                MachineId::parse(value)
                    .expect("known machine ID should be valid")
                    .as_str(),
                value
            );
        }
    }

    #[test]
    fn accepts_minimum_and_maximum_lengths() {
        assert!(MachineId::parse("a").is_ok());
        assert!(MachineId::parse(&"a".repeat(32)).is_ok());
    }

    #[test]
    fn rejects_empty_and_overlong_values() {
        assert!(MachineId::parse("").is_err());
        assert!(MachineId::parse(&"a".repeat(33)).is_err());
    }

    #[test]
    fn rejects_uppercase_whitespace_newline_and_non_ascii() {
        for value in ["Home", "home office", "home\n", "日本語"] {
            assert!(
                MachineId::parse(value).is_err(),
                "{value:?} must be invalid"
            );
        }
    }

    #[test]
    fn rejects_dots_separators_and_colon() {
        for value in [".", "..", "home/other", r"home\other", "home:other"] {
            assert!(
                MachineId::parse(value).is_err(),
                "{value:?} must be invalid"
            );
        }
    }

    #[test]
    fn rejects_leading_hyphen_and_underscore() {
        assert!(MachineId::parse("-home").is_err());
        assert!(MachineId::parse("_home").is_err());
    }

    #[test]
    fn accepts_allowed_separators_and_leading_digit() {
        for value in ["pc-1", "pc_2", "1home"] {
            assert!(MachineId::parse(value).is_ok(), "{value:?} should be valid");
        }
    }

    #[test]
    fn reads_machine_id_with_no_line_ending_lf_or_crlf() {
        for (case, bytes) in [
            ("no-line-ending", b"home".as_slice()),
            ("lf", b"home\n".as_slice()),
            ("crlf", b"home\r\n".as_slice()),
        ] {
            let root = TestRoot::new(case);
            root.write_machine_id(bytes);
            let before = fs::read(root.machine_id_file()).unwrap();

            let machine_id = read_machine_id(root.local_data_root())
                .expect("valid machine ID file should be read");

            assert_eq!(machine_id.as_str(), "home");
            assert_eq!(fs::read(root.machine_id_file()).unwrap(), before);
        }
    }

    #[test]
    fn rejects_empty_machine_id_file() {
        let root = TestRoot::new("empty");
        root.write_machine_id(b"");

        assert_eq!(
            read_machine_id(root.local_data_root()),
            Err(MachineIdReadError::InvalidFormat)
        );
    }

    #[test]
    fn rejects_utf8_bom() {
        let root = TestRoot::new("bom");
        root.write_machine_id(b"\xef\xbb\xbfhome");

        assert_eq!(
            read_machine_id(root.local_data_root()),
            Err(MachineIdReadError::InvalidFormat)
        );
    }

    #[test]
    fn rejects_non_utf8() {
        let root = TestRoot::new("non-utf8");
        root.write_machine_id(&[0xff, 0xfe]);

        assert_eq!(
            read_machine_id(root.local_data_root()),
            Err(MachineIdReadError::InvalidEncoding)
        );
    }

    #[test]
    fn rejects_values_disallowed_by_machine_id_parser() {
        for (case, bytes) in [
            ("leading-space", b" home".as_slice()),
            ("trailing-space", b"home ".as_slice()),
            ("uppercase", b"Home".as_slice()),
            ("forward-slash", b"home/other".as_slice()),
            ("backslash", br"home\other".as_slice()),
        ] {
            let root = TestRoot::new(case);
            root.write_machine_id(bytes);

            assert_eq!(
                read_machine_id(root.local_data_root()),
                Err(MachineIdReadError::InvalidMachineId),
                "{case} should be rejected by MachineId::parse"
            );
        }
    }

    #[test]
    fn rejects_multiple_lines_embedded_cr_and_multiple_trailing_line_endings() {
        for (case, bytes) in [
            ("multiple-lines", b"home\nother".as_slice()),
            ("embedded-cr", b"ho\rme".as_slice()),
            ("double-lf", b"home\n\n".as_slice()),
            ("double-crlf", b"home\r\n\r\n".as_slice()),
        ] {
            let root = TestRoot::new(case);
            root.write_machine_id(bytes);

            assert_eq!(
                read_machine_id(root.local_data_root()),
                Err(MachineIdReadError::InvalidFormat),
                "{case} should be rejected as an invalid file format"
            );
        }
    }

    #[test]
    fn missing_file_is_not_configured_and_is_not_created() {
        let root = TestRoot::new("not-configured");
        let machine_id_file = root.machine_id_file();
        assert!(!machine_id_file.exists());

        assert_eq!(
            read_machine_id(root.local_data_root()),
            Err(MachineIdReadError::NotConfigured)
        );
        assert!(!machine_id_file.exists());
    }

    #[test]
    fn reading_directory_as_machine_id_file_returns_fixed_error_without_panicking() {
        let root = TestRoot::new("directory");
        fs::create_dir(root.machine_id_file()).expect("test directory should be created");

        assert_eq!(
            read_machine_id(root.local_data_root()),
            Err(MachineIdReadError::ReadFailed)
        );
    }

    #[test]
    fn read_errors_do_not_expose_content_path_or_os_error() {
        const SECRET_SENTINEL: &str = "TEST_SECRET_MACHINE_ID_7f3a";
        const PATH_SENTINEL: &str = "TEST_ABSOLUTE_PATH_9b21";

        let invalid_root = TestRoot::new(PATH_SENTINEL);
        invalid_root.write_machine_id(SECRET_SENTINEL.as_bytes());
        let invalid_error = read_machine_id(invalid_root.local_data_root()).unwrap_err();
        let invalid_rendered = format!("{invalid_error:?}: {invalid_error}");
        let invalid_path = invalid_root.path.to_string_lossy();
        assert!(!invalid_rendered.contains(SECRET_SENTINEL));
        assert!(!invalid_rendered.contains(PATH_SENTINEL));
        assert!(!invalid_rendered.contains(invalid_path.as_ref()));

        let read_root = TestRoot::new(PATH_SENTINEL);
        fs::create_dir(read_root.machine_id_file()).expect("test directory should be created");
        let read_error = read_machine_id(read_root.local_data_root()).unwrap_err();
        assert_eq!(
            format!("{read_error:?}: {read_error}"),
            "ReadFailed: unable to read machine ID"
        );
    }

    #[test]
    fn builds_machine_id_file_from_injected_root() {
        let root = injected_root();
        let machine_id = MachineId::parse("home").unwrap();
        let paths = SnapshotPaths::new(&root, &machine_id);

        assert_eq!(
            paths.machine_id_file(),
            root.join(APPLICATION_DIRECTORY).join("machine_id.txt")
        );
    }

    #[test]
    fn builds_current_snapshot_from_injected_root_and_machine_id() {
        let root = injected_root();
        let machine_id = MachineId::parse("pc-1").unwrap();
        let paths = SnapshotPaths::new(&root, &machine_id);

        assert_eq!(
            paths.current_snapshot(),
            root.join(APPLICATION_DIRECTORY)
                .join(USAGE_DIRECTORY)
                .join("pc-1")
                .join("current.json")
        );
    }

    #[test]
    fn builds_history_from_injected_root_and_machine_id() {
        let root = injected_root();
        let machine_id = MachineId::parse("pc_2").unwrap();
        let paths = SnapshotPaths::new(&root, &machine_id);

        assert_eq!(
            paths.history(),
            root.join(APPLICATION_DIRECTORY)
                .join(USAGE_DIRECTORY)
                .join("pc_2")
                .join("history.jsonl")
        );
    }

    #[test]
    fn path_construction_rejects_unvalidated_separators() {
        assert!(MachineId::parse("home/other").is_err());
        assert!(MachineId::parse(r"home\other").is_err());

        let root = injected_root();
        let machine_id = MachineId::parse("home").unwrap();
        let paths = SnapshotPaths::new(&root, &machine_id);
        assert!(paths.current_snapshot().starts_with(&root));
        assert!(paths.history().starts_with(&root));
    }

    #[test]
    fn path_construction_uses_only_the_injected_root() {
        let root = injected_root();
        let machine_id = MachineId::parse("central").unwrap();
        let paths = SnapshotPaths::new(&root, &machine_id);

        assert!(paths.machine_id_file().starts_with(&root));
        assert!(paths.current_snapshot().starts_with(&root));
        assert!(paths.history().starts_with(&root));
    }
}
