//! Watching a tree and syncing what changes.
//!
//! The hard part of a file watcher is not noticing changes, it is *not*
//! reacting to each one. Saving a file in an editor can produce half a dozen
//! events -- a write to a temporary name, a rename over the target, a
//! permission change -- and a build touches thousands. A watcher that planned
//! a sync per event would spend its life planning.
//!
//! So changes are collected and the clock restarted on each one, and a plan is
//! built only once the tree has been quiet for `debounce_ms`. The batching is
//! over *paths*, not events: a burst that touches the same file forty times is
//! one path, and what comes out is one plan.
//!
//! **What a plan is built from.** Not the changed paths. The paths decide
//! *when* to plan; the planner then walks the trees as usual. Syncing only
//! the files that fired events would miss anything the watcher dropped --
//! inotify queues overflow, and a move into the tree from outside may arrive
//! as nothing at all -- and would make the watcher's output differ from what
//! `sp` would do on the same trees. One planner, one answer.
//!
//! **A watcher that applies without confirming is a different promise** from
//! the rest of lazysync, so it is a separate setting
//! ([`crate::config::Watch::Auto`]) and it keeps the safety rails: a plan that
//! trips a delete limit is refused in watch mode exactly as it is by hand.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    time::{Duration, Instant},
};

use notify::{RecursiveMode, Watcher as _};

/// A tree being watched, with its pending changes.
pub struct Watcher {
    /// Kept alive: dropping it stops the watch.
    _inner: notify::RecommendedWatcher,
    events: Receiver<PathBuf>,
    debounce: Duration,

    /// Paths seen since the last plan, deduplicated and ordered.
    pending: BTreeSet<PathBuf>,
    /// When the most recent change arrived. The window restarts on each one,
    /// so a continuous stream of writes does not trigger mid-burst.
    last_change: Option<Instant>,

    root: PathBuf,
}

