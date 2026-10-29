//! Watching a tree walk that has not finished yet.
//!
//! Planning a sync is one blocking call that lists every directory on both
//! sides -- one network round trip each, over SFTP -- and lazysync draws
//! nothing and answers no keys while it runs. A twenty-second scan is
//! indistinguishable from a hang, and the keys pressed during it used to sit
//! in the terminal's buffer and then all arrive at once when it finished.
//!
//! So the walk reports where it has got to. What is done with that is the
//! caller's business: the TUI animates a spinner and watches for the cancel
//! key, and everything headless uses [`Silent`], which is what keeps
//! `lazysync sync --yes` from needing a terminal at all.
//!
//! This is not the whole fix. The scan still owns the thread it runs on, so
//! the browser underneath it is frozen rather than merely busy -- the real
//! answer is a walk that yields, the way `sync::transfer` already does. What
//! this buys is that the wait is legible and interruptible, which is most of
//! what a frozen-looking program costs you.

use std::path::Path;

/// Whether the walk should keep going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// The user asked to stop. The partial plan is discarded -- half a plan
    /// is not a plan, and showing one would invite confirming it.
    Cancel,
}

/// A unit of work the walk has just done.
#[derive(Debug, Clone, Copy)]
pub enum Step<'a> {
    /// A directory was listed on both sides. The path is relative to the
    /// sync root, which is the part worth showing.
    Directory(&'a Path),
    /// One entry was compared. Cheap and frequent: it is what keeps a
    /// spinner moving inside a directory of ten thousand files.
    Entry,
}

/// Somewhere for the walk to report to.
pub trait Progress {
    /// Called once before the first listing.
    fn start(&mut self, _label: &str) {}

    /// Called as the walk proceeds. Returning [`Flow::Cancel`] stops it.
    ///
    /// Implementations must be cheap: this runs once per entry, so anything
    /// that touches the terminal needs its own rate limit.
    fn step(&mut self, _step: Step<'_>) -> Flow {
        Flow::Continue
    }

    /// Called once when the walk ends, cancelled or not.
    fn finish(&mut self) {}
}

/// Reports nothing and never cancels.
///
/// The default everywhere that is not a terminal: `lazysync sync --yes` from
/// cron, and every test that builds a plan directly.
pub struct Silent;

impl Progress for Silent {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts what it was told about, and stops after `limit` entries.
    struct Counter {
        dirs: usize,
        entries: usize,
        limit: usize,
    }

    impl Progress for Counter {
        fn step(&mut self, step: Step<'_>) -> Flow {
            match step {
                Step::Directory(_) => self.dirs += 1,
                Step::Entry => self.entries += 1,
            }
            if self.entries >= self.limit {
                Flow::Cancel
            } else {
                Flow::Continue
            }
        }
    }

    #[test]
    fn silent_always_continues() {
        assert_eq!(Silent.step(Step::Entry), Flow::Continue);
        assert_eq!(
            Silent.step(Step::Directory(Path::new("src"))),
            Flow::Continue
        );
    }

    #[test]
    fn a_watcher_can_ask_the_walk_to_stop() {
        let mut counter = Counter {
            dirs: 0,
            entries: 0,
            limit: 2,
        };

        assert_eq!(counter.step(Step::Entry), Flow::Continue);
        assert_eq!(counter.step(Step::Entry), Flow::Cancel);
        assert_eq!(counter.entries, 2);
    }
}
