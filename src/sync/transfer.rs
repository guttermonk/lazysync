//! The transfer queue.
//!
//! Phase 3's executor ran a whole plan in one blocking call: no progress, no
//! cancel, and every file read into memory. This replaces it with a state
//! machine the main loop drives a slice at a time -- [`Transfer::step`] copies
//! one chunk and returns -- so between chunks the UI can draw a progress bar
//! and read the keyboard.
//!
//! **Why not a worker thread.** The obvious design is a background thread
//! owning the two backends. It cannot work here without a second connection:
//! the panes own the `Vfs` values, and `Vfs` is `Send` but not `Sync`, so
//! handing them to a thread means the browser loses them for the duration --
//! and an SFTP session cannot be cloned to make a second one without asking
//! for the password again. Stepping cooperatively gives the two things that
//! actually matter, progress and cancellation, with no shared state at all.
//! Parallel transfers within one SSH session would need multiple channels and
//! are a separate piece of work.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use super::{
    executor::{Applied, Outcome, Report},
    plan::{Action, Plan, Row, Side},
    safety::DeletePolicy,
};
use crate::vfs::{ReadHandle, Vfs, WriteHandle, WriteMetadata};

/// How much is copied per [`Transfer::step`].
///
/// The trade is responsiveness against throughput. 256 KiB over a fast local
/// disk is a fraction of a millisecond and over SFTP is a handful; either way
/// the UI redraws often enough to feel live, and the per-chunk overhead stays
/// invisible next to the transfer itself.
pub const CHUNK: usize = 256 * 1024;

/// What the queue is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    /// Paused between chunks. The current file's streams stay open, so
    /// resuming does not re-send what has already gone.
    Paused,
    /// Cancelled. The file in flight is abandoned and its partial file
    /// removed; files already finished stay finished.
    Cancelling,
    Finished,
}

/// Progress for the file currently being copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    pub path: PathBuf,
    pub done: u64,
    pub total: u64,
}

impl Current {
    pub fn fraction(&self) -> f64 {
        if self.total == 0 {
            // A zero-length file is complete the moment it is created;
            // reporting 0% would leave the bar stuck.
            return 1.0;
        }
        (self.done as f64 / self.total as f64).clamp(0.0, 1.0)
    }
}

/// The file in flight, with its open streams.
struct Active {
    row_index: usize,
    path: PathBuf,
    /// Where the bytes are being written before the rename.
    partial: PathBuf,
    source: Box<dyn ReadHandle>,
    dest: Option<Box<dyn WriteHandle>>,
    metadata: WriteMetadata,
    done: u64,
    total: u64,
    /// Bytes already present when a partial file was resumed.
    resumed_from: u64,
}

/// A plan being applied.
pub struct Transfer {
    plan: Plan,
    /// Indices into `plan.rows`, in the order they will be applied.
    queue: VecDeque<usize>,
    active: Option<Active>,

    state: State,
    report: Report,

    policy: DeletePolicy,
    partial_suffix: String,
    resume_enabled: bool,

    /// Total bytes the plan expects to move, for the aggregate bar.
    total_bytes: u64,
    bytes_done: u64,

    started: Instant,
    /// Bytes at the last throughput sample, and when it was taken.
    sample: (Instant, u64),
    rate_bytes_per_sec: f64,

    /// Directories still to create, shallowest first.
    directories: VecDeque<PathBuf>,
}

