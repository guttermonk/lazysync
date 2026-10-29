//! Browsing and syncing through `rclone`.
//!
//! This is the route around a wall. S3, WebDAV and SMB have been unavailable
//! since phase 5 for a reason that has nothing to do with lazysync:
//! `remotefs-aws-s3` and `remotefs-webdav` still require `remotefs 0.3`, whose
//! `RemoteFs` is a *different, incompatible* trait from the 1.0 one every
//! other backend here implements, so neither can share
//! [`RemoteAdapter`](super::remote::RemoteAdapter). Waiting for upstream ports
//! would mean waiting indefinitely.
//!
//! rclone already speaks all of them, and about seventy more. So rather than
//! implementing each protocol again, this implements [`Vfs`] over the `rclone`
//! command line -- which means a bookmark can point at any remote in the
//! user's `rclone.conf` and the dual-pane browser, the planner, the
//! confirmation window and the transfer queue all work against it unchanged.
//!
//! **A subprocess per operation, not the `rcd` daemon.** The daemon would be
//! faster -- one process, a connection pool, JSON-RPC over HTTP -- and it was
//! the original plan. It is not what is here, because it needs an HTTP client
//! and a port to bind, and binding a port means deciding what happens when it
//! is taken, when the daemon dies mid-transfer, and who shuts it down if
//! lazysync is killed. A subprocess per listing is slower and has none of
//! those questions. The cost is real but bounded: one `rclone lsjson` per
//! directory entered, which is the same order as the round trips SFTP already
//! makes.
//!
//! Reads and writes do *not* pay that per-chunk. `rclone cat` and `rclone
//! rcat` stream over one process's pipe for the whole file, so a transfer is
//! one subprocess however large the file is.

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{
    Capabilities, Entry, EntryKind, ReadHandle, Vfs, VfsError, WriteHandle, WriteMetadata,
};

/// `rclone`'s exit code for "directory not found".
///
/// Distinguished because synchronized browsing treats a missing counterpart
/// directory as routine, and a backend that reported it as a hard error would
/// make every ghosted position look like a failure.
const EXIT_DIR_NOT_FOUND: i32 = 3;

/// One entry as `rclone lsjson` reports it.
#[derive(Debug, Deserialize)]
struct LsJson {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Size")]
    size: i64,
    #[serde(rename = "ModTime")]
    mod_time: Option<String>,
    #[serde(rename = "IsDir")]
    is_dir: bool,
}

impl LsJson {
    fn into_entry(self) -> Entry {
        Entry {
            name: self.name,
            kind: if self.is_dir {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
            // rclone reports -1 for an object whose size it does not know,
            // which is not the same as empty. Clamping to 0 is the honest
            // floor; the comparison then falls back to whatever else it has.
            size: self.size.max(0) as u64,
            mtime: self
                .mod_time
                .as_deref()
                .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
                .map(|t| t.with_timezone(&Utc)),
            mode: None,
            symlink_target: None,
            symlink_to_dir: false,
        }
    }
}

/// A filesystem reached through `rclone`.
pub struct RcloneFs {
    /// The remote's name in `rclone.conf`, without the colon.
    remote: String,
}

impl RcloneFs {
    /// Open a remote by name. Does not connect: there is no session to hold.
    ///
    /// The absence of a connection is a genuine difference from every other
    /// backend here, and a pleasant one -- there is nothing to time out, no
    /// host key, and no password to keep alive. What it costs is that a
    /// misconfigured remote is not discovered until the first listing.
    pub fn open(remote: impl Into<String>) -> Result<Self, VfsError> {
        if !crate::sync::external::available("rclone") {
            return Err(VfsError::Unsupported(
                "rclone is not installed, and an rclone remote needs it".into(),
            ));
        }

        let remote = remote.into();
        if remote.is_empty() {
            return Err(VfsError::Unsupported(
                "an rclone target needs a remote name, as in rclone://gdrive/Documents".into(),
            ));
        }

        Ok(Self { remote })
    }

    /// Which remotes `rclone.conf` defines.
    ///
    /// Used by the connection form, so picking a remote is a choice from a
    /// list rather than a name typed from memory.
    pub fn remotes() -> Vec<String> {
        let Ok(output) = Command::new("rclone").arg("listremotes").output() else {
            return Vec::new();
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|line| line.trim().trim_end_matches(':').to_string())
            .filter(|line| !line.is_empty())
            .collect()
    }

    /// `remote:path`, as rclone names it.
    fn spec(&self, path: &Path) -> String {
        let text = path.to_string_lossy();
        // An rclone remote has no filesystem root, so a leading slash would
        // name a different thing: `gdrive:/Documents` is not
        // `gdrive:Documents`.
        format!("{}:{}", self.remote, text.trim_start_matches('/'))
    }

