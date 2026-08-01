use std::{
    fmt, fs,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    str,
    sync::atomic::{AtomicU64, Ordering},
};

use windows::{
    core::PCWSTR,
    Win32::Storage::FileSystem::{MoveFileExW, ReplaceFileW, MOVE_FILE_FLAGS, REPLACE_FILE_FLAGS},
};

use crate::snapshot_schema::SnapshotV1;

const APPLICATION_DIRECTORY: &str = "ClaudeCodeUsageMonitor";
const USAGE_DIRECTORY: &str = "usage";
const CURRENT_SNAPSHOT_FILE: &str = "current.json";
const CURRENT_SNAPSHOT_BACKUP_FILE: &str = "current.json.bak";

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SnapshotWriteError {
    InvalidSnapshot,
    BackupConflict,
    SerializeFailed,
    InvalidPath,
    DirectoryCreateFailed,
    CurrentCheckFailed,
    TempCreateFailed,
    TempWriteFailed,
    TempSyncFailed,
    InitialPlacementFailed,
    ReplacementFailed,
}

impl fmt::Display for SnapshotWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidSnapshot => "invalid snapshot",
            Self::BackupConflict => "snapshot backup already exists",
            Self::SerializeFailed => "unable to serialize snapshot",
            Self::InvalidPath => "invalid snapshot path",
            Self::DirectoryCreateFailed => "unable to create snapshot directory",
            Self::CurrentCheckFailed => "unable to inspect current snapshot",
            Self::TempCreateFailed => "unable to create snapshot temp file",
            Self::TempWriteFailed => "unable to write snapshot temp file",
            Self::TempSyncFailed => "unable to sync snapshot temp file",
            Self::InitialPlacementFailed => "unable to place initial snapshot",
            Self::ReplacementFailed => "unable to replace current snapshot",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for SnapshotWriteError {}

pub(crate) fn write_current_snapshot(
    paths: &SnapshotPaths,
    snapshot: &SnapshotV1,
) -> Result<(), SnapshotWriteError> {
    write_current_snapshot_with(paths, snapshot, &WindowsSnapshotPlacement, || {
        TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    })
}

fn write_current_snapshot_with<F>(
    paths: &SnapshotPaths,
    snapshot: &SnapshotV1,
    placement: &impl SnapshotPlacement,
    next_temp_counter: F,
) -> Result<(), SnapshotWriteError>
where
    F: FnOnce() -> u64,
{
    snapshot
        .validate()
        .map_err(|_| SnapshotWriteError::InvalidSnapshot)?;
    ensure_no_interior_null(paths.current_snapshot())?;
    ensure_no_interior_null(paths.current_snapshot_backup())?;
    ensure_backup_absent(paths.current_snapshot_backup())?;
    let json = serde_json::to_vec(snapshot).map_err(|_| SnapshotWriteError::SerializeFailed)?;

    let current = paths.current_snapshot();
    let directory = current.parent().ok_or(SnapshotWriteError::InvalidPath)?;
    fs::create_dir_all(directory).map_err(|_| SnapshotWriteError::DirectoryCreateFailed)?;

    let temp = temp_file_path(current, next_temp_counter())?;
    let mut temp_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|_| SnapshotWriteError::TempCreateFailed)?;
    let mut temp_guard = TempFileGuard::new(temp);
    let write_result = write_and_sync(&mut temp_file, &json);
    drop(temp_file);
    write_result?;

    if current_exists(current)? {
        placement
            .replace(current, temp_guard.path(), paths.current_snapshot_backup())
            .map_err(replacement_error)?;
        temp_guard.disarm();
        let _ = fs::remove_file(paths.current_snapshot_backup());
    } else {
        placement
            .move_new(temp_guard.path(), current)
            .map_err(initial_placement_error)?;
        temp_guard.disarm();
    }
    Ok(())
}

fn ensure_backup_absent(backup: &Path) -> Result<(), SnapshotWriteError> {
    match fs::symlink_metadata(backup) {
        Ok(_) => Err(SnapshotWriteError::BackupConflict),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(SnapshotWriteError::CurrentCheckFailed),
    }
}

fn current_exists(current: &Path) -> Result<bool, SnapshotWriteError> {
    match fs::symlink_metadata(current) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(SnapshotWriteError::CurrentCheckFailed),
    }
}

fn temp_file_path(current: &Path, counter: u64) -> Result<PathBuf, SnapshotWriteError> {
    let directory = current.parent().ok_or(SnapshotWriteError::InvalidPath)?;
    Ok(directory.join(format!(
        ".{CURRENT_SNAPSHOT_FILE}.{}.{counter}.tmp",
        std::process::id()
    )))
}

fn write_and_sync(file: &mut File, json: &[u8]) -> Result<(), SnapshotWriteError> {
    file.write_all(json)
        .map_err(|_| SnapshotWriteError::TempWriteFailed)?;
    file.flush()
        .map_err(|_| SnapshotWriteError::TempWriteFailed)?;
    file.sync_all()
        .map_err(|_| SnapshotWriteError::TempSyncFailed)
}