impl Transfer {
    /// Queue a plan's included rows.
    pub fn new(
        plan: Plan,
        policy: DeletePolicy,
        partial_suffix: String,
        resume_enabled: bool,
    ) -> Self {
        // Transfers first, deletions last. A mirror that deletes before it
        // writes leaves the destination briefly missing files that the sync
        // was going to provide -- and if it is interrupted in between, missing
        // them for good.
        let mut transfers = Vec::new();
        let mut deletions = Vec::new();
        for (index, row) in plan.rows.iter().enumerate() {
            if !row.included {
                continue;
            }
            match row.action {
                Action::Create | Action::Overwrite => transfers.push(index),
                Action::Delete => deletions.push(index),
                Action::CreateDir | Action::Skip => {}
            }
        }

        let total_bytes = transfers
            .iter()
            .filter_map(|i| plan.rows.get(*i))
            .map(Row::bytes)
            .fold(0u64, u64::saturating_add);

        let directories = plan.required_directories().into();

        let queue: VecDeque<usize> = transfers.into_iter().chain(deletions).collect();
        let now = Instant::now();

        Self {
            state: if queue.is_empty() && plan.required_directories().is_empty() {
                State::Finished
            } else {
                State::Running
            },
            plan,
            queue,
            active: None,
            report: Report::default(),
            policy,
            partial_suffix,
            resume_enabled,
            total_bytes,
            bytes_done: 0,
            started: now,
            sample: (now, 0),
            rate_bytes_per_sec: 0.0,
            directories,
        }
    }

    pub const fn state(&self) -> State {
        self.state
    }

    pub const fn is_finished(&self) -> bool {
        matches!(self.state, State::Finished)
    }

    pub const fn report(&self) -> &Report {
        &self.report
    }

    pub const fn plan(&self) -> &Plan {
        &self.plan
    }

    /// The file being copied, if any.
    pub fn current(&self) -> Option<Current> {
        self.active.as_ref().map(|a| Current {
            path: a.path.clone(),
            done: a.done,
            total: a.total,
        })
    }

    /// How far through the whole plan, by bytes.
    pub fn fraction(&self) -> f64 {
        if self.total_bytes == 0 {
            return if self.is_finished() { 1.0 } else { 0.0 };
        }
        (self.bytes_done as f64 / self.total_bytes as f64).clamp(0.0, 1.0)
    }

    pub const fn bytes_done(&self) -> u64 {
        self.bytes_done
    }

    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Bytes per second, smoothed.
    pub const fn rate(&self) -> f64 {
        self.rate_bytes_per_sec
    }

    /// How many files are left, including the one in flight.
    pub fn remaining_files(&self) -> usize {
        self.queue.len() + usize::from(self.active.is_some())
    }

    /// Estimated time remaining.
    ///
    /// `None` until there is a rate to extrapolate from. A made-up ETA in the
    /// first second is worse than none: it is always wrong and people watch it.
    pub fn eta(&self) -> Option<Duration> {
        if self.rate_bytes_per_sec <= 0.0 || self.is_finished() {
            return None;
        }
        let left = self.total_bytes.saturating_sub(self.bytes_done);
        if left == 0 {
            return None;
        }
        Some(Duration::from_secs_f64(
            left as f64 / self.rate_bytes_per_sec,
        ))
    }

    pub fn pause(&mut self) {
        if matches!(self.state, State::Running) {
            self.state = State::Paused;
        }
    }

    pub fn resume(&mut self) {
        if matches!(self.state, State::Paused) {
            self.state = State::Running;
        }
    }

    pub fn toggle_pause(&mut self) {
        match self.state {
            State::Running => self.pause(),
            State::Paused => self.resume(),
            _ => {}
        }
    }

    /// Stop after abandoning the file in flight.
    pub fn cancel(&mut self) {
        if !matches!(self.state, State::Finished) {
            self.state = State::Cancelling;
        }
    }

    /// Do a slice of work. Returns `true` when there is nothing left.
    ///
    /// Takes the backends on each call rather than owning them, so the panes
    /// keep theirs and the browser stays usable the moment the transfer ends.
    pub fn step(
        &mut self,
        source: &mut dyn Vfs,
        source_root: &Path,
        dest: &mut dyn Vfs,
        dest_root: &Path,
    ) -> bool {
        match self.state {
            State::Finished => return true,
            State::Paused => return false,
            State::Cancelling => {
                self.abandon_active(dest);
                self.finish();
                return true;
            }
            State::Running => {}
        }

        // Directories first, shallowest to deepest, so no create can fail for
        // a missing parent.
        if let Some(relative) = self.directories.pop_front() {
            let target = dest_root.join(&relative);
            if let Err(e) = dest.create_dir(&target) {
                self.report.applied.push(Applied {
                    path: relative,
                    action: Action::CreateDir,
                    outcome: Outcome::Failed { why: e.to_string() },
                });
            }
            return false;
        }

        if self.active.is_some() {
            self.copy_chunk(dest);
            return false;
        }

        let Some(index) = self.queue.pop_front() else {
            self.finish();
            return true;
        };

        let row = self.plan.rows[index].clone();
        match row.action {
            Action::Delete => {
                let outcome = super::executor::delete(dest, dest_root, &row, &self.policy);
                self.report.applied.push(Applied {
                    path: row.path.clone(),
                    action: row.action,
                    outcome,
                });
            }
            Action::Create | Action::Overwrite => {
                if let Err(outcome) =
                    self.begin_file(index, &row, source, source_root, dest, dest_root)
                {
                    self.report.applied.push(Applied {
                        path: row.path.clone(),
                        action: row.action,
                        outcome,
                    });
                }
            }
            Action::CreateDir | Action::Skip => {}
        }

        false
    }