    /// Run an rclone subcommand that produces no output worth streaming.
    fn run(&self, args: &[&str], context: &str) -> Result<Vec<u8>, VfsError> {
        let output = Command::new("rclone")
            .args(args)
            // rclone writes a NOTICE to stderr when there is no config file,
            // which is not an error and must not be mistaken for one.
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| VfsError::Io {
                context: context.to_owned(),
                source: e,
            })?;

        if output.status.success() {
            return Ok(output.stdout);
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(classify(output.status.code(), &stderr, context))
    }
}

/// Turn an rclone failure into the error variant the UI distinguishes.
fn classify(code: Option<i32>, stderr: &str, context: &str) -> VfsError {
    let lower = stderr.to_lowercase();

    if code == Some(EXIT_DIR_NOT_FOUND)
        || lower.contains("directory not found")
        || lower.contains("object not found")
        || lower.contains("not found")
    {
        return VfsError::NotFound(context.to_owned());
    }
    if lower.contains("permission denied") || lower.contains("403") {
        return VfsError::PermissionDenied(context.to_owned());
    }
    // An unknown remote is the mistake a user actually makes, and it is worth
    // saying plainly rather than as a protocol error: the fix is to name a
    // remote that exists, or to run `rclone config`.
    if lower.contains("didn't find section in config file")
        || lower.contains("unknown remote")
        || lower.contains("not found in config file")
    {
        return VfsError::Unsupported(format!(
            "{context}: that remote is not in rclone.conf -- `rclone listremotes` shows what is"
        ));
    }
    if lower.contains("401") || lower.contains("unauthenticated") || lower.contains("auth") {
        return VfsError::AuthFailed(last_meaningful_line(stderr));
    }

    VfsError::Protocol {
        context: context.to_owned(),
        message: last_meaningful_line(stderr),
    }
}

/// The most informative line of rclone's stderr.
///
/// rclone prefixes each line with a timestamp and a level, and the useful part
/// is usually the last ERROR or Failed line rather than the first.
fn last_meaningful_line(stderr: &str) -> String {
    stderr
        .lines()
        .rfind(|line| !line.contains("NOTICE:") && !line.trim().is_empty())
        .unwrap_or("rclone failed without saying why")
        .trim()
        .to_string()
}

impl Vfs for RcloneFs {
    fn scheme(&self) -> &str {
        "rclone"
    }

    fn display_path(&self, path: &Path) -> String {
        self.spec(path)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // `lsjson` reports ModTime for every backend that has one.
            mtime: true,
            // rclone preserves a modification time when *it* copies, but
            // exposes no way to set one on a file already there -- which is
            // what the planner means by this. Claiming otherwise would have
            // the comparator degrade incorrectly.
            set_mtime: false,
            mode: false,
            symlinks: false,
            // `rcat` writes a whole object from stdin; there is no append and
            // no ranged write, so a partial transfer restarts.
            ranged_writes: false,
            // `moveto` replaces an existing destination.
            atomic_overwrite_rename: true,
            // Pessimistic on purpose: the real granularity depends on which
            // of seventy backends is behind the remote, and S3 in particular
            // rounds. A claim finer than the truth makes every sync re-copy.
            mtime_granularity_secs: 1,
        }
    }

    fn list(&mut self, path: &Path) -> Result<Vec<Entry>, VfsError> {
        let spec = self.spec(path);
        let context = format!("listing {spec}");
        let stdout = self.run(&["lsjson", &spec], &context)?;

        let entries: Vec<LsJson> =
            serde_json::from_slice(&stdout).map_err(|e| VfsError::Protocol {
                context,
                message: format!("rclone's listing could not be parsed: {e}"),
            })?;

        Ok(entries.into_iter().map(LsJson::into_entry).collect())
    }

    fn is_dir(&mut self, path: &Path) -> bool {
        self.stat(path).is_ok_and(|entry| entry.is_dir_like())
    }

    fn create_dir(&mut self, path: &Path) -> Result<(), VfsError> {
        let spec = self.spec(path);
        // `mkdir` creates parents and is happy if it already exists, so the
        // walk every other backend needs is unnecessary here.
        self.run(&["mkdir", &spec], &format!("creating {spec}"))
            .map(|_| ())
    }

