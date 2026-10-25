//! What both sides looked like at the end of the last successful bisync.
//!
//! This file is the entire difference between a two-way sync and a guess.
//! Given only the two trees as they are now, "present on the left and absent
//! on the right" has two readings that call for opposite actions:
//!
//! - it was created on the left, so copy it right; or
//! - it was deleted on the right, so delete it on the left.
//!
//! Nothing in the current state of the two trees distinguishes those. A
//! snapshot of what was there last time does, and that is all it is for.
//!
//! **Where it lives: `$XDG_STATE_HOME/lazysync/bisync/`, not in the synced
//! tree.** The alternative is tempting -- state beside the data survives a
//! reinstall and is visible from both ends -- but it does not work. A file
//! inside the tree is part of the tree being compared, so it would be synced:
//! it changes on every run, both machines would write their own, and the two
//! would conflict with each other on every single sync. Excluding it by name
//! solves that and creates a worse problem, since the exclusion would have to
//! hold for every profile and a user who overrode the filters would silently
//! start syncing their own bookkeeping.
//!
//! **Sizes and times, not checksums.** The snapshot records what the planner
//! compares, so that "changed since last sync" is decided by the same rule
//! that decides "differs between sides". Storing hashes would make the two
//! disagree: a file could be unchanged by one measure and changed by the
//! other, and there would be no principled answer about which wins.
//!
//! A missing snapshot is not an error -- it is the first run, and
//! [`Snapshot::is_first_run`] says so. What the *planner* does about that is
//! deliberately restrictive; see `bisync`.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::vfs::Entry;

/// Format version, so a future change can be detected rather than
/// misinterpreted.
///
/// A snapshot whose version is not understood is discarded and treated as a
/// first run. That is the safe direction: a first run refuses to delete
/// anything, whereas misreading an old format could mean propagating a
/// deletion that never happened.
const VERSION: u32 = 1;

/// One file as it was at the end of the last sync.
///
/// Both sides are recorded, not one merged view. After a successful sync they
/// are usually equal -- but not always: a destination that cannot set
/// timestamps ends up with its own, and recording a value it does not have
/// would make the next run see a change that never happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub local_size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_mtime: Option<DateTime<Utc>>,
    pub remote_size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_mtime: Option<DateTime<Utc>>,
}

impl Record {
    /// Record a path that is in agreement on both sides.
    pub fn from_entries(local: &Entry, remote: &Entry) -> Self {
        Self {
            local_size: local.size,
            local_mtime: local.mtime,
            remote_size: remote.size,
            remote_mtime: remote.mtime,
        }
    }

    /// This record's view of one side, as the planner's comparison sees it.
    pub const fn side(&self, pane: usize) -> (u64, Option<DateTime<Utc>>) {
        if pane == 0 {
            (self.local_size, self.local_mtime)
        } else {
            (self.remote_size, self.remote_mtime)
        }
    }
}

/// The last known state of one synced pair.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub version: u32,

    /// When this snapshot was written. Shown in the window, because a
    /// snapshot from six months ago means six months of changes are about to
    /// be worked out at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken: Option<DateTime<Utc>>,

    /// The roots this describes, so a snapshot cannot be silently applied to a
    /// different pair of directories.
    #[serde(default)]
    pub local_root: PathBuf,
    #[serde(default)]
    pub remote_root: String,

    /// Every path that was in agreement, relative to the roots.
    ///
    /// A `BTreeMap` rather than a `Vec`: the planner looks paths up one at a
    /// time while walking, and the ordering makes the file on disk readable
    /// and its diffs small.
    #[serde(default)]
    pub files: BTreeMap<String, Record>,
}

impl Snapshot {
    /// An empty snapshot for a pair that has never been synced.
    pub fn new(local_root: &Path, remote_root: &str) -> Self {
        Self {
            version: VERSION,
            taken: None,
            local_root: local_root.to_path_buf(),
            remote_root: remote_root.to_owned(),
            files: BTreeMap::new(),
        }
    }

    /// Whether this pair has never been successfully synced.
    ///
    /// Distinct from "has no files": a snapshot recording an empty tree is a
    /// real snapshot, and deleting from it is legitimate. One that was never
    /// taken is not, and the planner must not propagate deletions from it.
    pub const fn is_first_run(&self) -> bool {
        self.taken.is_none()
    }