    /// Open both streams for one row.
    fn begin_file(
        &mut self,
        index: usize,
        row: &Row,
        source: &mut dyn Vfs,
        source_root: &Path,
        dest: &mut dyn Vfs,
        dest_root: &Path,
    ) -> Result<(), Outcome> {
        let from = source_root.join(&row.path);
        let to = dest_root.join(&row.path);

        // Re-stat the source: a protocol that sends a length before the data
        // needs the current size, and a stale one corrupts the transfer rather
        // than merely misreporting it.
        let current = source.stat(&from).map_err(|e| Outcome::Failed {
            why: format!("the source is gone: {e}"),
        })?;

        // Re-check the destination against the plan. Overwriting on the
        // strength of a snapshot that is no longer true is how a sync destroys
        // something the user never saw in the window.
        if let Some(planned) = row.dest {
            match dest.stat(&to) {
                Ok(now) if Side::from_entry(&now) != planned => {
                    return Err(Outcome::SkippedStale {
                        why: "the destination changed after the plan was built".into(),
                    });
                }
                Ok(_) => {}
                // Planned as an overwrite and now absent: writing it is still
                // the intent.
                Err(e) if e.is_not_found() => {}
                Err(e) => return Err(Outcome::Failed { why: e.to_string() }),
            }
        }

        let partial = partial_path(&to, &self.partial_suffix);
        let metadata = WriteMetadata::from_entry(&current);

        // Resume, if a partial file from an earlier run is there and both ends
        // can pick up mid-file. Anything longer than the source is debris from
        // a different version of the file, so it is discarded rather than
        // appended to.
        let resumed_from = if self.resume_enabled
            && source.capabilities().ranged_writes
            && dest.capabilities().ranged_writes
        {
            match dest.stat(&partial) {
                Ok(existing) if existing.size < current.size => existing.size,
                Ok(_) => {
                    let _ = dest.remove_file(&partial);
                    0
                }
                Err(_) => 0,
            }
        } else {
            let _ = dest.remove_file(&partial);
            0
        };

        let reader = source
            .open_read(&from, resumed_from)
            .map_err(|e| Outcome::Failed {
                why: format!("reading: {e}"),
            })?;

        let writer = dest
            .open_write(&partial, &metadata, resumed_from > 0)
            .map_err(|e| Outcome::Failed {
                why: format!("writing: {e}"),
            })?;

        if resumed_from > 0 {
            tracing::info!(
                "resuming {} from {resumed_from} of {} bytes",
                row.path.display(),
                current.size
            );
        }

        self.active = Some(Active {
            row_index: index,
            path: row.path.clone(),
            partial,
            source: reader,
            dest: Some(writer),
            metadata,
            done: resumed_from,
            total: current.size,
            resumed_from,
        });

        // Bytes already on disk count towards the aggregate, or resuming a
        // half-done file would make the bar go backwards.
        self.bytes_done = self.bytes_done.saturating_add(resumed_from);

        Ok(())
    }

