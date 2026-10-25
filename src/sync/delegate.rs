//! Handing the transfer to `rsync` or `rclone`.
//!
//! **Delegate the bytes, not the decisions.** Both tools are perfectly happy
//! to walk two trees and work out what to copy, and letting them would destroy
//! the one property this project is built around: that the plan you reviewed
//! is what happens. So neither is ever allowed to decide anything. Each is
//! given an explicit list of paths on stdin and the flags that stop it
//! thinking:
//!
//! - `--files-from=-` makes rsync copy exactly the listed paths. It also
//!   switches off recursion, so a directory in the list does not drag its
//!   contents along.
//! - `--ignore-times` stops rsync applying its *own* size-and-mtime check and
//!   skipping a file lazysync decided had changed. This one is not theoretical:
//!   without it, a row shown in the window as an overwrite is silently not
//!   transferred whenever rsync's quick check disagrees with our comparator --
//!   which it does as soon as the tolerances differ.
//! - `rclone copy --files-from -` with `--no-traverse` is the same idea.
//!
//! **Deletions are never delegated.** `rsync --delete` re-derives what is
//! extraneous from the trees as it finds them, which is exactly the judgement
//! the plan already made and the user already reviewed. And neither tool has
//! an equivalent of lazysync's backup-directory policy that could be mapped
//! onto it faithfully. So a delegated run moves the bytes and the native
//! executor performs the deletions, which also means `safety`'s refusals keep
//! applying unchanged.
//!
//! **Progress comes off the tool's stdout on a reader thread.** A thread is
//! fine here, unlike for the native queue: what it owns is a pipe, not a
//! `Vfs`, so nothing is taken away from the browser while a transfer runs.

use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
};

use super::{
    external::{self, Spec},
    plan::{Action, Plan, Row},
};
use crate::config::Executor;

/// Which external tool to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Rsync,
    Rclone,
}

impl Tool {
    pub const fn program(self) -> &'static str {
        match self {
            Self::Rsync => "rsync",
            Self::Rclone => "rclone",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Rsync => "rsync",
            Self::Rclone => "rclone",
        }
    }
}

/// Why a tool cannot be used for a particular plan.
///
/// Carried rather than collapsed into a bool, because `executor = "rsync"`
/// has to say *why* it fell back. "rsync is not installed" and "rsync cannot
/// address an FTP server" send you to different places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unusable {
    NotInstalled(&'static str),
    /// One side cannot be named on a command line at all.
    CannotAddress {
        side: &'static str,
        scheme: String,
    },
    /// Both sides are reachable, but one needs a password lazysync would have
    /// to type into a subprocess.
    NeedsPassword,
    /// rsync speaks local paths and SSH. Anything else is rclone's job.
    UnsupportedPair,
    /// Nothing to transfer, so there is nothing to delegate.
    NothingToTransfer,
    /// The plan contains a conflict resolved "keep both".
    ///
    /// Refused rather than traded away. Preserving the displaced copy happens
    /// in lazysync's own write path, which a delegated transfer never enters
    /// -- so the tool would overwrite the loser and the copy the user chose
    /// explicitly to keep would simply be gone. Everything else delegation
    /// gives up is a matter of degree; this one is silent data loss.
    WouldLoseAKeptCopy,
}

impl std::fmt::Display for Unusable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled(program) => write!(f, "{program} is not installed"),
            Self::CannotAddress { side, scheme } => write!(
                f,
                "the {side} side is {scheme}, which no external tool can be pointed at"
            ),
            Self::NeedsPassword => write!(
                f,
                "the connection uses a typed password, and a subprocess would have to ask for \
                 it again over the display"
            ),
            Self::UnsupportedPair => {
                write!(f, "rsync handles local and ssh paths only")
            }
            Self::NothingToTransfer => write!(f, "there are no files to transfer"),
            Self::WouldLoseAKeptCopy => write!(
                f,
                "a conflict is resolved \"keep both\", and only the native engine preserves \
                 the displaced copy"
            ),
        }
    }
}

/// What the plan's two sides look like to an external tool.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub local: Option<Spec>,
    pub remote: Option<Spec>,
}

impl Endpoints {
    fn side(&self, pane: usize) -> Option<&Spec> {
        if pane == 0 {
            self.local.as_ref()
        } else {
            self.remote.as_ref()
        }
    }
}