    fn stat(&mut self, path: &Path) -> Result<Entry, VfsError> {
        let spec = self.spec(path);
        let context = format!("stat {spec}");
        let stdout = self.run(&["lsjson", "--stat", &spec], &context)?;

        // `--stat` on a missing path can succeed with a null body rather than
        // failing, so an empty result is a not-found rather than a parse
        // error -- and synchronized browsing depends on telling those apart.
        let trimmed = stdout.trim_ascii();
        if trimmed.is_empty() || trimmed == b"null" {
            return Err(VfsError::NotFound(context));
        }

        let entry: LsJson = serde_json::from_slice(trimmed).map_err(|e| VfsError::Protocol {
            context,
            message: format!("rclone's stat could not be parsed: {e}"),
        })?;
        Ok(entry.into_entry())
    }

    fn read_into(&mut self, path: &Path, dest: &mut (dyn Write + Send)) -> Result<u64, VfsError> {
        let mut handle = self.open_read(path, 0)?;
        let copied = std::io::copy(&mut handle, dest).map_err(|e| VfsError::Io {
            context: format!("reading {}", self.spec(path)),
            source: e,
        })?;
        handle.finish()?;
        Ok(copied)
    }

    fn open_read(&mut self, path: &Path, offset: u64) -> Result<Box<dyn ReadHandle>, VfsError> {
        let spec = self.spec(path);
        let mut command = Command::new("rclone");
        command.arg("cat");
        if offset > 0 {
            command.arg("--offset").arg(offset.to_string());
        }
        command.arg(&spec);

        let child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| VfsError::Io {
                context: format!("reading {spec}"),
                source: e,
            })?;

        Ok(Box::new(RcloneRead { child, spec }))
    }

    fn open_write(
        &mut self,
        path: &Path,
        _metadata: &WriteMetadata,
        append: bool,
    ) -> Result<Box<dyn WriteHandle>, VfsError> {
        if append {
            // Said rather than silently truncating: resume is off for this
            // backend (`ranged_writes: false`), so nothing should ask -- and
            // if something does, losing the existing bytes would be worse
            // than failing.
            return Err(VfsError::Unsupported(
                "rclone writes a whole object at a time; there is no append".into(),
            ));
        }

        let spec = self.spec(path);
        let child = Command::new("rclone")
            .arg("rcat")
            .arg(&spec)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| VfsError::Io {
                context: format!("writing {spec}"),
                source: e,
            })?;

        Ok(Box::new(RcloneWrite {
            child: Some(child),
            spec,
        }))
    }

    fn write_from(
        &mut self,
        path: &Path,
        metadata: &WriteMetadata,
        src: &mut (dyn Read + Send),
    ) -> Result<u64, VfsError> {
        let mut handle = self.open_write(path, metadata, false)?;
        let copied = std::io::copy(src, &mut handle).map_err(|e| VfsError::Io {
            context: format!("writing {}", self.spec(path)),
            source: e,
        })?;
        handle.finish()?;
        Ok(copied)
    }

    fn set_times(&mut self, path: &Path, _metadata: &WriteMetadata) -> Result<(), VfsError> {
        Err(VfsError::Unsupported(format!(
            "rclone cannot set a modification time on {}",
            self.spec(path)
        )))
    }

    fn set_mode(&mut self, path: &Path, _mode: u32) -> Result<(), VfsError> {
        // An rclone remote is whatever backend it fronts, and most of them --
        // object stores above all -- have no Unix permission bits at all.
        // There is no `rclone chmod`, so there is nothing to attempt.
        Err(VfsError::Unsupported(format!(
            "rclone remotes have no permission bits ({})",
            self.spec(path)
        )))
    }

    fn remove_file(&mut self, path: &Path) -> Result<(), VfsError> {
        let spec = self.spec(path);
        self.run(&["deletefile", &spec], &format!("deleting {spec}"))
            .map(|_| ())
    }

    fn remove_dir_all(&mut self, path: &Path) -> Result<(), VfsError> {
        let spec = self.spec(path);
        self.run(&["purge", &spec], &format!("deleting {spec}"))
            .map(|_| ())
    }

    fn rename(&mut self, from: &Path, to: &Path) -> Result<(), VfsError> {
        let (from_spec, to_spec) = (self.spec(from), self.spec(to));

        // `--ignore-times` is **not** optional here, and the reason is nastier
        // than it looks.
        //
        // `rclone moveto` applies its own size-and-mtime check before moving.
        // When it decides the two are already equal it skips the transfer --
        // and then deletes the source anyway, because that is what a move
        // does. Exit code 0. Measured: moving a 3-byte file onto a different
        // 3-byte file destroyed the source and left the destination's old
        // contents in place, reporting success.
        //
        // The executor renames a completed partial file into place, so the two
        // are *routinely* the same size: that is what a finished download
        // looks like. Without this flag a transfer would report success,
        // delete the bytes it just fetched, and leave the stale file -- which
        // is the worst outcome in the whole program.
        self.run(
            &["moveto", "--ignore-times", &from_spec, &to_spec],
            &format!("renaming {from_spec} to {to_spec}"),
        )
        .map(|_| ())
    }

    fn external_spec(&self, path: &Path) -> Option<crate::sync::external::Spec> {
        Some(crate::sync::external::Spec::Rclone {
            remote: self.remote.clone(),
            path: path.to_path_buf(),
        })
    }

    fn home(&mut self) -> Option<PathBuf> {
        // A remote's root is the only thing it has that resembles a home.
        Some(PathBuf::from("/"))
    }

    fn root(&self) -> PathBuf {
        PathBuf::from("/")
    }
}