    /// Copy up to [`CHUNK`] bytes of the file in flight.
    fn copy_chunk(&mut self, dest: &mut dyn Vfs) {
        let Some(active) = &mut self.active else {
            return;
        };

        let mut buffer = vec![0u8; CHUNK];
        let read = match std::io::Read::read(&mut active.source, &mut buffer) {
            Ok(count) => count,
            Err(e) => {
                let path = active.path.clone();
                let index = active.row_index;
                self.fail_active(dest, index, &path, format!("reading: {e}"));
                return;
            }
        };

        if read == 0 {
            self.complete_active(dest);
            return;
        }

        let Some(writer) = active.dest.as_mut() else {
            return;
        };
        if let Err(e) = std::io::Write::write_all(writer, &buffer[..read]) {
            let path = active.path.clone();
            let index = active.row_index;
            self.fail_active(dest, index, &path, format!("writing: {e}"));
            return;
        }

        active.done = active.done.saturating_add(read as u64);
        self.bytes_done = self.bytes_done.saturating_add(read as u64);
        self.update_rate();
    }

    /// Finish the file in flight: close the streams and rename it into place.
    fn complete_active(&mut self, dest: &mut dyn Vfs) {
        let Some(mut active) = self.active.take() else {
            return;
        };

        let row = self.plan.rows[active.row_index].clone();
        let to = row_target(&self.plan, &row);

        // A short read means the file changed while being copied. Writing what
        // was read would put a truncated file at the destination, which is
        // worse than not copying at all.
        if active.done != active.total {
            if let Some(writer) = active.dest.take() {
                let _ = writer.finish();
            }
            let _ = dest.remove_file(&active.partial);
            self.report.applied.push(Applied {
                path: active.path.clone(),
                action: row.action,
                outcome: Outcome::Failed {
                    why: format!(
                        "the source changed while being read ({} bytes of {})",
                        active.done, active.total
                    ),
                },
            });
            return;
        }

        if let Some(writer) = active.dest.take() {
            if let Err(e) = writer.finish() {
                let _ = dest.remove_file(&active.partial);
                self.report.applied.push(Applied {
                    path: active.path.clone(),
                    action: row.action,
                    outcome: Outcome::Failed {
                        why: format!("closing the destination: {e}"),
                    },
                });
                return;
            }
        }

        // Reading is finished too; a protocol stream has to be closed or the
        // channel leaks for the rest of the session.
        if let Err(e) = active.source.finish() {
            tracing::debug!("closing the read of {}: {e}", active.path.display());
        }

        if let Err(e) =
            super::executor::move_into_place(dest, &active.partial, &to, row.dest.is_some())
        {
            let _ = dest.remove_file(&active.partial);
            self.report.applied.push(Applied {
                path: active.path.clone(),
                action: row.action,
                outcome: Outcome::Failed {
                    why: format!("renaming into place: {e}"),
                },
            });
            return;
        }

        // Timestamps again after the rename: some backends apply them to the
        // partial name and lose them in the move, and a destination stamped
        // "now" looks newer than its source -- which would make the next sync
        // in the other direction copy it back.
        if active.metadata.mtime.is_some() {
            if let Err(e) = dest.set_times(&to, &active.metadata) {
                tracing::debug!("could not restamp {}: {e}", to.display());
            }
        }

        self.report.bytes = self
            .report
            .bytes
            .saturating_add(active.done.saturating_sub(active.resumed_from));
        self.report.applied.push(Applied {
            path: active.path,
            action: row.action,
            outcome: Outcome::Transferred { bytes: active.done },
        });
    }

    /// Abandon the file in flight with an error.
    fn fail_active(&mut self, dest: &mut dyn Vfs, index: usize, path: &Path, why: String) {
        let action = self.plan.rows[index].action;
        if let Some(mut active) = self.active.take() {
            if let Some(writer) = active.dest.take() {
                let _ = writer.finish();
            }
            // The partial file stays only if resume is on: it is the point of
            // having one. Otherwise it would show up in the next plan as a
            // file to sync back.
            if !self.resume_enabled {
                let _ = dest.remove_file(&active.partial);
            }
        }

        self.report.applied.push(Applied {
            path: path.to_path_buf(),
            action,
            outcome: Outcome::Failed { why },
        });
    }