struct TempFileGuard {
    path: PathBuf,
    armed: bool,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

trait SnapshotPlacement {
    fn move_new(&self, temp: &Path, current: &Path) -> Result<(), PlacementError>;
    fn replace(&self, current: &Path, temp: &Path, backup: &Path) -> Result<(), PlacementError>;
}

#[derive(Clone, Copy)]
enum PlacementError {
    InvalidPath,
    Failed,
}

fn initial_placement_error(error: PlacementError) -> SnapshotWriteError {
    match error {
        PlacementError::InvalidPath => SnapshotWriteError::InvalidPath,
        PlacementError::Failed => SnapshotWriteError::InitialPlacementFailed,
    }
}

fn replacement_error(error: PlacementError) -> SnapshotWriteError {
    match error {
        PlacementError::InvalidPath => SnapshotWriteError::InvalidPath,
        PlacementError::Failed => SnapshotWriteError::ReplacementFailed,
    }
}

struct WindowsSnapshotPlacement;

impl SnapshotPlacement for WindowsSnapshotPlacement {
    fn move_new(&self, temp: &Path, current: &Path) -> Result<(), PlacementError> {
        let temp = path_to_wide(temp).map_err(|_| PlacementError::InvalidPath)?;
        let current = path_to_wide(current).map_err(|_| PlacementError::InvalidPath)?;
        unsafe {
            MoveFileExW(
                PCWSTR::from_raw(temp.as_ptr()),
                PCWSTR::from_raw(current.as_ptr()),
                MOVE_FILE_FLAGS(0),
            )
        }
        .map_err(|_| PlacementError::Failed)
    }

