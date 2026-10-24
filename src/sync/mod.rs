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

pub mod compare;
pub mod diff;
pub mod executor;
pub mod plan;
pub mod safety;

use crate::config::SyncMode;

pub use compare::{Comparator, Method, Reason};
pub use diff::{Direction, Endpoint};
pub use executor::{apply, Report};
pub use plan::{Action, Plan, Row, Section, Totals};
pub use safety::DeletePolicy;

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
            // Bisync has no single direction: it is both, with a stored
            // snapshot deciding per file. Phase 6. Treated as a pull here so
            // that a profile naming it is not silently destructive -- the UI
            // refuses it by name before this is reached.
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
    pub const fn deletes(self) -> bool {
        self.direction().delete_extraneous
    }

    /// Whether lazysync can run this mode yet.
    pub const fn is_implemented(self) -> bool {
        !matches!(self, Self::Bisync)
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

    /// A mode that is not implemented must not be quietly destructive: a
    /// profile naming bisync should fail to run, not run a mirror.
    #[test]
    fn bisync_is_not_implemented_and_not_destructive() {
        assert!(!SyncMode::Bisync.is_implemented());
        assert!(
            !SyncMode::Bisync.deletes(),
            "an unimplemented mode falling back to something that deletes would be the worst \
             possible default"
        );
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