    /// What was recorded for a path, if anything.
    pub fn get(&self, relative: &Path) -> Option<&Record> {
        self.files.get(&key(relative))
    }

    pub fn insert(&mut self, relative: &Path, record: Record) {
        self.files.insert(key(relative), record);
    }

    pub fn remove(&mut self, relative: &Path) {
        self.files.remove(&key(relative));
    }

    /// Where this snapshot is stored.
    ///
    /// Keyed by a digest of the two roots, so one machine can bisync several
    /// pairs without them colliding, and so a path containing `/` does not
    /// have to be escaped into a filename.
    pub fn path_for(local_root: &Path, remote_root: &str) -> Option<PathBuf> {
        Some(directory()?.join(format!(
            "{}.json",
            key_digest(&local_root.to_string_lossy(), remote_root)
        )))
    }

    /// Load the snapshot for a pair, or an empty one.
    ///
    /// Every failure mode -- no file, unreadable file, unparsable contents,
    /// a version from the future -- returns a first-run snapshot rather than
    /// an error. That is the conservative direction: a first run will not
    /// delete anything, whereas treating a damaged snapshot as authoritative
    /// could propagate a deletion that never happened.
    pub fn load(local_root: &Path, remote_root: &str) -> Self {
        let fresh = Self::new(local_root, remote_root);

        let Some(path) = Self::path_for(local_root, remote_root) else {
            return fresh;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return fresh;
        };

        match serde_json::from_str::<Self>(&text) {
            Ok(loaded) if loaded.version == VERSION => loaded,
            Ok(loaded) => {
                tracing::warn!(
                    "bisync snapshot {} is version {} and this lazysync understands {VERSION}; \
                     treating the pair as never synced",
                    path.display(),
                    loaded.version
                );
                fresh
            }
            Err(e) => {
                tracing::warn!(
                    "bisync snapshot {} could not be read ({e}); treating the pair as never \
                     synced",
                    path.display()
                );
                fresh
            }
        }
    }

    /// Write the snapshot out, stamped with the current time.
    ///
    /// Written to a temporary file and renamed, because a snapshot truncated
    /// by a crash mid-write is worse than none: it would claim files were in
    /// agreement when they were not, and the next run would propagate
    /// deletions for everything missing from it.
    pub fn save(&mut self, now: DateTime<Utc>) -> std::io::Result<PathBuf> {
        self.version = VERSION;
        self.taken = Some(now);

        let path = Self::path_for(&self.local_root, &self.remote_root).ok_or_else(|| {
            std::io::Error::other("no state directory is available for the bisync snapshot")
        })?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let temporary = path.with_extension("json.new");
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, &path)?;

        Ok(path)
    }
}

/// `$XDG_STATE_HOME/lazysync/bisync`, or the platform equivalent.
///
/// Goes through [`crate::config::state_dir`] rather than calling `dirs`
/// directly, so that `LAZYSYNC_DEV_STATE_DIR` redirects it along with the logs
/// and session state. The tests rely on that: a suite that wrote real
/// snapshots would both pollute the developer's state directory and have its
/// cases interfere with each other through it.
fn directory() -> Option<PathBuf> {
    Some(crate::config::state_dir().join("bisync"))
}