    fn replace(&self, current: &Path, temp: &Path, backup: &Path) -> Result<(), PlacementError> {
        let current = path_to_wide(current).map_err(|_| PlacementError::InvalidPath)?;
        let temp = path_to_wide(temp).map_err(|_| PlacementError::InvalidPath)?;
        let backup = path_to_wide(backup).map_err(|_| PlacementError::InvalidPath)?;
        unsafe {
            ReplaceFileW(
                PCWSTR::from_raw(current.as_ptr()),
                PCWSTR::from_raw(temp.as_ptr()),
                PCWSTR::from_raw(backup.as_ptr()),
                REPLACE_FILE_FLAGS(0),
                None,
                None,
            )
        }
        .map_err(|_| PlacementError::Failed)
    }
}

fn path_to_wide(path: &Path) -> Result<Vec<u16>, SnapshotWriteError> {
    ensure_no_interior_null(path)?;
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    Ok(wide)
}

fn ensure_no_interior_null(path: &Path) -> Result<(), SnapshotWriteError> {
    if path.as_os_str().encode_wide().any(|unit| unit == 0) {
        Err(SnapshotWriteError::InvalidPath)
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SnapshotPaths {
    machine_id_file: PathBuf,
    current_snapshot: PathBuf,
    current_snapshot_backup: PathBuf,
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
            current_snapshot: machine_usage_root.join(CURRENT_SNAPSHOT_FILE),
            current_snapshot_backup: machine_usage_root.join(CURRENT_SNAPSHOT_BACKUP_FILE),
            history: machine_usage_root.join("history.jsonl"),
        }
    }

    pub(crate) fn machine_id_file(&self) -> &Path {
        &self.machine_id_file
    }

    pub(crate) fn current_snapshot(&self) -> &Path {
        &self.current_snapshot
    }

    fn current_snapshot_backup(&self) -> &Path {
        &self.current_snapshot_backup
    }

    pub(crate) fn history(&self) -> &Path {
        &self.history
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HistoryAppendError {
    InvalidSnapshot,
    SerializeFailed,
    InvalidPath,
    DirectoryCreateFailed,
    HistoryOpenFailed,
    HistoryTailCheckFailed,
    HistoryTailInvalid,
    HistoryAppendFailed,
    HistorySyncFailed,
}

impl fmt::Display for HistoryAppendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidSnapshot => "invalid snapshot",
            Self::SerializeFailed => "unable to serialize snapshot",
            Self::InvalidPath => "invalid snapshot history path",
            Self::DirectoryCreateFailed => "unable to create snapshot directory",
            Self::HistoryOpenFailed => "unable to open snapshot history",
            Self::HistoryTailCheckFailed => "unable to inspect snapshot history",
            Self::HistoryTailInvalid => "snapshot history ends unexpectedly",
            Self::HistoryAppendFailed => "unable to append snapshot history entry",
            Self::HistorySyncFailed => "unable to sync snapshot history entry",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for HistoryAppendError {}

pub(crate) fn append_snapshot_to_history(
    paths: &SnapshotPaths,
    snapshot: &SnapshotV1,
) -> Result<(), HistoryAppendError> {
    snapshot
        .validate()
        .map_err(|_| HistoryAppendError::InvalidSnapshot)?;

    let history = paths.history();
    if history.as_os_str().encode_wide().any(|unit| unit == 0) {
        return Err(HistoryAppendError::InvalidPath);
    }

    let mut line = serde_json::to_vec(snapshot).map_err(|_| HistoryAppendError::SerializeFailed)?;
    line.push(b'\n');

    let directory = history.parent().ok_or(HistoryAppendError::InvalidPath)?;
    fs::create_dir_all(directory).map_err(|_| HistoryAppendError::DirectoryCreateFailed)?;

    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(history)
        .map_err(|_| HistoryAppendError::HistoryOpenFailed)?;

    ensure_valid_history_tail(&mut file)?;

    file.write_all(&line)
        .map_err(|_| HistoryAppendError::HistoryAppendFailed)?;
    file.flush()
        .map_err(|_| HistoryAppendError::HistoryAppendFailed)?;
    file.sync_all()
        .map_err(|_| HistoryAppendError::HistorySyncFailed)
}

fn ensure_valid_history_tail(file: &mut File) -> Result<(), HistoryAppendError> {
    let length = file
        .metadata()
        .map_err(|_| HistoryAppendError::HistoryTailCheckFailed)?
        .len();
    if length == 0 {
        return Ok(());
    }

    let tail_len = length.min(2) as usize;
    file.seek(SeekFrom::End(-(tail_len as i64)))
        .map_err(|_| HistoryAppendError::HistoryTailCheckFailed)?;
    let mut tail = [0u8; 2];
    file.read_exact(&mut tail[..tail_len])
        .map_err(|_| HistoryAppendError::HistoryTailCheckFailed)?;

    if tail[tail_len - 1] != b'\n' {
        return Err(HistoryAppendError::HistoryTailInvalid);
    }
    if tail_len == 2 && tail[0] == b'\r' {
        return Err(HistoryAppendError::HistoryTailInvalid);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PersistResult {
    Saved,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SnapshotPersistOutcome {
    pub(crate) current: PersistResult,
    pub(crate) history: PersistResult,
}

pub(crate) fn persist_snapshot(
    paths: &SnapshotPaths,
    snapshot: &SnapshotV1,
) -> SnapshotPersistOutcome {
    let current = match write_current_snapshot(paths, snapshot) {
        Ok(()) => PersistResult::Saved,
        Err(_) => PersistResult::Failed,
    };
    let history = match append_snapshot_to_history(paths, snapshot) {
        Ok(()) => PersistResult::Saved,
        Err(_) => PersistResult::Failed,
    };
    SnapshotPersistOutcome { current, history }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};

    use crate::snapshot_schema::{
        deserialize_validated_snapshot, ProviderSnapshot, Providers, SnapshotV1,
    };

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestRoot {
        path: PathBuf,
    }

    impl TestRoot {
        fn uncreated(case: &str) -> Self {
            let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ai-usage-monitor-snapshot-store-{}-{case}-{sequence}",
                std::process::id()
            ));
            Self { path }
        }

        fn new(case: &str) -> Self {
            let root = Self::uncreated(case);
            fs::create_dir_all(root.path.join(APPLICATION_DIRECTORY))
                .expect("test application directory should be created");
            root
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

        fn snapshot_paths(&self) -> SnapshotPaths {
            SnapshotPaths::new(
                self.local_data_root(),
                &MachineId::parse("home").expect("test machine ID should be valid"),
            )
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

    fn snapshot(machine_id: &str, generated_at: u64) -> SnapshotV1 {
        SnapshotV1::new(
            machine_id.to_string(),
            generated_at,
            generated_at,
            generated_at,
            Providers::new(
                ProviderSnapshot::disabled(),
                ProviderSnapshot::disabled(),
                ProviderSnapshot::disabled(),
            ),
        )
    }

    fn create_snapshot_directory(paths: &SnapshotPaths) {
        fs::create_dir_all(
            paths
                .current_snapshot()
                .parent()
                .expect("snapshot path should have a parent"),
        )
        .expect("test snapshot directory should be created");
    }

    fn snapshot_temp_files(paths: &SnapshotPaths) -> Vec<PathBuf> {
        let Some(directory) = paths.current_snapshot().parent() else {
            return Vec::new();
        };
        let Ok(entries) = fs::read_dir(directory) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with(&format!(".{CURRENT_SNAPSHOT_FILE}."))
                            && name.ends_with(".tmp")
                    })
            })
            .collect()
    }

    struct FailingPlacement;

    impl SnapshotPlacement for FailingPlacement {
        fn move_new(&self, _temp: &Path, _current: &Path) -> Result<(), PlacementError> {
            Err(PlacementError::Failed)
        }

        fn replace(
            &self,
            _current: &Path,
            _temp: &Path,
            _backup: &Path,
        ) -> Result<(), PlacementError> {
            Err(PlacementError::Failed)
        }
    }

    struct CompetingInitialPlacement;

    impl SnapshotPlacement for CompetingInitialPlacement {
        fn move_new(&self, _temp: &Path, current: &Path) -> Result<(), PlacementError> {
            fs::write(current, b"competing-current").expect("competing current should be created");
            Err(PlacementError::Failed)
        }

        fn replace(
            &self,
            _current: &Path,
            _temp: &Path,
            _backup: &Path,
        ) -> Result<(), PlacementError> {
            Err(PlacementError::Failed)
        }
    }

    struct FailingReplacementWithBackup;

    impl SnapshotPlacement for FailingReplacementWithBackup {
        fn move_new(&self, _temp: &Path, _current: &Path) -> Result<(), PlacementError> {
            Err(PlacementError::Failed)
        }

        fn replace(
            &self,
            current: &Path,
            _temp: &Path,
            backup: &Path,
        ) -> Result<(), PlacementError> {
            fs::copy(current, backup).expect("recovery backup should be simulated");
            Err(PlacementError::Failed)
        }
    }

    struct SuccessfulReplacementWithUndeletableBackup;

    impl SnapshotPlacement for SuccessfulReplacementWithUndeletableBackup {
        fn move_new(&self, _temp: &Path, _current: &Path) -> Result<(), PlacementError> {
            Err(PlacementError::Failed)
        }

        fn replace(
            &self,
            current: &Path,
            temp: &Path,
            backup: &Path,
        ) -> Result<(), PlacementError> {
            let old_current = fs::read(current).unwrap();
            fs::create_dir(backup).unwrap();
            fs::write(backup.join("old-current"), old_current).unwrap();
            fs::write(current, fs::read(temp).unwrap()).unwrap();
            fs::remove_file(temp).unwrap();
            Ok(())
        }
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
    fn creates_initial_current_as_compact_validated_json() {
        let root = TestRoot::uncreated("initial-current");
        let paths = root.snapshot_paths();
        let expected = snapshot("home", 1_725_000_000_200);

        write_current_snapshot(&paths, &expected).expect("initial snapshot should be written");

        let bytes = fs::read(paths.current_snapshot()).expect("current snapshot should be read");
        assert_eq!(bytes, serde_json::to_vec(&expected).unwrap());
        assert!(!bytes.ends_with(b"\n"));
        assert!(!bytes.ends_with(b"\r\n"));
        let json = str::from_utf8(&bytes).expect("snapshot JSON should be UTF-8");
        assert_eq!(
            deserialize_validated_snapshot(json).expect("written snapshot should validate"),
            expected
        );
        assert!(snapshot_temp_files(&paths).is_empty());
        assert!(!paths.current_snapshot_backup().exists());
    }

    #[test]
    fn replaces_existing_current_and_removes_backup() {
        let root = TestRoot::uncreated("replace-current");
        let paths = root.snapshot_paths();
        let old_snapshot = snapshot("home", 1_725_000_000_100);
        let new_snapshot = snapshot("home", 1_725_000_000_200);
        write_current_snapshot(&paths, &old_snapshot).expect("initial snapshot should be written");

        write_current_snapshot(&paths, &new_snapshot)
            .expect("existing snapshot should be replaced");

        let bytes = fs::read(paths.current_snapshot()).expect("current snapshot should be read");
        let json = str::from_utf8(&bytes).expect("snapshot JSON should be UTF-8");
        assert_eq!(
            deserialize_validated_snapshot(json).expect("replaced snapshot should validate"),
            new_snapshot
        );
        assert_ne!(bytes, serde_json::to_vec(&old_snapshot).unwrap());
        assert!(snapshot_temp_files(&paths).is_empty());
        assert!(!paths.current_snapshot_backup().exists());
    }

    #[test]
    fn invalid_snapshot_does_not_touch_filesystem() {
        let root = TestRoot::uncreated("invalid-snapshot");
        let paths = root.snapshot_paths();
        let invalid = snapshot("TEST_SECRET_INVALID_MACHINE_ID", 1_725_000_000_200);

        assert_eq!(
            write_current_snapshot(&paths, &invalid),
            Err(SnapshotWriteError::InvalidSnapshot)
        );
        assert!(!root.path.exists());
        assert!(!paths.current_snapshot().exists());
        assert!(!paths.current_snapshot_backup().exists());
    }

    #[test]
    fn preexisting_backup_stops_before_temp_creation_and_preserves_files() {
        let root = TestRoot::uncreated("backup-conflict");
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        fs::write(paths.current_snapshot(), b"old-current").unwrap();
        fs::write(paths.current_snapshot_backup(), b"recovery-backup").unwrap();

        assert_eq!(
            write_current_snapshot(&paths, &snapshot("home", 1_725_000_000_200)),
            Err(SnapshotWriteError::BackupConflict)
        );
        assert_eq!(fs::read(paths.current_snapshot()).unwrap(), b"old-current");
        assert_eq!(
            fs::read(paths.current_snapshot_backup()).unwrap(),
            b"recovery-backup"
        );
        assert!(snapshot_temp_files(&paths).is_empty());
    }

    #[test]
    fn temp_names_use_required_prefix_and_unique_counters() {
        let root = TestRoot::uncreated("temp-names");
        let paths = root.snapshot_paths();
        let first_counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let second_counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let first = temp_file_path(paths.current_snapshot(), first_counter).unwrap();
        let second = temp_file_path(paths.current_snapshot(), second_counter).unwrap();
        let expected_name = format!(
            ".{CURRENT_SNAPSHOT_FILE}.{}.{first_counter}.tmp",
            std::process::id()
        );

        assert_eq!(
            first.file_name().and_then(|name| name.to_str()),
            Some(expected_name.as_str())
        );
        assert_ne!(first, second);
    }

    #[test]
    fn create_new_does_not_overwrite_existing_temp() {
        const COUNTER: u64 = 7;
        let root = TestRoot::uncreated("existing-temp");
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        let temp = temp_file_path(paths.current_snapshot(), COUNTER).unwrap();
        fs::write(&temp, b"unrelated-existing-temp").unwrap();

        assert_eq!(
            write_current_snapshot_with(
                &paths,
                &snapshot("home", 1_725_000_000_200),
                &FailingPlacement,
                || COUNTER,
            ),
            Err(SnapshotWriteError::TempCreateFailed)
        );
        assert_eq!(fs::read(&temp).unwrap(), b"unrelated-existing-temp");
        assert!(!paths.current_snapshot().exists());
    }

    #[test]
    fn placement_failure_removes_only_current_temp() {
        const COUNTER: u64 = 8;
        let root = TestRoot::uncreated("placement-failure");
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        let unrelated = paths
            .current_snapshot()
            .parent()
            .unwrap()
            .join(".current.json.unrelated.tmp");
        fs::write(&unrelated, b"keep-me").unwrap();
        let current_temp = temp_file_path(paths.current_snapshot(), COUNTER).unwrap();

        assert_eq!(
            write_current_snapshot_with(
                &paths,
                &snapshot("home", 1_725_000_000_200),
                &FailingPlacement,
                || COUNTER,
            ),
            Err(SnapshotWriteError::InitialPlacementFailed)
        );
        assert!(!current_temp.exists());
        assert_eq!(fs::read(unrelated).unwrap(), b"keep-me");
        assert!(!paths.current_snapshot().exists());
    }

    #[test]
    fn initial_placement_race_does_not_overwrite_competing_current() {
        const COUNTER: u64 = 9;
        let root = TestRoot::uncreated("initial-race");
        let paths = root.snapshot_paths();

        assert_eq!(
            write_current_snapshot_with(
                &paths,
                &snapshot("home", 1_725_000_000_200),
                &CompetingInitialPlacement,
                || COUNTER,
            ),
            Err(SnapshotWriteError::InitialPlacementFailed)
        );
        assert_eq!(
            fs::read(paths.current_snapshot()).unwrap(),
            b"competing-current"
        );
        assert!(!temp_file_path(paths.current_snapshot(), COUNTER)
            .unwrap()
            .exists());
    }

    #[test]
    fn replacement_failure_preserves_old_current_and_cleans_temp() {
        const COUNTER: u64 = 10;
        let root = TestRoot::uncreated("replacement-failure");
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        fs::write(paths.current_snapshot(), b"old-current").unwrap();

        assert_eq!(
            write_current_snapshot_with(
                &paths,
                &snapshot("home", 1_725_000_000_200),
                &FailingReplacementWithBackup,
                || COUNTER,
            ),
            Err(SnapshotWriteError::ReplacementFailed)
        );
        assert_eq!(fs::read(paths.current_snapshot()).unwrap(), b"old-current");
        assert_eq!(
            fs::read(paths.current_snapshot_backup()).unwrap(),
            b"old-current"
        );
        assert!(!temp_file_path(paths.current_snapshot(), COUNTER)
            .unwrap()
            .exists());
    }

    #[test]
    fn backup_cleanup_failure_does_not_turn_success_into_write_failure() {
        const COUNTER: u64 = 12;
        let root = TestRoot::uncreated("backup-cleanup-failure");
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        fs::write(paths.current_snapshot(), b"old-current").unwrap();
        let expected = snapshot("home", 1_725_000_000_200);

        assert_eq!(
            write_current_snapshot_with(
                &paths,
                &expected,
                &SuccessfulReplacementWithUndeletableBackup,
                || COUNTER,
            ),
            Ok(())
        );
        let json = fs::read_to_string(paths.current_snapshot()).unwrap();
        assert_eq!(deserialize_validated_snapshot(&json).unwrap(), expected);
        assert!(paths.current_snapshot_backup().is_dir());
        assert_eq!(
            fs::read(paths.current_snapshot_backup().join("old-current")).unwrap(),
            b"old-current"
        );
    }

    #[test]
    fn pcwstr_buffer_is_null_terminated_and_rejects_interior_null() {
        let wide = path_to_wide(Path::new(r"C:\snapshots\current.json")).unwrap();
        assert_eq!(wide.last(), Some(&0));
        assert!(!wide[..wide.len() - 1].contains(&0));

        let path = PathBuf::from(OsString::from_wide(&[
            b'C' as u16,
            b':' as u16,
            b'\\' as u16,
            b'a' as u16,
            0,
            b'b' as u16,
        ]));
        assert_eq!(path_to_wide(&path), Err(SnapshotWriteError::InvalidPath));
    }

    #[test]
    fn writer_rejects_interior_null_path_before_filesystem_changes() {
        let root = TestRoot {
            path: PathBuf::from(OsString::from_wide(&[
                b'C' as u16,
                b':' as u16,
                b'\\' as u16,
                b'a' as u16,
                0,
                b'b' as u16,
            ])),
        };
        let paths = root.snapshot_paths();

        assert_eq!(
            write_current_snapshot(&paths, &snapshot("home", 1_725_000_000_200)),
            Err(SnapshotWriteError::InvalidPath)
        );
        assert!(!paths.current_snapshot().exists());
        assert!(!paths.current_snapshot_backup().exists());
    }

    #[test]
    fn write_errors_do_not_expose_snapshot_path_json_or_os_error() {
        const COUNTER: u64 = 11;
        const PATH_SENTINEL: &str = "TEST_ATOMIC_PATH_SENTINEL_9b21";
        let root = TestRoot::uncreated(PATH_SENTINEL);
        let paths = root.snapshot_paths();
        let secret_snapshot = snapshot("secret-snapshot-7f3a", 1_725_000_000_200);
        let raw_json = serde_json::to_string(&secret_snapshot).unwrap();

        let error =
            write_current_snapshot_with(&paths, &secret_snapshot, &FailingPlacement, || COUNTER)
                .unwrap_err();
        assert_eq!(error, SnapshotWriteError::InitialPlacementFailed);
        let rendered = format!("{error:?}: {error}");
        assert_eq!(
            rendered,
            "InitialPlacementFailed: unable to place initial snapshot"
        );
        assert!(!rendered.contains("secret-snapshot-7f3a"));
        assert!(!rendered.contains(PATH_SENTINEL));
        assert!(!rendered.contains(&raw_json));
        assert!(!rendered.contains(root.path.to_string_lossy().as_ref()));
        assert!(std::error::Error::source(&error).is_none());
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
        assert_eq!(
            paths.current_snapshot_backup(),
            root.join(APPLICATION_DIRECTORY)
                .join(USAGE_DIRECTORY)
                .join("pc-1")
                .join("current.json.bak")
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

    fn history_entries(bytes: &[u8]) -> Vec<&str> {
        assert!(
            bytes.ends_with(b"\n"),
            "history must end with exactly one LF"
        );
        assert!(!bytes.ends_with(b"\r\n"), "history must not end with CRLF");
        let without_trailing_lf = &bytes[..bytes.len() - 1];
        let text = str::from_utf8(without_trailing_lf).expect("history should be UTF-8");
        let lines: Vec<&str> = text.split('\n').collect();
        assert!(
            lines.iter().all(|line| !line.is_empty()),
            "history must not contain blank lines"
        );
        lines
    }

    #[test]
    fn appends_first_line_to_missing_history_file() {
        let root = TestRoot::uncreated("history-missing");
        let paths = root.snapshot_paths();
        let expected = snapshot("home", 1_725_000_000_200);

        append_snapshot_to_history(&paths, &expected)
            .expect("first history entry should be written");

        let bytes = fs::read(paths.history()).expect("history file should be read");
        let lines = history_entries(&bytes);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            deserialize_validated_snapshot(lines[0]).expect("history line should validate"),
            expected
        );
    }

    #[test]
    fn appends_first_line_to_zero_byte_history_file() {
        let root = TestRoot::uncreated("history-zero-byte");
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        fs::write(paths.history(), b"").expect("zero byte history should be created");
        let expected = snapshot("home", 1_725_000_000_200);

        append_snapshot_to_history(&paths, &expected).expect("history entry should be appended");

        let bytes = fs::read(paths.history()).expect("history file should be read");
        let lines = history_entries(&bytes);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            deserialize_validated_snapshot(lines[0]).expect("history line should validate"),
            expected
        );
    }

    #[test]
    fn appends_second_line_after_existing_entry_and_preserves_order_and_bytes() {
        let root = TestRoot::uncreated("history-second-line");
        let paths = root.snapshot_paths();
        let first = snapshot("home", 1_725_000_000_100);
        let second = snapshot("home", 1_725_000_000_200);
        append_snapshot_to_history(&paths, &first).expect("first history entry should be written");
        let after_first = fs::read(paths.history()).unwrap();

        append_snapshot_to_history(&paths, &second)
            .expect("second history entry should be appended");

        let bytes = fs::read(paths.history()).expect("history file should be read");
        assert!(bytes.starts_with(&after_first));
        let lines = history_entries(&bytes);
        assert_eq!(lines.len(), 2);
        assert_eq!(deserialize_validated_snapshot(lines[0]).unwrap(), first);
        assert_eq!(deserialize_validated_snapshot(lines[1]).unwrap(), second);
    }

    #[test]
    fn invalid_snapshot_is_rejected_without_creating_directory_or_history() {
        let root = TestRoot::uncreated("history-invalid-snapshot");
        let paths = root.snapshot_paths();
        let invalid = snapshot("TEST_SECRET_INVALID_MACHINE_ID", 1_725_000_000_200);

        assert_eq!(
            append_snapshot_to_history(&paths, &invalid),
            Err(HistoryAppendError::InvalidSnapshot)
        );
        assert!(!root.path.exists());
        assert!(!paths.history().exists());
    }

    #[test]
    fn invalid_snapshot_does_not_modify_existing_history() {
        let root = TestRoot::uncreated("history-invalid-preserves");
        let paths = root.snapshot_paths();
        let valid = snapshot("home", 1_725_000_000_100);
        append_snapshot_to_history(&paths, &valid).expect("valid history entry should be written");
        let before = fs::read(paths.history()).unwrap();
        let invalid = snapshot("TEST_SECRET_INVALID_MACHINE_ID", 1_725_000_000_200);

        assert_eq!(
            append_snapshot_to_history(&paths, &invalid),
            Err(HistoryAppendError::InvalidSnapshot)
        );
        assert_eq!(fs::read(paths.history()).unwrap(), before);
    }

    #[test]
    fn nonempty_history_without_trailing_lf_is_rejected_and_left_unchanged() {
        let root = TestRoot::uncreated("history-missing-lf");
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        let existing = b"TEST_SECRET_HISTORY_TAIL_7f3a-no-newline".to_vec();
        fs::write(paths.history(), &existing).unwrap();

        assert_eq!(
            append_snapshot_to_history(&paths, &snapshot("home", 1_725_000_000_200)),
            Err(HistoryAppendError::HistoryTailInvalid)
        );
        assert_eq!(fs::read(paths.history()).unwrap(), existing);
    }

    #[test]
    fn history_ending_in_crlf_is_rejected_as_noncanonical_and_left_unchanged() {
        let root = TestRoot::uncreated("history-crlf-tail");
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        let existing = b"{\"line\":\"one\"}\r\n".to_vec();
        fs::write(paths.history(), &existing).unwrap();

        assert_eq!(
            append_snapshot_to_history(&paths, &snapshot("home", 1_725_000_000_200)),
            Err(HistoryAppendError::HistoryTailInvalid)
        );
        assert_eq!(fs::read(paths.history()).unwrap(), existing);
    }

    #[test]
    fn history_ending_in_bare_cr_is_rejected_and_left_unchanged() {
        let root = TestRoot::uncreated("history-cr-tail");
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        let existing = b"{\"line\":\"one\"}\r".to_vec();
        fs::write(paths.history(), &existing).unwrap();

        assert_eq!(
            append_snapshot_to_history(&paths, &snapshot("home", 1_725_000_000_200)),
            Err(HistoryAppendError::HistoryTailInvalid)
        );
        assert_eq!(fs::read(paths.history()).unwrap(), existing);
    }

    #[test]
    fn append_does_not_modify_current_snapshot_backup_machine_id_or_unrelated_files() {
        let root = TestRoot::uncreated("history-non-destructive");
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        fs::write(paths.current_snapshot(), b"current-untouched").unwrap();
        fs::write(paths.current_snapshot_backup(), b"backup-untouched").unwrap();
        root.write_machine_id(b"home");
        let unrelated = paths
            .current_snapshot()
            .parent()
            .unwrap()
            .join("unrelated.txt");
        fs::write(&unrelated, b"unrelated-untouched").unwrap();

        append_snapshot_to_history(&paths, &snapshot("home", 1_725_000_000_200))
            .expect("history entry should be appended");

        assert_eq!(
            fs::read(paths.current_snapshot()).unwrap(),
            b"current-untouched"
        );
        assert_eq!(
            fs::read(paths.current_snapshot_backup()).unwrap(),
            b"backup-untouched"
        );
        assert_eq!(fs::read(root.machine_id_file()).unwrap(), b"home");
        assert_eq!(fs::read(&unrelated).unwrap(), b"unrelated-untouched");
    }

    #[test]
    fn open_failure_does_not_expose_path_or_os_error() {
        const PATH_SENTINEL: &str = "TEST_HISTORY_OPEN_PATH_9b21";
        let root = TestRoot::uncreated(PATH_SENTINEL);
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        fs::create_dir(paths.history()).expect("history path should be a directory");

        let error =
            append_snapshot_to_history(&paths, &snapshot("home", 1_725_000_000_200)).unwrap_err();

        assert_eq!(error, HistoryAppendError::HistoryOpenFailed);
        let rendered = format!("{error:?}: {error}");
        assert_eq!(
            rendered,
            "HistoryOpenFailed: unable to open snapshot history"
        );
        assert!(!rendered.contains(PATH_SENTINEL));
        assert!(!rendered.contains(root.path.to_string_lossy().as_ref()));
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn tail_invalid_error_does_not_expose_existing_history_content() {
        const CONTENT_SENTINEL: &str = "TEST_SECRET_HISTORY_CONTENT_a14c";
        let root = TestRoot::uncreated("history-tail-secret");
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        let existing = format!("{CONTENT_SENTINEL}-no-newline").into_bytes();
        fs::write(paths.history(), &existing).unwrap();

        let error =
            append_snapshot_to_history(&paths, &snapshot("home", 1_725_000_000_200)).unwrap_err();

        assert_eq!(error, HistoryAppendError::HistoryTailInvalid);
        let rendered = format!("{error:?}: {error}");
        assert_eq!(
            rendered,
            "HistoryTailInvalid: snapshot history ends unexpectedly"
        );
        assert!(!rendered.contains(CONTENT_SENTINEL));
        assert!(std::error::Error::source(&error).is_none());
        assert_eq!(fs::read(paths.history()).unwrap(), existing);
    }

    #[test]
    fn invalid_snapshot_error_does_not_expose_machine_id_secret() {
        const SECRET_SENTINEL: &str = "TEST_SECRET_MACHINE_ID_d4e2";
        let root = TestRoot::uncreated("history-invalid-secret");
        let paths = root.snapshot_paths();
        let invalid = snapshot(SECRET_SENTINEL, 1_725_000_000_200);

        let error = append_snapshot_to_history(&paths, &invalid).unwrap_err();

        assert_eq!(error, HistoryAppendError::InvalidSnapshot);
        let rendered = format!("{error:?}: {error}");
        assert_eq!(rendered, "InvalidSnapshot: invalid snapshot");
        assert!(!rendered.contains(SECRET_SENTINEL));
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn persist_snapshot_reports_success_for_both_current_and_history() {
        let root = TestRoot::uncreated("persist-both-success");
        let paths = root.snapshot_paths();
        let expected = snapshot("home", 1_725_000_000_200);

        let outcome = persist_snapshot(&paths, &expected);

        assert_eq!(
            outcome,
            SnapshotPersistOutcome {
                current: PersistResult::Saved,
                history: PersistResult::Saved,
            }
        );
        let current_json = fs::read_to_string(paths.current_snapshot()).unwrap();
        assert_eq!(
            deserialize_validated_snapshot(&current_json).unwrap(),
            expected
        );
        let history_bytes = fs::read(paths.history()).unwrap();
        let lines = history_entries(&history_bytes);
        assert_eq!(lines.len(), 1);
        assert_eq!(deserialize_validated_snapshot(lines[0]).unwrap(), expected);
    }

    #[test]
    fn persist_snapshot_attempts_history_even_when_current_fails() {
        let root = TestRoot::uncreated("persist-current-fails");
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        fs::write(paths.current_snapshot_backup(), b"stale-backup").unwrap();
        let expected = snapshot("home", 1_725_000_000_200);

        let outcome = persist_snapshot(&paths, &expected);

        assert_eq!(
            outcome,
            SnapshotPersistOutcome {
                current: PersistResult::Failed,
                history: PersistResult::Saved,
            }
        );
        assert!(!paths.current_snapshot().exists());
        assert_eq!(
            fs::read(paths.current_snapshot_backup()).unwrap(),
            b"stale-backup"
        );
        let history_bytes = fs::read(paths.history()).unwrap();
        let lines = history_entries(&history_bytes);
        assert_eq!(lines.len(), 1);
        assert_eq!(deserialize_validated_snapshot(lines[0]).unwrap(), expected);
    }

    #[test]
    fn persist_snapshot_preserves_current_success_when_history_fails() {
        let root = TestRoot::uncreated("persist-history-fails");
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        let malformed_history = b"TEST_SECRET_PERSIST_HISTORY_TAIL_9b21-no-newline".to_vec();
        fs::write(paths.history(), &malformed_history).unwrap();
        let expected = snapshot("home", 1_725_000_000_200);

        let outcome = persist_snapshot(&paths, &expected);

        assert_eq!(
            outcome,
            SnapshotPersistOutcome {
                current: PersistResult::Saved,
                history: PersistResult::Failed,
            }
        );
        let current_json = fs::read_to_string(paths.current_snapshot()).unwrap();
        assert_eq!(
            deserialize_validated_snapshot(&current_json).unwrap(),
            expected
        );
        assert_eq!(fs::read(paths.history()).unwrap(), malformed_history);
    }

    #[test]
    fn persist_snapshot_reports_failure_for_both_without_panicking() {
        let root = TestRoot::uncreated("persist-both-fail");
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        fs::write(paths.current_snapshot_backup(), b"stale-backup").unwrap();
        let malformed_history = b"TEST_SECRET_PERSIST_BOTH_FAIL_a14c-no-newline".to_vec();
        fs::write(paths.history(), &malformed_history).unwrap();
        let expected = snapshot("home", 1_725_000_000_200);

        let outcome = persist_snapshot(&paths, &expected);

        assert_eq!(
            outcome,
            SnapshotPersistOutcome {
                current: PersistResult::Failed,
                history: PersistResult::Failed,
            }
        );
        assert!(!paths.current_snapshot().exists());
        assert_eq!(
            fs::read(paths.current_snapshot_backup()).unwrap(),
            b"stale-backup"
        );
        assert_eq!(fs::read(paths.history()).unwrap(), malformed_history);
    }

    #[test]
    fn persist_outcome_debug_does_not_expose_secret_snapshot_or_path() {
        const PATH_SENTINEL: &str = "TEST_PERSIST_OUTCOME_PATH_d4e2";
        const HISTORY_SENTINEL: &str = "TEST_PERSIST_OUTCOME_HISTORY_7f3a";
        let root = TestRoot::uncreated(PATH_SENTINEL);
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        fs::write(paths.current_snapshot_backup(), b"stale-backup").unwrap();
        let malformed_history = format!("{HISTORY_SENTINEL}-no-newline").into_bytes();
        fs::write(paths.history(), &malformed_history).unwrap();
        let secret_snapshot = snapshot("home", 1_725_000_000_200);

        let outcome = persist_snapshot(&paths, &secret_snapshot);

        let rendered = format!("{outcome:?}");
        assert_eq!(
            rendered,
            "SnapshotPersistOutcome { current: Failed, history: Failed }"
        );
        assert!(!rendered.contains(PATH_SENTINEL));
        assert!(!rendered.contains(HISTORY_SENTINEL));
        assert!(!rendered.contains(root.path.to_string_lossy().as_ref()));
    }

    #[test]
    fn persist_snapshot_only_touches_current_and_history_files() {
        let root = TestRoot::uncreated("persist-non-destructive");
        let paths = root.snapshot_paths();
        create_snapshot_directory(&paths);
        root.write_machine_id(b"home");
        let unrelated = paths
            .current_snapshot()
            .parent()
            .unwrap()
            .join("unrelated.txt");
        fs::write(&unrelated, b"unrelated-untouched").unwrap();

        let outcome = persist_snapshot(&paths, &snapshot("home", 1_725_000_000_200));

        assert_eq!(
            outcome,
            SnapshotPersistOutcome {
                current: PersistResult::Saved,
                history: PersistResult::Saved,
            }
        );
        assert_eq!(fs::read(root.machine_id_file()).unwrap(), b"home");
        assert_eq!(fs::read(&unrelated).unwrap(), b"unrelated-untouched");
    }
}