    /// Drop the file in flight because the user cancelled.
    fn abandon_active(&mut self, dest: &mut dyn Vfs) {
        let Some(mut active) = self.active.take() else {
            return;
        };

        if let Some(writer) = active.dest.take() {
            let _ = writer.finish();
        }

        // Cancelling keeps the partial file when resume is on, so picking the
        // sync up again does not re-send what already went.
        if !self.resume_enabled {
            let _ = dest.remove_file(&active.partial);
        }

        self.report.applied.push(Applied {
            path: active.path,
            action: self.plan.rows[active.row_index].action,
            outcome: Outcome::Cancelled {
                partial_bytes: active.done,
            },
        });
    }

    fn finish(&mut self) {
        self.state = State::Finished;
        self.queue.clear();
    }

    /// Update the smoothed throughput.
    fn update_rate(&mut self) {
        const WINDOW: Duration = Duration::from_millis(500);
        // Exponential smoothing. A raw bytes-per-sample figure jumps around so
        // much that the ETA built on it is unreadable.
        const SMOOTHING: f64 = 0.3;

        let elapsed = self.sample.0.elapsed();
        if elapsed < WINDOW {
            return;
        }

        let moved = self.bytes_done.saturating_sub(self.sample.1);
        let instant = moved as f64 / elapsed.as_secs_f64();

        self.rate_bytes_per_sec = if self.rate_bytes_per_sec <= 0.0 {
            instant
        } else {
            SMOOTHING * instant + (1.0 - SMOOTHING) * self.rate_bytes_per_sec
        };

        self.sample = (Instant::now(), self.bytes_done);
    }
}

/// Where a row's file goes.
fn row_target(plan: &Plan, row: &Row) -> PathBuf {
    plan.dest_root.join(&row.path)
}

/// `path` with the partial-transfer suffix appended.
pub fn partial_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push_str(suffix);
    path.with_file_name(name)
}