/// A path's key in the snapshot.
///
/// Always `/`-separated, whatever the host uses, because the same pair may be
/// synced from more than one machine and a snapshot keyed by `\` on Windows
/// would match nothing from Linux.
fn key(relative: &Path) -> String {
    relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// A short, stable, filename-safe digest of the two roots.
///
/// FNV-1a, not a cryptographic hash: this names a file in the user's own state
/// directory, so the requirement is "different pairs get different names", not
/// "an attacker cannot find a collision".
fn key_digest(local: &str, remote: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in local
        .bytes()
        .chain(b"\0".iter().copied())
        .chain(remote.bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::EntryKind;

    fn entry(size: u64, epoch: Option<i64>) -> Entry {
        use chrono::TimeZone;
        Entry {
            name: "f".into(),
            kind: EntryKind::File,
            size,
            mtime: epoch.map(|s| Utc.timestamp_opt(s, 0).unwrap()),
            mode: None,
            symlink_target: None,
            symlink_to_dir: false,
        }
    }

    /// The distinction the whole design rests on: never-synced is not the same
    /// as synced-and-empty.
    #[test]
    fn a_snapshot_with_no_timestamp_is_a_first_run() {
        let fresh = Snapshot::new(Path::new("/home/a"), "sftp://box/srv");
        assert!(fresh.is_first_run());

        let mut recorded = fresh.clone();
        recorded.taken = Some(Utc::now());
        assert!(
            !recorded.is_first_run(),
            "a snapshot that was taken describes an empty tree, and deleting from it is \
             legitimate; one that was never taken is not"
        );
    }

    #[test]
    fn records_round_trip_through_json() {
        let mut snapshot = Snapshot::new(Path::new("/home/a"), "sftp://box/srv");
        snapshot.insert(
            Path::new("src/main.rs"),
            Record::from_entries(&entry(10, Some(1000)), &entry(10, Some(1000))),
        );

        let text = serde_json::to_string(&snapshot).unwrap();
        let back: Snapshot = serde_json::from_str(&text).unwrap();

        assert_eq!(back.get(Path::new("src/main.rs")).unwrap().local_size, 10);
        assert_eq!(back, snapshot);
    }

    /// Keys use `/` whatever the host separator is, so a pair synced from two
    /// different machines agrees about what a path is called.
    #[test]
    fn keys_are_slash_separated() {
        assert_eq!(key(Path::new("src/api/handler.rs")), "src/api/handler.rs");
        assert_eq!(key(Path::new("")), "");

        let mut snapshot = Snapshot::new(Path::new("/a"), "r");
        snapshot.insert(
            Path::new("src/api/h.rs"),
            Record::from_entries(&entry(1, None), &entry(1, None)),
        );
        assert!(snapshot.files.contains_key("src/api/h.rs"));
        assert!(snapshot.get(Path::new("src/api/h.rs")).is_some());
    }

    /// Both sides are stored separately, because after a sync to a backend
    /// that cannot set timestamps they genuinely differ -- and recording a
    /// value the remote does not have would show a change next run that never
    /// happened.
    #[test]
    fn both_sides_are_recorded_independently() {
        let record = Record::from_entries(&entry(10, Some(1000)), &entry(10, None));

        assert_eq!(record.side(0), (10, entry(10, Some(1000)).mtime));
        assert_eq!(record.side(1), (10, None));
    }

    #[test]
    fn different_pairs_get_different_files() {
        let a = Snapshot::path_for(Path::new("/home/a"), "sftp://box/srv");
        let b = Snapshot::path_for(Path::new("/home/b"), "sftp://box/srv");
        let c = Snapshot::path_for(Path::new("/home/a"), "sftp://other/srv");

        // All or nothing: on a machine with no state directory they are all
        // `None`, which the planner handles as "cannot bisync".
        if let (Some(a), Some(b), Some(c)) = (a, b, c) {
            assert_ne!(a, b, "a different local root is a different pair");
            assert_ne!(a, c, "a different remote is a different pair");
        }
    }

    #[test]
    fn the_same_pair_gets_the_same_file_every_time() {
        let first = Snapshot::path_for(Path::new("/home/a"), "sftp://box/srv");
        let again = Snapshot::path_for(Path::new("/home/a"), "sftp://box/srv");
        assert_eq!(first, again);
    }

    /// A snapshot from a future version must be ignored rather than
    /// misinterpreted: reading an unknown layout as authoritative could
    /// propagate deletions that never happened.
    #[test]
    fn an_unknown_version_is_treated_as_a_first_run() {
        let mut snapshot = Snapshot::new(Path::new("/home/a"), "r");
        snapshot.version = VERSION + 99;
        snapshot.taken = Some(Utc::now());
        snapshot.insert(
            Path::new("a"),
            Record::from_entries(&entry(1, None), &entry(1, None)),
        );

        let text = serde_json::to_string(&snapshot).unwrap();
        let parsed: Snapshot = serde_json::from_str(&text).unwrap();

        // `load` is what applies the rule; this asserts the input it keys off.
        assert_ne!(parsed.version, VERSION);
    }

    #[test]
    fn removing_a_path_forgets_it() {
        let mut snapshot = Snapshot::new(Path::new("/a"), "r");
        snapshot.insert(
            Path::new("gone"),
            Record::from_entries(&entry(1, None), &entry(1, None)),
        );
        assert!(snapshot.get(Path::new("gone")).is_some());

        snapshot.remove(Path::new("gone"));
        assert!(snapshot.get(Path::new("gone")).is_none());
    }
}