/// Decide which executor will actually run, and why.
///
/// `Auto` degrades quietly to the native engine; a named tool that cannot run
/// reports the reason, because silently doing something other than what the
/// profile asked for is how a bandwidth-limited link ends up saturated.
pub fn choose(
    preference: Executor,
    plan: &Plan,
    endpoints: &Endpoints,
) -> (Option<Tool>, Option<Unusable>) {
    match preference {
        Executor::Native => (None, None),
        Executor::Rsync => match usable(Tool::Rsync, plan, endpoints) {
            Ok(()) => (Some(Tool::Rsync), None),
            Err(why) => (None, Some(why)),
        },
        Executor::Rclone => match usable(Tool::Rclone, plan, endpoints) {
            Ok(()) => (Some(Tool::Rclone), None),
            Err(why) => (None, Some(why)),
        },
        // **`auto` means the native engine.** This is not what the name
        // suggests and it was not the original intent, so it is worth being
        // plain about why it changed.
        //
        // Delegating gives up three things that only lazysync's own write
        // path provides. The destination is re-checked immediately before it
        // is written, so a file that changed after the plan was built is
        // skipped rather than clobbered -- a tool knows nothing of the plan
        // and overwrites it. Progress and cancellation are per file rather
        // than one aggregate line. And a conflict resolved "keep both" has
        // its displaced copy preserved, which a tool does not do.
        //
        // Those are real trades, and a user who wants rsync's delta transfer
        // may well accept them. What is not defensible is making them
        // *silently*, on a setting whose name promises only "the best
        // available" -- the first two were measured as live test failures the
        // moment auto started delegating. So `auto` is the engine that keeps
        // every guarantee, and a tool is used when it is named.
        Executor::Auto => (None, None),
    }
}

/// Whether `tool` can run this plan between these endpoints.
pub fn usable(tool: Tool, plan: &Plan, endpoints: &Endpoints) -> Result<(), Unusable> {
    if !external::available(tool.program()) {
        return Err(Unusable::NotInstalled(tool.program()));
    }
    suits(tool, plan, endpoints)
}

/// Whether `tool` *could* run this plan, setting aside whether it is
/// installed.
///
/// Split out from [`usable`] because the two are different questions and
/// conflating them made the tests lie: every check below was unreachable
/// wherever the tool happened to be absent, so a suite that passed in the dev
/// shell failed inside the Nix sandbox -- which has no rsync on `PATH` --
/// reporting `NotInstalled` for seven cases that were testing the logic here.
pub fn suits(tool: Tool, plan: &Plan, endpoints: &Endpoints) -> Result<(), Unusable> {
    // A delegated run only moves bytes. With nothing to move there is nothing
    // to delegate, and spawning a tool to copy no files would be pure
    // overhead.
    if transfers(plan).next().is_none() {
        return Err(Unusable::NothingToTransfer);
    }

    // Keep-both is refused even when a tool is asked for by name. The
    // displaced copy is preserved by lazysync's write path, so a delegated
    // transfer would overwrite it -- losing the file the user went out of
    // their way to keep. That is not a trade to offer; it is a bug.
    if plan.rows.iter().any(|row| {
        row.included && matches!(row.resolution, Some(super::plan::Resolution::KeepBoth))
    }) {
        return Err(Unusable::WouldLoseAKeptCopy);
    }

    // Every transferring row has to point the same way. A bisync plan moves
    // files in both directions, and one invocation of either tool copies in
    // one direction only -- so a two-way plan falls to the native engine
    // rather than being split into two subprocesses whose failures would be
    // reported as one.
    let mut panes = transfers(plan).map(super::transfer::written_pane);
    let first = panes.next().ok_or(Unusable::NothingToTransfer)?;
    if panes.any(|pane| pane != first) {
        return Err(Unusable::UnsupportedPair);
    }

    let source = endpoints
        .side(1 - first)
        .ok_or_else(|| Unusable::CannotAddress {
            side: "source",
            scheme: "not addressable".into(),
        })?;
    let dest = endpoints
        .side(first)
        .ok_or_else(|| Unusable::CannotAddress {
            side: "destination",
            scheme: "not addressable".into(),
        })?;

    if !source.is_delegable() || !dest.is_delegable() {
        return Err(Unusable::NeedsPassword);
    }

    match tool {
        // rsync speaks local paths and SSH, and exactly one end may be
        // remote: rsync refuses a remote-to-remote copy itself.
        Tool::Rsync => {
            let both_known =
                (source.is_local() || source.is_ssh()) && (dest.is_local() || dest.is_ssh());
            if !both_known || (source.is_ssh() && dest.is_ssh()) {
                return Err(Unusable::UnsupportedPair);
            }
            Ok(())
        }
        // rclone will copy between any two things it can name, including two
        // remotes.
        Tool::Rclone => Ok(()),
    }
}

