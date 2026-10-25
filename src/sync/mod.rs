//! Planning and applying a sync.
//!
//! The shape of this module is the design: **a plan is produced, shown, and
//! only then applied**, and the thing applied is the same structure that was
//! shown. There is no second code path for "actually doing it" whose behaviour
//! could drift from the preview, which is why the plan *is* the dry run rather
//! than a separate report generated alongside one.
//!
//! The order the pieces run in:
//!
//! 1. [`compare::Comparator`] decides what "the same file" means for a given
//!    pair of backends, which is mostly a question about timestamp precision.
//! 2. [`diff::plan`] walks both trees and produces a [`plan::Plan`] of rows,
//!    each carrying both sides' size and time and the reason for its verdict.
//! 3. [`safety::check`] refuses a plan that would delete too much, recording
//!    the reason *on* the plan so the window can show what it was about to do.
//! 4. [`executor::apply`] performs the included rows, re-checking each
//!    destination immediately before touching it.

pub mod bisync;
pub mod compare;
pub mod diff;
pub mod executor;
pub mod filters;
pub mod plan;
pub mod safety;
pub mod state;
pub mod transfer;

use crate::config::SyncMode;

pub use compare::{Comparator, Method, Reason};
pub use diff::{Direction, Endpoint};
pub use executor::{apply, Report};
pub use filters::Filters;
pub use plan::{Action, Plan, Resolution, Row, Section, Totals};
pub use safety::DeletePolicy;
pub use transfer::{State as TransferState, Transfer};

impl SyncMode {
    /// Which pane files come from, and whether extraneous ones are removed.
    ///
    /// Pane 0 is local and 1 is remote, matching [`crate::app::App::panes`].
    pub const fn direction(self) -> Direction {
        match self {
            Self::Push => Direction {
                source: 0,
                delete_extraneous: false,
            },
            Self::Pull => Direction {
                source: 1,
                delete_extraneous: false,
            },
            Self::MirrorPush => Direction {
                source: 0,
                delete_extraneous: true,
            },
            Self::MirrorPull => Direction {
                source: 1,
                delete_extraneous: true,
            },
            // Bisync has no single direction: it is both, decided per file by
            // the stored snapshot, and every row carries its own
            // `target_pane`. Nothing in the bisync path reads this -- it
            // exists because `Direction` is part of the shared signature --
            // so it is set to the non-deleting pull, which is the harmless
            // answer if anything ever does.
            Self::Bisync => Direction {
                source: 1,
                delete_extraneous: false,
            },
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::Pull => "pull",
            Self::MirrorPush => "mirror push",
            Self::MirrorPull => "mirror pull",
            Self::Bisync => "bisync",
        }
    }

    /// Whether this mode deletes anything.
    ///
    /// Bisync does, once a snapshot exists: a file removed on one side is
    /// removed on the other. It cannot be read off `direction` like the
    /// others, because its deletions are per row rather than a property of
    /// the mode's direction.
    pub const fn deletes(self) -> bool {
        match self {
            Self::Bisync => true,
            _ => self.direction().delete_extraneous,
        }
    }

    /// Whether lazysync can run this mode yet.
    pub const fn is_implemented(self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_goes_local_to_remote_and_pull_the_other_way() {
        assert_eq!(SyncMode::Push.direction().source, 0);
        assert_eq!(SyncMode::Push.direction().dest(), 1);
        assert_eq!(SyncMode::Pull.direction().source, 1);
        assert_eq!(SyncMode::Pull.direction().dest(), 0);
    }

    #[test]
    fn only_the_mirror_modes_delete() {
        assert!(!SyncMode::Push.deletes());
        assert!(!SyncMode::Pull.deletes());
        assert!(SyncMode::MirrorPush.deletes());
        assert!(SyncMode::MirrorPull.deletes());
    }

    /// Bisync must declare that it deletes.
    ///
    /// It cannot be read off `direction` like the other modes -- a two-way
    /// sync has no single direction and its deletions are decided per row --
    /// so the risk is the opposite of the usual one: a mode that removes files
    /// reporting that it does not. The second confirmation for permanent
    /// deletion keys off this, so getting it wrong would skip the prompt.
    #[test]
    fn bisync_declares_that_it_deletes() {
        assert!(
            SyncMode::Bisync.deletes(),
            "bisync propagates deletions once a snapshot exists; reporting otherwise would \
             skip the extra confirmation that permanent deletion gets"
        );
        assert!(
            !SyncMode::Bisync.direction().delete_extraneous,
            "and it must not be reachable through `direction`, which bisync does not use"
        );
    }

    /// Every mode is implemented now. Kept as a test rather than deleted
    /// along with the flag: the flag is what the UI consults before planning,
    /// and a mode added later must not default to runnable.
    #[test]
    fn every_mode_is_implemented() {
        for mode in [
            SyncMode::Push,
            SyncMode::Pull,
            SyncMode::MirrorPush,
            SyncMode::MirrorPull,
            SyncMode::Bisync,
        ] {
            assert!(mode.is_implemented(), "{} is not", mode.label());
        }
    }

    #[test]
    fn every_mode_has_a_label() {
        for mode in [
            SyncMode::Push,
            SyncMode::Pull,
            SyncMode::MirrorPush,
            SyncMode::MirrorPull,
            SyncMode::Bisync,
        ] {
            assert!(!mode.label().is_empty());
        }
    }
}
