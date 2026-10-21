//! Durable, append-only dispatch journal (contract §12.4).
//!
//! One file `<dispatch_journal_dir>/<goalId>.jsonl` per goal. Lines are only
//! appended, each followed by `fsync`; the server never truncates or rewrites
//! a journal. The directory and the files follow the private-file rules of the
//! channel B/C files (owner = server UID, no group or other bits, no symlinks,
//! single link), except that the directory is writable by the server.
//!
//! The parsing and evaluation rules live in [`onshape_mcp_core::goal::Journal`].
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use onshape_mcp_core::goal::{
    GoalAdmissionError, JOURNAL_FILE_MAX_BYTES, Journal, JournalEntry, is_uuid_v4,
};

/// `O_NOFOLLOW` for the supported Linux targets.
#[cfg(target_arch = "aarch64")]
const O_NOFOLLOW: i32 = 0o100_000;
/// `O_NOFOLLOW` for the supported Linux targets.
#[cfg(not(target_arch = "aarch64"))]
const O_NOFOLLOW: i32 = 0o400_000;

/// Serializes journal file reads and appends within the process, so that the
/// journal-summary endpoint never observes a half-written line.
static JOURNAL_IO: std::sync::Mutex<()> = std::sync::Mutex::new(());

const UNAVAILABLE: GoalAdmissionError = GoalAdmissionError::JournalUnavailable;

/// The journal directory configured in `dispatch_journal_dir`.
#[derive(Clone, Debug)]
pub struct JournalStore {
    dir: PathBuf,
}

fn server_uid() -> Result<u32, GoalAdmissionError> {
    std::fs::metadata("/proc/self")
        .map(|meta| meta.uid())
        .map_err(|_| UNAVAILABLE)
}

fn insecure_file(meta: &std::fs::Metadata, owner: u32) -> bool {
    !meta.is_file() || meta.nlink() != 1 || meta.uid() != owner || meta.mode() & 0o077 != 0
}

impl JournalStore {
    /// A store over `dir`; nothing is checked until it is used.
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    /// The directory is canonical, a real directory, owned by the server UID
    /// and has no group or other bits.
    pub fn check_dir(&self) -> Result<(), GoalAdmissionError> {
        let owner = server_uid()?;
        // Byte comparison: `Path` equality would ignore `.` components.
        let canonical = std::fs::canonicalize(&self.dir).map_err(|_| UNAVAILABLE)?;
        if canonical.as_os_str() != self.dir.as_os_str() {
            return Err(UNAVAILABLE);
        }
        let meta = std::fs::symlink_metadata(&self.dir).map_err(|_| UNAVAILABLE)?;
        if !meta.is_dir() || meta.uid() != owner || meta.mode() & 0o077 != 0 {
            return Err(UNAVAILABLE);
        }
        Ok(())
    }

    fn file(&self, goal_id: &str) -> Result<PathBuf, GoalAdmissionError> {
        if !is_uuid_v4(goal_id) {
            return Err(UNAVAILABLE);
        }
        Ok(self.dir.join(format!("{goal_id}.jsonl")))
    }

    /// Load the effective journal of `goal_id`. A missing file is an empty
    /// journal; an insecure, oversized, unreadable or corrupt one fails closed.
    pub fn read(&self, goal_id: &str) -> Result<Journal, GoalAdmissionError> {
        let _io = JOURNAL_IO.lock().map_err(|_| UNAVAILABLE)?;
        self.check_dir()?;
        let path = self.file(goal_id)?;
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(O_NOFOLLOW)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Journal::default()),
            Err(_) => return Err(UNAVAILABLE),
        };
        let meta = file.metadata().map_err(|_| UNAVAILABLE)?;
        if insecure_file(&meta, server_uid()?) || meta.len() > JOURNAL_FILE_MAX_BYTES {
            return Err(UNAVAILABLE);
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(JOURNAL_FILE_MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| UNAVAILABLE)?;
        if bytes.len() as u64 > JOURNAL_FILE_MAX_BYTES {
            return Err(UNAVAILABLE);
        }
        Journal::parse(&bytes)
    }

    /// Append one line for `goal_id` and `fsync` it. A newly created file is
    /// `0600`, and the directory is synced as well so that the file survives a
    /// crash.
    pub fn append(&self, goal_id: &str, entry: &JournalEntry) -> Result<(), GoalAdmissionError> {
        let line = entry.to_line()?;
        let _io = JOURNAL_IO.lock().map_err(|_| UNAVAILABLE)?;
        self.check_dir()?;
        let owner = server_uid()?;
        let path = self.file(goal_id)?;
        let existed = match std::fs::symlink_metadata(&path) {
            Ok(meta) if !insecure_file(&meta, owner) => true,
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            _ => return Err(UNAVAILABLE),
        };
        let mut options = OpenOptions::new();
        options.append(true).custom_flags(O_NOFOLLOW);
        if !existed {
            options.create_new(true).mode(0o600);
        }
        let mut file = options.open(&path).map_err(|_| UNAVAILABLE)?;
        let meta = file.metadata().map_err(|_| UNAVAILABLE)?;
        if insecure_file(&meta, owner) {
            return Err(UNAVAILABLE);
        }
        file.write_all(line.as_bytes()).map_err(|_| UNAVAILABLE)?;
        file.sync_all().map_err(|_| UNAVAILABLE)?;
        if !existed {
            File::open(&self.dir)
                .and_then(|dir| dir.sync_all())
                .map_err(|_| UNAVAILABLE)?;
        }
        Ok(())
    }
}