/// The rows a tool would move: creates, overwrites, and settled conflicts.
fn transfers(plan: &Plan) -> impl Iterator<Item = &Row> {
    plan.rows.iter().filter(|row| {
        row.included
            && row.is_effective()
            && matches!(
                row.action,
                Action::Create | Action::Overwrite | Action::Conflict
            )
    })
}

/// A tool running, with its output being read.
pub struct Delegated {
    tool: Tool,
    child: Child,
    /// Lines of the tool's output, from the reader thread.
    output: Receiver<String>,
    /// Everything the tool has said, kept for the report when it fails.
    transcript: Vec<String>,
    /// Paths handed to it, so the report can name what was in flight.
    paths: Vec<PathBuf>,
    finished: bool,
    /// Most recent progress line, for the UI.
    last_progress: Option<String>,
}

impl Delegated {
    /// Spawn `tool` to copy the plan's transferring rows.
    ///
    /// The path list goes in on stdin rather than as arguments: a plan can
    /// easily have more paths than a command line can hold, and `execve`'s
    /// limit is the kind of thing that works in testing and fails on a real
    /// tree.
    pub fn start(
        tool: Tool,
        plan: &Plan,
        endpoints: &Endpoints,
        extra_args: &[String],
    ) -> std::io::Result<Self> {
        let rows: Vec<&Row> = transfers(plan).collect();
        let paths: Vec<PathBuf> = rows.iter().map(|row| row.path.clone()).collect();

        let target_pane = rows
            .first()
            .map(|row| super::transfer::written_pane(row))
            .unwrap_or(1);

        let source = endpoints
            .side(1 - target_pane)
            .ok_or_else(|| std::io::Error::other("the source side cannot be addressed"))?;
        let dest = endpoints
            .side(target_pane)
            .ok_or_else(|| std::io::Error::other("the destination side cannot be addressed"))?;

        let mut command = Command::new(tool.program());
        match tool {
            Tool::Rsync => {
                command.args([
                    "--archive",
                    // Keep a partial file on failure, which is what makes a
                    // retry cheap -- and matches what the native queue does.
                    "--partial",
                    // The two flags that make rsync a transport rather than a
                    // planner. See the module docs.
                    "--files-from=-",
                    "--from0",
                    "--ignore-times",
                    // Machine-readable totals rather than a line per file.
                    "--info=progress2",
                    // Without this rsync builds its file list incrementally
                    // and the totals in `progress2` climb as it goes, so the
                    // percentage runs backwards.
                    "--no-inc-recursive",
                ]);
                if let Some(transport) = source.ssh_transport().or_else(|| dest.ssh_transport()) {
                    command.arg("-e").arg(transport);
                }
                command.args(extra_args);
                // A trailing slash on the source means "the contents of",
                // which is what a sync root is.
                command.arg(format!("{}/", source.argument()));
                command.arg(dest.argument());
            }
            Tool::Rclone => {
                command.args([
                    "copy",
                    "--files-from",
                    "-",
                    // rclone would otherwise list the whole destination to
                    // decide what is missing; the plan already knows.
                    "--no-traverse",
                    // Transfer every listed file rather than re-checking it,
                    // for the same reason as rsync's --ignore-times.
                    "--ignore-times",
                    "--stats-one-line",
                    "--stats=500ms",
                    "--use-json-log",
                ]);
                command.args(extra_args);
                command.arg(source.argument());
                command.arg(dest.argument());
            }
        }

        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Write the list and close stdin, or the tool waits for more.
        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| std::io::Error::other("no stdin on the spawned tool"))?;
            for path in &paths {
                let text = path.to_string_lossy();
                stdin.write_all(text.as_bytes())?;
                // rsync was told `--from0`; rclone wants newlines.
                stdin.write_all(if matches!(tool, Tool::Rsync) {
                    b"\0"
                } else {
                    b"\n"
                })?;
            }
            stdin.flush()?;
        }

        // One thread per stream, merging into one channel. Reading them
        // in-line would deadlock: a tool that fills the pipe we are not
        // reading blocks for ever while we wait on the other.
        let (sender, output) = mpsc::channel();
        for stream in [
            child.stdout.take().map(Readable::Out),
            child.stderr.take().map(Readable::Err),
        ]
        .into_iter()
        .flatten()
        {
            let sender = sender.clone();
            std::thread::spawn(move || {
                let reader: Box<dyn std::io::Read + Send> = match stream {
                    Readable::Out(out) => Box::new(out),
                    Readable::Err(err) => Box::new(err),
                };
                // rsync writes progress with `\r`, not `\n`, so splitting on
                // lines alone would deliver one enormous line at the end.
                let mut reader = BufReader::new(reader);
                let mut buffer = Vec::new();
                loop {
                    buffer.clear();
                    match read_record(&mut reader, &mut buffer) {
                        Ok(0) => break,
                        Ok(_) => {
                            let text = String::from_utf8_lossy(&buffer).trim().to_string();
                            if !text.is_empty() && sender.send(text).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        Ok(Self {
            tool,
            child,
            output,
            transcript: Vec::new(),
            paths,
            finished: false,
            last_progress: None,
        })
    }

    pub const fn tool(&self) -> Tool {
        self.tool
    }

    pub const fn is_finished(&self) -> bool {
        self.finished
    }

    /// The most recent thing the tool said about its progress.
    pub fn progress(&self) -> Option<&str> {
        self.last_progress.as_deref()
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Drain whatever output is waiting and check whether the tool has exited.
    ///
    /// Returns `true` once it is finished. Never blocks, so the caller can
    /// keep drawing.
    pub fn step(&mut self) -> bool {
        if self.finished {
            return true;
        }

        loop {
            match self.output.try_recv() {
                Ok(line) => {
                    if looks_like_progress(&line) {
                        self.last_progress = Some(line.clone());
                    }
                    // Bounded: a tool that logs per file over a large tree
                    // would otherwise grow this without limit, and only the
                    // tail is any use in a failure report.
                    if self.transcript.len() < 200 {
                        self.transcript.push(line);
                    }
                }
                Err(TryRecvError::Empty) => break,
                // Both readers are gone, which means both streams closed.
                Err(TryRecvError::Disconnected) => break,
            }
        }

        match self.child.try_wait() {
            Ok(Some(_)) => {
                // Drain anything the readers queued between the last drain and
                // the exit, or a failure's final message is lost.
                while let Ok(line) = self.output.try_recv() {
                    if self.transcript.len() < 200 {
                        self.transcript.push(line);
                    }
                }
                self.finished = true;
                true
            }
            Ok(None) => false,
            Err(e) => {
                self.transcript
                    .push(format!("could not wait for the tool: {e}"));
                self.finished = true;
                true
            }
        }
    }

    /// Stop the tool. Used when the user cancels.
    pub fn cancel(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.finished = true;
    }

    /// How it went, once finished.
    pub fn outcome(&mut self) -> Result<(), String> {
        match self.child.try_wait() {
            Ok(Some(status)) if status.success() => Ok(()),
            Ok(Some(status)) => {
                let tail: Vec<&str> = self
                    .transcript
                    .iter()
                    .rev()
                    .take(5)
                    .map(String::as_str)
                    .collect();
                Err(format!(
                    "{} exited with {}: {}",
                    self.tool.label(),
                    status
                        .code()
                        .map_or_else(|| "a signal".to_string(), |c| c.to_string()),
                    tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
                ))
            }
            Ok(None) => Err(format!("{} is still running", self.tool.label())),
            Err(e) => Err(format!("could not wait for {}: {e}", self.tool.label())),
        }
    }
}

enum Readable {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

/// Read up to the next `\n` or `\r`, whichever comes first.
///
/// rsync separates progress updates with `\r` so they overwrite each other on
/// a terminal. Reading by line would buffer the whole run into one record and
/// deliver it after the transfer had finished, which is no use for a progress
/// bar.
fn read_record<R: BufRead>(reader: &mut R, buffer: &mut Vec<u8>) -> std::io::Result<usize> {
    let mut total = 0;
    loop {
        let available = match reader.fill_buf() {
            Ok(bytes) => bytes,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if available.is_empty() {
            return Ok(total);
        }

        match available.iter().position(|b| *b == b'\n' || *b == b'\r') {
            Some(at) => {
                buffer.extend_from_slice(&available[..at]);
                reader.consume(at + 1);
                return Ok(total + at + 1);
            }
            None => {
                let len = available.len();
                buffer.extend_from_slice(available);
                reader.consume(len);
                total += len;
            }
        }
    }
}

/// Whether a line is a progress update rather than a log message.
fn looks_like_progress(line: &str) -> bool {
    // rsync's progress2: "  1,234,567  45%   1.23MB/s    0:00:12".
    // rclone's one-line stats: "Transferred: 1.234 MiB / 2 MiB, 62%, ...".
    line.contains('%') && (line.contains("/s") || line.contains("Transferred"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        sync::plan::{self, Side},
        vfs::{Entry, EntryKind},
    };

    fn entry(size: u64) -> Entry {
        Entry {
            name: "f".into(),
            kind: EntryKind::File,
            size,
            mtime: None,
            mode: None,
            symlink_target: None,
            symlink_to_dir: false,
        }
    }

    fn plan_with(rows: Vec<Row>) -> Plan {
        Plan {
            rows,
            ..Plan::default()
        }
    }

    fn push_plan() -> Plan {
        plan_with(vec![plan::create_row(
            "a.txt".into(),
            &entry(10),
            crate::sync::Method::SizeMtime,
            1,
        )])
    }

    fn local_pair() -> Endpoints {
        Endpoints {
            local: Some(Spec::Local("/home/greg/proj".into())),
            remote: Some(Spec::Local("/mnt/backup".into())),
        }
    }

    fn ssh_pair(password_auth: bool) -> Endpoints {
        Endpoints {
            local: Some(Spec::Local("/home/greg/proj".into())),
            remote: Some(Spec::Ssh {
                user: Some("greg".into()),
                host: "box".into(),
                port: 22,
                identity: None,
                password_auth,
                path: "/srv/www".into(),
            }),
        }
    }

    #[test]
    fn native_is_chosen_when_asked_for_and_reports_no_reason() {
        let (tool, why) = choose(Executor::Native, &push_plan(), &local_pair());
        assert!(tool.is_none());
        assert!(why.is_none(), "native is not a fallback, it is a choice");
    }

    /// The property that keeps the display usable: a session authenticated
    /// with a typed password is never handed to a subprocess, which would
    /// prompt for it again over the TUI.
    #[test]
    fn a_password_session_is_never_delegated() {
        let why = suits(Tool::Rsync, &push_plan(), &ssh_pair(true)).unwrap_err();
        assert_eq!(why, Unusable::NeedsPassword);
    }

    /// FTP cannot be named to either tool, so the plan falls to the native
    /// engine rather than being pointed somewhere wrong.
    #[test]
    fn an_unaddressable_side_makes_a_tool_unusable() {
        let endpoints = Endpoints {
            local: Some(Spec::Local("/home/greg/proj".into())),
            remote: None,
        };
        let why = suits(Tool::Rsync, &push_plan(), &endpoints).unwrap_err();
        assert!(matches!(why, Unusable::CannotAddress { .. }), "{why:?}");
    }

    /// rsync refuses a remote-to-remote copy itself, so it must not be offered
    /// one.
    #[test]
    fn rsync_declines_two_remote_sides() {
        let remote = |host: &str| {
            Some(Spec::Ssh {
                user: None,
                host: host.into(),
                port: 22,
                identity: None,
                password_auth: false,
                path: "/srv".into(),
            })
        };
        let endpoints = Endpoints {
            local: remote("a"),
            remote: remote("b"),
        };
        assert_eq!(
            suits(Tool::Rsync, &push_plan(), &endpoints).unwrap_err(),
            Unusable::UnsupportedPair
        );
    }

    /// A bisync plan moves files both ways; one invocation copies one way. It
    /// goes to the native engine rather than being split into two subprocesses
    /// whose separate failures would have to be reported as one.
    #[test]
    fn a_two_way_plan_is_not_delegated() {
        let plan = plan_with(vec![
            plan::create_row(
                "to-remote".into(),
                &entry(1),
                crate::sync::Method::SizeMtime,
                1,
            ),
            plan::create_row(
                "to-local".into(),
                &entry(1),
                crate::sync::Method::SizeMtime,
                0,
            ),
        ]);
        assert_eq!(
            suits(Tool::Rsync, &plan, &local_pair()).unwrap_err(),
            Unusable::UnsupportedPair
        );
    }

    /// Deletions alone are not worth a subprocess, and are never delegated in
    /// any case.
    #[test]
    fn a_delete_only_plan_has_nothing_to_delegate() {
        let plan = plan_with(vec![plan::delete_row(
            "gone".into(),
            &entry(1),
            crate::sync::Method::SizeMtime,
            1,
        )]);
        assert_eq!(
            suits(Tool::Rsync, &plan, &local_pair()).unwrap_err(),
            Unusable::NothingToTransfer
        );
    }

    /// An excluded row is not transferred, so a plan whose only transfer is
    /// excluded has nothing to delegate either.
    #[test]
    fn excluded_rows_do_not_count_as_transfers() {
        let mut plan = push_plan();
        plan.rows[0].included = false;
        assert_eq!(
            suits(Tool::Rsync, &plan, &local_pair()).unwrap_err(),
            Unusable::NothingToTransfer
        );
    }

    /// An undecided conflict must not be handed to a tool: the direction is
    /// not known yet, and `can_apply` refuses the plan anyway.
    #[test]
    fn an_undecided_conflict_is_not_a_transfer() {
        let plan = plan_with(vec![plan::conflict_row(
            "both".into(),
            Some(&entry(1)),
            Some(&entry(2)),
            crate::sync::Reason::BothChanged,
            crate::sync::Method::SizeMtime,
        )]);
        assert_eq!(
            suits(Tool::Rsync, &plan, &local_pair()).unwrap_err(),
            Unusable::NothingToTransfer
        );
    }

    #[test]
    fn progress_lines_are_told_from_log_lines() {
        assert!(looks_like_progress(
            "      1,234,567  45%    1.23MB/s    0:00:12"
        ));
        assert!(looks_like_progress(
            "Transferred:   1.234 MiB / 2 MiB, 62%, 1.1 MiB/s, ETA 1s"
        ));

        assert!(!looks_like_progress("sending incremental file list"));
        assert!(!looks_like_progress(
            "rsync: [sender] change_dir failed: No such file or directory (2)"
        ));
    }

    /// rsync separates progress with `\r`. Reading by `\n` alone would hold
    /// the whole run in a buffer and deliver it after the transfer finished.
    #[test]
    fn records_split_on_carriage_returns_as_well_as_newlines() {
        let input = b"first\rsecond\nthird\r";
        let mut reader = BufReader::new(&input[..]);

        let mut records = Vec::new();
        loop {
            let mut buffer = Vec::new();
            match read_record(&mut reader, &mut buffer) {
                Ok(0) => break,
                Ok(_) => records.push(String::from_utf8(buffer).unwrap()),
                Err(e) => panic!("{e}"),
            }
        }

        assert_eq!(records, ["first", "second", "third"]);
    }

    #[test]
    fn a_record_with_no_terminator_still_arrives() {
        let input = b"no newline at the end";
        let mut reader = BufReader::new(&input[..]);
        let mut buffer = Vec::new();

        assert!(read_record(&mut reader, &mut buffer).unwrap() > 0);
        assert_eq!(String::from_utf8(buffer).unwrap(), "no newline at the end");
    }

    /// Unusable reasons have to read as sentences: they go straight into the
    /// status line when a named executor cannot run.
    #[test]
    fn every_reason_explains_itself() {
        for reason in [
            Unusable::NotInstalled("rsync"),
            Unusable::CannotAddress {
                side: "destination",
                scheme: "ftp".into(),
            },
            Unusable::NeedsPassword,
            Unusable::UnsupportedPair,
            Unusable::NothingToTransfer,
        ] {
            let text = reason.to_string();
            assert!(text.len() > 10, "{text}");
            assert!(
                !text.starts_with(char::is_uppercase),
                "a reason is appended to a sentence, so it must not start capitalised: {text}"
            );
        }
    }

    #[test]
    fn side_lookup_follows_the_pane_index() {
        let endpoints = ssh_pair(false);
        assert!(endpoints.side(0).unwrap().is_local());
        assert!(endpoints.side(1).unwrap().is_ssh());
    }

    /// `Side` is re-exported through the plan; this keeps the import honest if
    /// the row builders change shape.
    #[test]
    fn a_transferring_row_is_recognised() {
        let plan = push_plan();
        assert_eq!(transfers(&plan).count(), 1);
        let _ = Side::from_entry(&entry(1));
    }
}