/// Format a duration as `1m 23s`, for the ETA.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use filetime::FileTime;
    use tempfile::TempDir;

    use super::*;
    use crate::{
        config::Config,
        sync::diff::{self, Direction, Endpoint},
        vfs::LocalFs,
    };

    fn write(path: &Path, content: &[u8], epoch: i64) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
        filetime::set_file_mtime(path, FileTime::from_unix_time(epoch, 0)).unwrap();
    }

    struct Trees {
        _dir: TempDir,
        left: PathBuf,
        right: PathBuf,
    }

    fn trees() -> Trees {
        let dir = tempfile::tempdir().unwrap();
        let left = dir.path().join("left");
        let right = dir.path().join("right");
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();
        Trees {
            _dir: dir,
            left,
            right,
        }
    }

    fn build(t: &Trees, mirror: bool) -> Plan {
        let mut source = LocalFs::new();
        let mut dest = LocalFs::new();
        diff::plan(
            Endpoint {
                fs: &mut source,
                root: &t.left,
                label: "local".into(),
            },
            Endpoint {
                fs: &mut dest,
                root: &t.right,
                label: "remote".into(),
            },
            Direction {
                source: 0,
                delete_extraneous: mirror,
            },
            &Config::default(),
        )
    }

    fn transfer(plan: Plan, resume: bool) -> Transfer {
        Transfer::new(plan, DeletePolicy::Remove, ".lazysync-part".into(), resume)
    }

    /// Run to completion, counting steps so a test can assert it really was
    /// incremental rather than one blocking call.
    fn drain(t: &Trees, transfer: &mut Transfer) -> usize {
        let mut source = LocalFs::new();
        let mut dest = LocalFs::new();
        let mut steps = 0;
        while !transfer.step(&mut source, &t.left, &mut dest, &t.right) {
            steps += 1;
            assert!(steps < 100_000, "the transfer did not terminate");
        }
        steps
    }

    #[test]
    fn a_transfer_copies_a_file() {
        let t = trees();
        write(&t.left.join("a.txt"), b"hello", 1_700_000_000);

        let mut transfer = transfer(build(&t, false), true);
        drain(&t, &mut transfer);

        assert!(transfer.is_finished());
        assert_eq!(transfer.report().failures().count(), 0);
        assert_eq!(fs::read_to_string(t.right.join("a.txt")).unwrap(), "hello");
    }

    /// The reason the state machine exists: work has to happen in slices, so
    /// the UI can draw between them.
    #[test]
    fn a_large_file_takes_several_steps() {
        let t = trees();
        // Three chunks' worth.
        let payload = vec![b'x'; CHUNK * 3];
        write(&t.left.join("big.bin"), &payload, 1_000_000);

        let mut transfer = transfer(build(&t, false), true);
        let steps = drain(&t, &mut transfer);

        assert!(
            steps >= 3,
            "a {}-byte file should take at least three chunks, took {steps}",
            payload.len()
        );
        assert_eq!(
            fs::read(t.right.join("big.bin")).unwrap().len(),
            payload.len()
        );
    }

    #[test]
    fn progress_is_reported_while_copying() {
        let t = trees();
        write(&t.left.join("big.bin"), &vec![b'x'; CHUNK * 3], 1_000_000);

        let mut transfer = transfer(build(&t, false), true);
        let mut source = LocalFs::new();
        let mut dest = LocalFs::new();

        // Step until a file is in flight with some bytes done.
        let mut seen_partial_progress = false;
        for _ in 0..50 {
            if transfer.step(&mut source, &t.left, &mut dest, &t.right) {
                break;
            }
            if let Some(current) = transfer.current() {
                if current.done > 0 && current.done < current.total {
                    assert_eq!(current.path, Path::new("big.bin"));
                    assert!(current.fraction() > 0.0 && current.fraction() < 1.0);
                    assert!(transfer.fraction() > 0.0);
                    seen_partial_progress = true;
                }
            }
        }

        assert!(
            seen_partial_progress,
            "a multi-chunk file should report partial progress"
        );
    }

    #[test]
    fn the_aggregate_total_is_the_sum_of_the_included_rows() {
        let t = trees();
        write(&t.left.join("a"), b"12345", 1_000_000);
        write(&t.left.join("b"), b"1234567890", 1_000_000);

        let transfer = transfer(build(&t, false), true);
        assert_eq!(transfer.total_bytes(), 15);
        assert_eq!(transfer.remaining_files(), 2);
    }

    #[test]
    fn a_zero_length_file_reports_as_complete_rather_than_stuck() {
        let current = Current {
            path: "empty".into(),
            done: 0,
            total: 0,
        };
        assert_eq!(current.fraction(), 1.0);
    }

    #[test]
    fn an_empty_plan_is_finished_immediately() {
        let t = trees();
        let mut transfer = transfer(build(&t, false), true);
        assert!(transfer.is_finished());

        let mut source = LocalFs::new();
        let mut dest = LocalFs::new();
        assert!(transfer.step(&mut source, &t.left, &mut dest, &t.right));
    }

    #[test]
    fn pausing_stops_progress_and_resuming_continues() {
        let t = trees();
        write(&t.left.join("big.bin"), &vec![b'x'; CHUNK * 3], 1_000_000);

        let mut transfer = transfer(build(&t, false), true);
        let mut source = LocalFs::new();
        let mut dest = LocalFs::new();

        // Get one chunk in.
        for _ in 0..3 {
            transfer.step(&mut source, &t.left, &mut dest, &t.right);
        }
        transfer.pause();
        assert_eq!(transfer.state(), State::Paused);

        let before = transfer.bytes_done();
        for _ in 0..10 {
            assert!(
                !transfer.step(&mut source, &t.left, &mut dest, &t.right),
                "a paused transfer must not report itself finished"
            );
        }
        assert_eq!(
            transfer.bytes_done(),
            before,
            "a paused transfer must not move bytes"
        );

        transfer.resume();
        drain(&t, &mut transfer);
        assert_eq!(
            fs::read(t.right.join("big.bin")).unwrap().len(),
            CHUNK * 3,
            "resuming has to finish the file"
        );
    }

    #[test]
    fn toggling_pause_flips_between_the_two_states() {
        let t = trees();
        write(&t.left.join("a"), b"x", 1_000_000);
        let mut transfer = transfer(build(&t, false), true);

        assert_eq!(transfer.state(), State::Running);
        transfer.toggle_pause();
        assert_eq!(transfer.state(), State::Paused);
        transfer.toggle_pause();
        assert_eq!(transfer.state(), State::Running);
    }

    /// Cancelling has to stop promptly and leave what was already finished
    /// alone.
    #[test]
    fn cancelling_stops_and_keeps_the_completed_files() {
        let t = trees();
        write(&t.left.join("a-small"), b"done", 1_000_000);
        write(&t.left.join("b-big.bin"), &vec![b'x'; CHUNK * 4], 1_000_000);

        let mut transfer = transfer(build(&t, false), false);
        let mut source = LocalFs::new();
        let mut dest = LocalFs::new();

        // Let the small file finish and the big one start.
        for _ in 0..5 {
            transfer.step(&mut source, &t.left, &mut dest, &t.right);
        }

        transfer.cancel();
        assert!(transfer.step(&mut source, &t.left, &mut dest, &t.right));
        assert!(transfer.is_finished());

        assert_eq!(
            fs::read_to_string(t.right.join("a-small")).unwrap(),
            "done",
            "a file that finished before the cancel stays finished"
        );
        assert!(
            !t.right.join("b-big.bin").exists(),
            "the file in flight must not be left half-written at its real name"
        );
        // With resume off, no partial debris either.
        let leftovers: Vec<_> = fs::read_dir(&t.right)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("lazysync-part"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn a_cancelled_file_is_reported_as_cancelled() {
        let t = trees();
        write(&t.left.join("big.bin"), &vec![b'x'; CHUNK * 4], 1_000_000);

        let mut transfer = transfer(build(&t, false), false);
        let mut source = LocalFs::new();
        let mut dest = LocalFs::new();

        for _ in 0..2 {
            transfer.step(&mut source, &t.left, &mut dest, &t.right);
        }
        transfer.cancel();
        transfer.step(&mut source, &t.left, &mut dest, &t.right);

        let cancelled: Vec<_> = transfer
            .report()
            .applied
            .iter()
            .filter(|a| matches!(a.outcome, Outcome::Cancelled { .. }))
            .collect();
        assert_eq!(cancelled.len(), 1, "{:?}", transfer.report().applied);
    }

    /// With resume on, a cancelled transfer leaves the partial file so the
    /// next run does not re-send what already went.
    #[test]
    fn cancelling_with_resume_on_keeps_the_partial_file() {
        let t = trees();
        write(&t.left.join("big.bin"), &vec![b'x'; CHUNK * 4], 1_000_000);

        let mut transfer = transfer(build(&t, false), true);
        let mut source = LocalFs::new();
        let mut dest = LocalFs::new();

        for _ in 0..2 {
            transfer.step(&mut source, &t.left, &mut dest, &t.right);
        }
        transfer.cancel();
        transfer.step(&mut source, &t.left, &mut dest, &t.right);

        assert!(
            t.right.join("big.bin.lazysync-part").exists(),
            "the partial file is the whole point of resuming"
        );
    }

    /// And the payoff: a second run picks up where the first stopped.
    #[test]
    fn a_resumed_transfer_continues_from_the_partial_file() {
        let t = trees();
        let payload = vec![b'x'; CHUNK * 4];
        write(&t.left.join("big.bin"), &payload, 1_000_000);

        // First attempt, cancelled part-way.
        let mut first = transfer(build(&t, false), true);
        let mut source = LocalFs::new();
        let mut dest = LocalFs::new();
        for _ in 0..3 {
            first.step(&mut source, &t.left, &mut dest, &t.right);
        }
        first.cancel();
        first.step(&mut source, &t.left, &mut dest, &t.right);

        let partial_size = fs::metadata(t.right.join("big.bin.lazysync-part"))
            .unwrap()
            .len();
        assert!(partial_size > 0 && partial_size < payload.len() as u64);

        // Second attempt resumes.
        let mut second = transfer(build(&t, false), true);
        drain(&t, &mut second);

        assert_eq!(
            fs::read(t.right.join("big.bin")).unwrap(),
            payload,
            "the resumed file has to be byte-identical, not just the right length"
        );
        assert!(
            second.report().bytes < payload.len() as u64,
            "resuming should re-send less than the whole file: sent {} of {}",
            second.report().bytes,
            payload.len()
        );
    }

    /// A partial file longer than the source is debris from a different
    /// version. Appending to it would produce a corrupt file.
    #[test]
    fn an_oversized_partial_file_is_discarded_rather_than_resumed() {
        let t = trees();
        write(&t.left.join("a"), b"short", 1_000_000);
        write(&t.right.join("a.lazysync-part"), b"much longer debris", 500);

        let mut transfer = transfer(build(&t, false), true);
        drain(&t, &mut transfer);

        assert_eq!(
            fs::read_to_string(t.right.join("a")).unwrap(),
            "short",
            "the stale partial must be thrown away, not appended to"
        );
    }

    /// Transfers before deletions: a mirror that deletes first leaves the
    /// destination missing files the sync was about to provide.
    #[test]
    fn transfers_are_queued_before_deletions() {
        let t = trees();
        write(&t.left.join("keep"), b"x", 1_000_000);
        write(&t.right.join("keep"), b"x", 1_000_000);
        write(&t.left.join("new"), b"y", 1_000_000);
        write(&t.right.join("gone"), b"z", 1_000_000);

        let mut plan = build(&t, true);
        super::super::safety::check(&mut plan, &crate::config::Safety::default());
        assert!(plan.refusal.is_none(), "{:?}", plan.refusal);

        let mut transfer = transfer(plan, true);
        drain(&t, &mut transfer);

        let order: Vec<&Action> = transfer
            .report()
            .applied
            .iter()
            .map(|a| &a.action)
            .collect();
        let first_delete = order.iter().position(|a| **a == Action::Delete);
        let last_transfer = order
            .iter()
            .rposition(|a| matches!(a, Action::Create | Action::Overwrite));

        if let (Some(delete), Some(transfer_at)) = (first_delete, last_transfer) {
            assert!(
                transfer_at < delete,
                "deletions must come after transfers: {order:?}"
            );
        }
    }

    #[test]
    fn nested_directories_are_created_before_their_contents() {
        let t = trees();
        write(&t.left.join("a/b/c/deep.txt"), b"deep", 1_000_000);

        let mut transfer = transfer(build(&t, false), true);
        drain(&t, &mut transfer);

        assert_eq!(transfer.report().failures().count(), 0);
        assert_eq!(
            fs::read_to_string(t.right.join("a/b/c/deep.txt")).unwrap(),
            "deep"
        );
    }

    #[test]
    fn the_eta_is_absent_until_there_is_a_rate() {
        let t = trees();
        write(&t.left.join("a"), b"x", 1_000_000);
        let transfer = transfer(build(&t, false), true);

        assert!(
            transfer.eta().is_none(),
            "a made-up ETA in the first second is worse than none"
        );
        assert_eq!(transfer.rate(), 0.0);
    }

    #[test]
    fn durations_format_readably() {
        assert_eq!(format_duration(Duration::from_secs(0)), "0s");
        assert_eq!(format_duration(Duration::from_secs(45)), "45s");
        assert_eq!(format_duration(Duration::from_secs(83)), "1m 23s");
        assert_eq!(format_duration(Duration::from_secs(3725)), "1h 02m");
    }

    #[test]
    fn the_partial_suffix_is_appended_to_the_whole_name() {
        assert_eq!(
            partial_path(Path::new("/srv/notes.md"), ".lazysync-part"),
            PathBuf::from("/srv/notes.md.lazysync-part")
        );
        assert_eq!(
            partial_path(Path::new("/srv/a.tar.gz"), ".part"),
            PathBuf::from("/srv/a.tar.gz.part")
        );
    }

    #[test]
    fn timestamps_survive_a_chunked_transfer() {
        let t = trees();
        write(
            &t.left.join("big.bin"),
            &vec![b'x'; CHUNK * 2],
            1_700_000_000,
        );

        let mut transfer = transfer(build(&t, false), true);
        drain(&t, &mut transfer);

        let landed =
            FileTime::from_last_modification_time(&fs::metadata(t.right.join("big.bin")).unwrap());
        assert_eq!(
            landed.unix_seconds(),
            1_700_000_000,
            "a timestamp lost in a chunked write makes the next sync re-copy the file"
        );
    }
}