/// `rclone cat`, streaming.
struct RcloneRead {
    child: Child,
    spec: String,
}

impl Read for RcloneRead {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self.child.stdout.as_mut() {
            Some(stdout) => stdout.read(buffer),
            None => Ok(0),
        }
    }
}

impl ReadHandle for RcloneRead {
    fn finish(mut self: Box<Self>) -> Result<(), VfsError> {
        // The exit status is the only place a mid-stream failure shows up: a
        // read that returns 0 bytes looks exactly like a complete file, so
        // without this a truncated download would be reported as a success.
        let status = self.child.wait().map_err(|e| VfsError::Io {
            context: format!("reading {}", self.spec),
            source: e,
        })?;

        if status.success() {
            return Ok(());
        }

        let stderr = self
            .child
            .stderr
            .as_mut()
            .map(|err| {
                let mut text = String::new();
                let _ = err.read_to_string(&mut text);
                text
            })
            .unwrap_or_default();

        Err(classify(
            status.code(),
            &stderr,
            &format!("reading {}", self.spec),
        ))
    }
}

/// `rclone rcat`, streaming.
struct RcloneWrite {
    child: Option<Child>,
    spec: String,
}

impl Write for RcloneWrite {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        match self.child.as_mut().and_then(|c| c.stdin.as_mut()) {
            Some(stdin) => stdin.write(buffer),
            None => Err(std::io::Error::other("the rclone process is gone")),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.child.as_mut().and_then(|c| c.stdin.as_mut()) {
            Some(stdin) => stdin.flush(),
            None => Ok(()),
        }
    }
}

impl WriteHandle for RcloneWrite {
    fn finish(mut self: Box<Self>) -> Result<(), VfsError> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };

        // Dropping stdin is what tells rcat the object is complete. Without
        // this it waits for more input for ever and the transfer hangs.
        drop(child.stdin.take());

        let status = child.wait().map_err(|e| VfsError::Io {
            context: format!("writing {}", self.spec),
            source: e,
        })?;

        if status.success() {
            return Ok(());
        }

        let stderr = child
            .stderr
            .as_mut()
            .map(|err| {
                let mut text = String::new();
                let _ = err.read_to_string(&mut text);
                text
            })
            .unwrap_or_default();

        Err(classify(
            status.code(),
            &stderr,
            &format!("writing {}", self.spec),
        ))
    }
}

impl Drop for RcloneWrite {
    fn drop(&mut self) {
        // A handle dropped rather than finished means the transfer was
        // abandoned. Killing the process stops a half-written object being
        // committed, which `rcat` would otherwise do on clean stdin close.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs() -> RcloneFs {
        RcloneFs {
            remote: "gdrive".into(),
        }
    }

    /// An rclone remote has no filesystem root, so the leading slash lazysync
    /// carries internally has to come off: `gdrive:/Documents` names a
    /// different thing from `gdrive:Documents`.
    #[test]
    fn a_spec_drops_the_leading_slash() {
        assert_eq!(
            fs().spec(Path::new("/Documents/a.txt")),
            "gdrive:Documents/a.txt"
        );
        assert_eq!(fs().spec(Path::new("/")), "gdrive:");
    }

    #[test]
    fn an_empty_remote_name_is_refused() {
        // Only reachable when rclone is installed; the availability check
        // fires first otherwise.
        if crate::sync::external::available("rclone") {
            assert!(matches!(RcloneFs::open(""), Err(VfsError::Unsupported(_))));
        }
    }