impl Watcher {
    /// Start watching `root` recursively.
    pub fn start(root: &Path, debounce: Duration) -> Result<Self, String> {
        let (sender, events) = mpsc::channel();

        let mut inner =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                // A send failure means the receiver is gone, which means the
                // watcher is being dropped. Nothing to report.
                if let Ok(event) = result {
                    for path in event.paths {
                        let _ = sender.send(path);
                    }
                }
            })
            .map_err(|e| format!("could not start watching: {e}"))?;

        inner
            .watch(root, RecursiveMode::Recursive)
            .map_err(|e| format!("could not watch {}: {e}", root.display()))?;

        Ok(Self {
            _inner: inner,
            events,
            debounce,
            pending: BTreeSet::new(),
            last_change: None,
            root: root.to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// How many paths are waiting, for the status line.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Whether anything is waiting to be synced.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// How long until the current batch is ready, if one is building.
    ///
    /// The main loop uses this as its poll timeout: with nothing pending it
    /// can block on input, and with a batch building it has to wake up when
    /// the window closes. Without this an idle lazysync in watch mode would
    /// spin.
    pub fn next_deadline(&self) -> Option<Duration> {
        let last = self.last_change?;
        Some(self.debounce.saturating_sub(last.elapsed()))
    }

    /// Collect whatever has arrived. `true` if a batch is now ready.
    ///
    /// Never blocks.
    pub fn poll(&mut self) -> bool {
        loop {
            match self.events.try_recv() {
                Ok(path) => {
                    // lazysync's own partial and backup files are not content.
                    // Reacting to them would make a transfer trigger the next
                    // sync, which triggers the next: a watcher that syncs its
                    // own output never stops.
                    if is_ours(&path) {
                        continue;
                    }
                    self.pending.insert(path);
                    self.last_change = Some(Instant::now());
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }

        self.is_ready()
    }

    /// Whether the tree has been quiet long enough.
    fn is_ready(&self) -> bool {
        match self.last_change {
            Some(at) => !self.pending.is_empty() && at.elapsed() >= self.debounce,
            None => false,
        }
    }

    /// Take the batch, leaving the watcher ready for the next one.
    ///
    /// Returned for the status line and the audit log, not for the planner:
    /// the planner walks the trees. See the module docs.
    pub fn take(&mut self) -> Vec<PathBuf> {
        self.last_change = None;
        std::mem::take(&mut self.pending).into_iter().collect()
    }

    /// Throw away the pending batch without syncing it.
    ///
    /// Used when the user cancels a watch-mode plan: the changes have been
    /// seen and declined, and presenting them again on the next tick would
    /// make cancelling impossible.
    pub fn discard(&mut self) {
        self.pending.clear();
        self.last_change = None;
    }
}

/// Whether a path is lazysync's own bookkeeping rather than content.
///
/// The feedback loop this prevents is not hypothetical: a sync writes
/// `x.lazysync-part`, the watcher sees it, plans a sync, which writes another
/// partial file, and so on until something gives.
fn is_ours(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };

    if name.contains(".lazysync-part") || name.contains(".lazysync-conflict") {
        return true;
    }

    // The backup directory, anywhere in the path.
    path.components()
        .any(|c| c.as_os_str() == ".lazysync-backup")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watcher(dir: &Path, debounce_ms: u64) -> Watcher {
        Watcher::start(dir, Duration::from_millis(debounce_ms)).expect("a watcher")
    }

    /// Wait for the watcher to see something, polling rather than sleeping a
    /// guessed amount -- inotify delivery is not instantaneous and a fixed
    /// sleep makes a flaky test.
    fn wait_for_pending(w: &mut Watcher) -> bool {
        for _ in 0..200 {
            w.poll();
            if w.has_pending() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_change_is_noticed() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = watcher(dir.path(), 50);

        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();

        assert!(wait_for_pending(&mut w), "the write should have been seen");
        assert!(w.pending_count() >= 1);
    }

    /// The whole point of the debounce: a batch is not ready while changes are
    /// still arriving.
    #[test]
    fn a_batch_is_not_ready_until_the_tree_goes_quiet() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = watcher(dir.path(), 300);

        std::fs::write(dir.path().join("a.txt"), b"one").unwrap();
        assert!(wait_for_pending(&mut w));

        assert!(
            !w.poll(),
            "300ms has not elapsed, so the batch must not be ready yet"
        );

        // Another change restarts the window.
        std::fs::write(dir.path().join("b.txt"), b"two").unwrap();
        w.poll();
        assert!(!w.poll(), "a fresh change restarts the clock");

        // Now wait it out.
        let mut ready = false;
        for _ in 0..100 {
            if w.poll() {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(ready, "the batch should be ready once the tree is quiet");
    }

    /// A burst touching one file forty times is one path, so one plan.
    #[test]
    fn repeated_changes_to_one_file_collapse_into_one_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = watcher(dir.path(), 50);

        let path = dir.path().join("busy.txt");
        for i in 0..40 {
            std::fs::write(&path, format!("{i}")).unwrap();
        }

        assert!(wait_for_pending(&mut w));
        // Settle, then count.
        for _ in 0..100 {
            if w.poll() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(
            w.pending_count(),
            1,
            "forty writes to one file is one path to sync"
        );
    }

    /// **The feedback loop guard.** A sync writes partial files; if the
    /// watcher reacted to them it would sync its own output for ever.
    #[test]
    fn lazysyncs_own_files_are_ignored() {
        assert!(is_ours(Path::new("/proj/a.txt.lazysync-part")));
        assert!(is_ours(Path::new("/proj/a.txt.lazysync-conflict")));
        assert!(is_ours(Path::new("/proj/.lazysync-backup/old/a.txt")));

        assert!(!is_ours(Path::new("/proj/a.txt")));
        assert!(!is_ours(Path::new("/proj/src/main.rs")));
    }

    #[test]
    fn a_partial_file_does_not_make_a_batch_ready() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = watcher(dir.path(), 20);

        std::fs::write(dir.path().join("a.txt.lazysync-part"), b"in flight").unwrap();

        // Give inotify a chance, then confirm nothing is pending.
        for _ in 0..30 {
            w.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !w.has_pending(),
            "a transfer's own partial file must not trigger the next sync"
        );
    }

    #[test]
    fn taking_a_batch_resets_for_the_next_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = watcher(dir.path(), 20);

        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        assert!(wait_for_pending(&mut w));

        let batch = w.take();
        assert!(!batch.is_empty());
        assert!(!w.has_pending(), "the batch has been taken");
        assert!(!w.poll(), "and nothing is ready until something changes");
    }

    #[test]
    fn discarding_forgets_the_batch() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = watcher(dir.path(), 20);

        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        assert!(wait_for_pending(&mut w));

        w.discard();
        assert!(
            !w.has_pending(),
            "a declined batch must not come back, or cancelling would be impossible"
        );
    }

    /// With nothing pending the main loop should block on input rather than
    /// spin, so there is no deadline to wake up for.
    #[test]
    fn an_idle_watcher_has_no_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let w = watcher(dir.path(), 500);
        assert!(w.next_deadline().is_none());
    }

    #[test]
    fn a_building_batch_has_a_deadline_inside_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = watcher(dir.path(), 500);

        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        assert!(wait_for_pending(&mut w));

        let deadline = w.next_deadline().expect("a batch is building");
        assert!(
            deadline <= Duration::from_millis(500),
            "the deadline cannot exceed the window: {deadline:?}"
        );
    }

    #[test]
    fn watching_a_missing_directory_fails_with_its_name() {
        match Watcher::start(Path::new("/definitely/not/here"), Duration::from_millis(10)) {
            Err(error) => assert!(
                error.contains("/definitely/not/here"),
                "the message has to name the path, since the usual cause is a typo: {error}"
            ),
            Ok(_) => panic!("a missing directory cannot be watched"),
        }
    }
}
