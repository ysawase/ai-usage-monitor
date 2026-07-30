use std::path::{Path, PathBuf};

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
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || *byte == b'-'
                || *byte == b'_'
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
            machine_id_file: application_root.join("machine_id.txt"),
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
            assert!(MachineId::parse(value).is_err(), "{value:?} must be invalid");
        }
    }

    #[test]
    fn rejects_dots_separators_and_colon() {
        for value in [".", "..", "home/other", r"home\other", "home:other"] {
            assert!(MachineId::parse(value).is_err(), "{value:?} must be invalid");
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