    #[test]
    fn a_listing_entry_becomes_an_entry() {
        let json = br#"[{"Path":"a.txt","Name":"a.txt","Size":11,
            "ModTime":"2026-10-25T14:00:38.482137128-05:00","IsDir":false},
            {"Path":"sub","Name":"sub","Size":4096,
            "ModTime":"2026-10-25T14:00:38Z","IsDir":true}]"#;

        let parsed: Vec<LsJson> = serde_json::from_slice(json).expect("rclone's own format");
        let entries: Vec<Entry> = parsed.into_iter().map(LsJson::into_entry).collect();

        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[0].kind, EntryKind::File);
        assert_eq!(entries[0].size, 11);
        assert!(entries[0].mtime.is_some(), "the planner needs a timestamp");

        assert_eq!(entries[1].kind, EntryKind::Dir);
        assert!(entries[1].is_dir_like());
    }

    /// rclone reports -1 for an object whose size it cannot determine. That is
    /// not "empty", and a negative cast into `u64` would become enormous.
    #[test]
    fn an_unknown_size_clamps_to_zero_rather_than_wrapping() {
        let json = br#"[{"Path":"x","Name":"x","Size":-1,"ModTime":null,"IsDir":false}]"#;
        let parsed: Vec<LsJson> = serde_json::from_slice(json).unwrap();
        let entry = parsed.into_iter().next().unwrap().into_entry();

        assert_eq!(
            entry.size, 0,
            "a -1 cast straight to u64 would be 18 exabytes"
        );
        assert!(entry.mtime.is_none());
    }

    /// Synchronized browsing depends on a missing directory reading as
    /// not-found rather than as a failure, so the ghosting path is taken.
    #[test]
    fn a_missing_directory_is_classified_as_not_found() {
        let error = classify(
            Some(EXIT_DIR_NOT_FOUND),
            "2026/10/25 ERROR : directory not found",
            "listing gdrive:nope",
        );
        assert!(error.is_not_found(), "{error:?}");
    }

    /// The mistake users actually make. Worth its own message, because the fix
    /// is to name a remote that exists rather than to debug a protocol error.
    #[test]
    fn an_unknown_remote_says_so_plainly() {
        let error = classify(
            Some(1),
            "2026/10/25 Failed to create file system for \"nope:\": didn't find section in config file",
            "listing nope:",
        );
        match error {
            VfsError::Unsupported(message) => {
                assert!(message.contains("rclone.conf"), "{message}");
                assert!(message.contains("listremotes"), "{message}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn permission_problems_keep_their_own_variant() {
        let error = classify(Some(1), "ERROR : 403 Forbidden", "listing gdrive:x");
        assert!(matches!(error, VfsError::PermissionDenied(_)), "{error:?}");
    }

    /// rclone writes a NOTICE about a missing config file on every invocation
    /// when there is none. Reporting that as the failure would bury the real
    /// one.
    #[test]
    fn the_config_notice_is_not_mistaken_for_an_error() {
        let stderr = "2026/10/25 NOTICE: Config file not found - using defaults\n\
                      2026/10/25 ERROR : the actual problem";
        assert_eq!(
            last_meaningful_line(stderr),
            "2026/10/25 ERROR : the actual problem"
        );
    }

    #[test]
    fn a_silent_failure_still_produces_a_message() {
        assert!(!last_meaningful_line("").is_empty());
        assert!(!last_meaningful_line("   \n  \n").is_empty());
    }

    /// Capabilities drive the comparison rules, and the two that matter here
    /// are both "no": claiming either would make every sync re-copy its tree.
    #[test]
    fn capabilities_are_pessimistic_where_rclone_cannot_promise() {
        let caps = fs().capabilities();

        assert!(caps.mtime, "lsjson reports a modification time");
        assert!(
            !caps.set_mtime,
            "rclone cannot set a time on a file already there, and claiming it would make the \
             comparator trust timestamps it cannot preserve"
        );
        assert!(
            !caps.ranged_writes,
            "rcat writes a whole object; resume would silently restart"
        );
        assert!(caps.mtime_granularity_secs >= 1);
    }

    /// Resume is off for this backend, so nothing should ask for an append --
    /// but if it does, failing is safer than truncating what is there.
    #[test]
    fn an_append_is_refused_rather_than_truncating() {
        if !crate::sync::external::available("rclone") {
            return;
        }
        let mut fs = fs();
        let result = fs.open_write(Path::new("/a.txt"), &WriteMetadata::default(), true);
        assert!(matches!(result, Err(VfsError::Unsupported(_))));
    }

    #[test]
    fn an_rclone_backend_can_be_named_to_an_external_tool() {
        let spec = fs().external_spec(Path::new("/Documents")).expect("a spec");
        assert_eq!(spec.argument(), "gdrive:Documents");
        assert!(spec.is_delegable(), "there is no password to prompt for");
    }
}