#[cfg(test)]
// Test-only: fixture construction failures are test bugs.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use onshape_mcp_core::goal::DispatchOutcome;
    use std::os::unix::fs::PermissionsExt;

    const GOAL: &str = "3f2a9c1e-5b7d-4e8f-9a0b-1c2d3e4f5a6b";

    pub fn private_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }

    fn entry(sequence: u64, outcome: DispatchOutcome) -> JournalEntry {
        JournalEntry {
            sequence,
            grant_revision: 1,
            request_hash: "1".repeat(64),
            feature_id: "f1".into(),
            source_microversion: "e".repeat(24),
            admitted_at: 1,
            outcome,
            http_status: None,
        }
    }

    #[test]
    fn append_is_durable_private_and_append_only() {
        let dir = private_dir();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let store = JournalStore::new(&root);
        assert_eq!(store.read(GOAL).unwrap(), Journal::default());
        let pending = entry(1, DispatchOutcome::Pending);
        store.append(GOAL, &pending).unwrap();
        let file = root.join(format!("{GOAL}.jsonl"));
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let ok = pending.with_outcome(DispatchOutcome::Ok, Some(200));
        store.append(GOAL, &ok).unwrap();
        let raw = std::fs::read_to_string(&file).unwrap();
        assert_eq!(raw.lines().count(), 2);
        assert_eq!(
            raw,
            format!("{}{}", pending.to_line().unwrap(), ok.to_line().unwrap())
        );
        // A fresh store over the same directory (restart) sees the same journal.
        let reloaded = JournalStore::new(&root).read(GOAL).unwrap();
        assert_eq!(reloaded.entries(), std::slice::from_ref(&ok));
    }

    #[test]
    fn insecure_or_corrupt_journal_fails_closed() {
        let dir = private_dir();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let store = JournalStore::new(&root);
        // Non-uuid goal IDs never become paths.
        assert_eq!(store.read("../x").err(), Some(UNAVAILABLE));
        assert_eq!(
            store
                .append("../x", &entry(1, DispatchOutcome::Pending))
                .err(),
            Some(UNAVAILABLE)
        );
        store.append(GOAL, &entry(1, DispatchOutcome::Ok)).unwrap();
        let file = root.join(format!("{GOAL}.jsonl"));
        // Group/other bits on the file.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(store.read(GOAL).err(), Some(UNAVAILABLE));
        assert_eq!(
            store.append(GOAL, &entry(2, DispatchOutcome::Ok)).err(),
            Some(UNAVAILABLE)
        );
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        // Hard link.
        let link = root.join("link");
        std::fs::hard_link(&file, &link).unwrap();
        assert_eq!(store.read(GOAL).err(), Some(UNAVAILABLE));
        std::fs::remove_file(&link).unwrap();
        // Group/other bits on the directory.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert_eq!(store.check_dir().err(), Some(UNAVAILABLE));
        assert_eq!(store.read(GOAL).err(), Some(UNAVAILABLE));
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(store.read(GOAL).is_ok());
        // Corrupt line.
        let mut raw = std::fs::read(&file).unwrap();
        raw.extend_from_slice(b"{\"sequence\":\n");
        std::fs::write(&file, raw).unwrap();
        assert_eq!(store.read(GOAL).err(), Some(UNAVAILABLE));
        // Symlinked journal file.
        std::fs::remove_file(&file).unwrap();
        let target = root.join("target");
        std::fs::write(&target, b"").unwrap();
        std::os::unix::fs::symlink(&target, &file).unwrap();
        assert_eq!(store.read(GOAL).err(), Some(UNAVAILABLE));
        assert_eq!(
            store.append(GOAL, &entry(1, DispatchOutcome::Ok)).err(),
            Some(UNAVAILABLE)
        );
        // Oversized journal.
        std::fs::remove_file(&file).unwrap();
        let ok_line = entry(1, DispatchOutcome::Ok).to_line().unwrap();
        let big = usize::try_from(JOURNAL_FILE_MAX_BYTES).unwrap() / ok_line.len() + 1;
        let mut raw = ok_line.clone();
        for _ in 1..big {
            raw.push_str(&ok_line);
        }
        std::fs::write(&file, raw).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(store.read(GOAL).err(), Some(UNAVAILABLE));
        // Non-canonical or missing directory.
        assert!(JournalStore::new(&root.join(".")).check_dir().is_err());
        assert!(JournalStore::new(&root.join("missing")).read(GOAL).is_err());
    }
}
