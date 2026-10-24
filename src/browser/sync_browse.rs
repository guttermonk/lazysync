//! Synchronized browsing.
//!
//! The rule is that each pane tracks a path *relative to its own anchor*, not
//! a mirrored absolute path. Enter `src/api` under `~/proj` and the other
//! pane goes to `src/api` under `/srv/www/proj`, whatever the two roots are.
//!
//! What makes this survive an asymmetric pair of trees is that a follower
//! which cannot find its counterpart directory does not stop and does not
//! lose its place: it stands on the path as a *ghost* (see
//! [`super::pane::PaneState::Ghost`]). The relative path is still correct, so
//! when the leader navigates back up, the follower's virtual path shortens
//! with it and the two re-couple without the user doing anything. A follower
//! that held a real path instead would be stranded at the last directory it
//! happened to find, which is the behaviour this exists to avoid.

use std::path::{Path, PathBuf};

use crate::config::{Leader, OnMissing};

use super::pane::Pane;

/// Which pane is being followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Local = 0,
    Remote = 1,
}

impl Side {
    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn other(self) -> Self {
        match self {
            Self::Local => Self::Remote,
            Self::Remote => Self::Local,
        }
    }

    pub const fn from_index(index: usize) -> Self {
        if index == 0 {
            Self::Local
        } else {
            Self::Remote
        }
    }
}

/// What a follow attempt did, so the caller can report it and so
/// `on_missing = "ask"` has something to ask about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Followed {
    /// The follower moved to a real directory.
    Moved(PathBuf),
    /// The counterpart does not exist; the follower is standing on it as a
    /// ghost.
    Ghosted(PathBuf),
    /// The follower held position and the panes are now out of step.
    Desynced,
    /// The follower created the missing directory and moved into it.
    Created(PathBuf),
    /// `on_missing = "ask"`: the caller should ask before creating `0`.
    NeedsPermission(PathBuf),
    /// The leader navigated outside its anchor, so both anchors were reset to
    /// where the panes now are.
    Reanchored,
}

/// Move the follower to match the leader.
///
/// `leader` is the pane that just navigated; `follower` is the other one.
pub fn follow(leader: &mut Pane, follower: &mut Pane, on_missing: OnMissing) -> Followed {
    // Leaving the anchor behind means relative paths would be `../..`-shaped
    // and meaningless. Re-anchor both panes where they stand instead; it is
    // what the user implied by navigating out.
    let Some(relative) = leader.relative_to_sync_root() else {
        leader.reanchor();
        follower.reanchor();
        return Followed::Reanchored;
    };

    let target = follower.sync_root.join(&relative);

    // Already there: nothing to do, and no spurious reload.
    if follower.path() == target {
        return Followed::Moved(target);
    }

    if follower.has_dir(&target) {
        follower.navigate_to(target.clone());
        return Followed::Moved(target);
    }

    match on_missing {
        OnMissing::Ghost => {
            follower.ghost_at(target.clone());
            Followed::Ghosted(target)
        }
        OnMissing::Stay => Followed::Desynced,
        OnMissing::Create => match follower.create_dir(&target) {
            Ok(()) => {
                follower.navigate_to(target.clone());
                Followed::Created(target)
            }
            Err(e) => {
                // A failed mkdir must not strand the follower: ghosting keeps
                // the panes coupled so backing out still works.
                tracing::warn!("could not create {}: {e}", target.display());
                follower.ghost_at(target.clone());
                Followed::Ghosted(target)
            }
        },
        OnMissing::Ask => Followed::NeedsPermission(target),
    }
}

/// Which pane leads, given the configured policy and which pane has focus.
pub const fn leader_side(policy: Leader, focused: Side) -> Side {
    match policy {
        Leader::Focused => focused,
        Leader::Local => Side::Local,
        Leader::Remote => Side::Remote,
    }
}

/// Anchor both panes where they currently stand.
///
/// Called when synchronized browsing is switched on, so that the two
/// directories you are looking at become the pair that moves together --
/// rather than retroactively yanking one pane somewhere else.
pub fn anchor_both(panes: &mut [Pane; 2]) {
    for pane in panes.iter_mut() {
        pane.reanchor();
    }
}

/// Whether the panes currently sit at the same relative path.
pub fn in_step(panes: &[Pane; 2]) -> bool {
    match (
        panes[0].relative_to_sync_root(),
        panes[1].relative_to_sync_root(),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// The relative path both panes are on, for the status bar.
pub fn common_relative(panes: &[Pane; 2]) -> Option<PathBuf> {
    let a = panes[0].relative_to_sync_root()?;
    let b = panes[1].relative_to_sync_root()?;
    (a == b).then_some(a)
}

/// Resolve a name selected in one pane into the equivalent path in the other.
///
/// Used by union view, where the row under the cursor may exist on only one
/// side: the other pane still needs a path to ghost at.
pub fn counterpart_path(from: &Pane, to: &Pane, name: &str) -> Option<PathBuf> {
    let relative = from.relative_to_sync_root()?;
    Some(to.sync_root.join(relative).join(name))
}

/// Whether `path` is inside `root`.
pub fn within(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).is_ok()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::{tempdir, TempDir};

    use super::*;
    use crate::{config::Config, vfs::LocalFs};

    /// Two local trees, deliberately asymmetric -- the case termscp handles
    /// badly:
    ///
    /// ```text
    /// left/                 right/
    ///   src/                  src/
    ///     api/                  api/
    ///                           web/          <- right only
    ///                         docs/           <- right only
    ///   notes/                              <- left only
    /// ```
    fn trees() -> (TempDir, [Pane; 2]) {
        let dir = tempdir().unwrap();
        let left = dir.path().join("left");
        let right = dir.path().join("right");

        for path in [
            left.join("src/api"),
            left.join("notes"),
            right.join("src/api"),
            right.join("src/web"),
            right.join("docs"),
        ] {
            fs::create_dir_all(path).unwrap();
        }

        let config = Config::default();
        let panes = [
            Pane::new(Box::new(LocalFs::new()), left, &config),
            Pane::new(Box::new(LocalFs::new()), right, &config),
        ];
        (dir, panes)
    }

    fn enter(pane: &mut Pane, name: &str) {
        let index = pane
            .entries()
            .iter()
            .position(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name} is not in the listing"));
        for _ in 0..index {
            pane.move_cursor(1);
        }
        pane.enter_selected()
            .unwrap_or_else(|| panic!("could not enter {name}"));
    }

    #[test]
    fn the_follower_matches_the_relative_path_not_the_absolute_one() {
        let (_dir, mut panes) = trees();
        anchor_both(&mut panes);

        let [left, right] = &mut panes;
        enter(left, "src");
        let result = follow(left, right, OnMissing::Ghost);

        assert!(matches!(result, Followed::Moved(_)));
        assert!(right.path().ends_with("right/src"));
        assert!(in_step(&panes));
    }

    #[test]
    fn descending_two_levels_stays_in_step() {
        let (_dir, mut panes) = trees();
        anchor_both(&mut panes);

        let [left, right] = &mut panes;
        enter(left, "src");
        follow(left, right, OnMissing::Ghost);
        enter(left, "api");
        follow(left, right, OnMissing::Ghost);

        assert!(right.path().ends_with("right/src/api"));
        assert_eq!(common_relative(&panes).unwrap(), Path::new("src/api"));
    }

    /// The case this module exists for: the leader enters a directory the
    /// follower does not have. The follower must ghost rather than stop.
    #[test]
    fn a_missing_counterpart_ghosts_instead_of_stopping() {
        let (_dir, mut panes) = trees();
        anchor_both(&mut panes);

        let [left, right] = &mut panes;
        enter(right, "docs"); // right-only

        let result = follow(right, left, OnMissing::Ghost);

        assert!(matches!(result, Followed::Ghosted(_)));
        assert!(left.is_ghost());
        assert!(left.path().ends_with("left/docs"));
        assert!(left.is_empty(), "a ghost pane shows no entries");
    }

    /// And the payoff: going back up re-couples the panes with no
    /// intervention. This is what a follower holding a *real* path cannot do.
    #[test]
    fn backing_out_of_a_ghost_recouples_the_panes() {
        let (_dir, mut panes) = trees();
        anchor_both(&mut panes);

        let [left, right] = &mut panes;
        enter(right, "docs");
        follow(right, left, OnMissing::Ghost);
        assert!(left.is_ghost());

        right.parent().unwrap();
        let result = follow(right, left, OnMissing::Ghost);

        assert!(matches!(result, Followed::Moved(_)));
        assert!(!left.is_ghost(), "the panes should be coupled again");
        assert!(left.path().ends_with("left"));
        assert!(in_step(&panes));
    }

    /// Ghosting survives going *deeper* into a tree the follower does not
    /// have at all, and still unwinds correctly.
    #[test]
    fn nested_ghosts_unwind_one_level_at_a_time() {
        let (_dir, mut panes) = trees();
        anchor_both(&mut panes);

        let [left, right] = &mut panes;
        enter(right, "src");
        follow(right, left, OnMissing::Ghost);
        enter(right, "web"); // right-only, two levels down
        follow(right, left, OnMissing::Ghost);

        assert!(left.is_ghost());
        assert!(left.path().ends_with("left/src/web"));

        right.parent().unwrap();
        follow(right, left, OnMissing::Ghost);

        assert!(!left.is_ghost());
        assert!(left.path().ends_with("left/src"));
    }

    #[test]
    fn stay_leaves_the_follower_put_and_reports_a_desync() {
        let (_dir, mut panes) = trees();
        anchor_both(&mut panes);

        let [left, right] = &mut panes;
        let before = left.path().to_path_buf();
        enter(right, "docs");

        let result = follow(right, left, OnMissing::Stay);

        assert_eq!(result, Followed::Desynced);
        assert_eq!(left.path(), before);
        assert!(!in_step(&panes));
    }

    #[test]
    fn create_makes_the_missing_directory() {
        let (_dir, mut panes) = trees();
        anchor_both(&mut panes);

        let [left, right] = &mut panes;
        enter(right, "docs");

        let result = follow(right, left, OnMissing::Create);

        assert!(matches!(result, Followed::Created(_)));
        assert!(!left.is_ghost());
        assert!(left.real_path().is_dir());
        assert!(left.path().ends_with("left/docs"));
    }

    #[test]
    fn ask_defers_to_the_caller_without_touching_anything() {
        let (_dir, mut panes) = trees();
        anchor_both(&mut panes);

        let [left, right] = &mut panes;
        enter(right, "docs");

        let result = follow(right, left, OnMissing::Ask);

        match result {
            Followed::NeedsPermission(path) => assert!(path.ends_with("left/docs")),
            other => panic!("expected NeedsPermission, got {other:?}"),
        }
        assert!(!left.is_ghost(), "asking must not change anything yet");
        assert!(!left.path().join("docs").exists());
    }

    /// Navigating above the anchor re-anchors both panes rather than
    /// generating relative paths full of `..`.
    #[test]
    fn leaving_the_anchor_reanchors_both_panes() {
        let (_dir, mut panes) = trees();
        anchor_both(&mut panes);

        let [left, right] = &mut panes;
        left.parent().unwrap();

        let result = follow(left, right, OnMissing::Ghost);

        assert_eq!(result, Followed::Reanchored);
        assert_eq!(left.sync_root, left.path());
        assert_eq!(right.sync_root, right.path());
    }

    #[test]
    fn following_when_already_in_place_is_a_no_op() {
        let (_dir, mut panes) = trees();
        anchor_both(&mut panes);

        let [left, right] = &mut panes;
        let result = follow(left, right, OnMissing::Ghost);
        assert!(matches!(result, Followed::Moved(_)));
        assert!(in_step(&panes));
    }

    #[test]
    fn leader_policy_overrides_focus() {
        assert_eq!(leader_side(Leader::Focused, Side::Remote), Side::Remote);
        assert_eq!(leader_side(Leader::Local, Side::Remote), Side::Local);
        assert_eq!(leader_side(Leader::Remote, Side::Local), Side::Remote);
    }

    #[test]
    fn counterpart_path_maps_a_name_across_the_panes() {
        let (_dir, mut panes) = trees();
        anchor_both(&mut panes);

        let [left, right] = &mut panes;
        enter(left, "src");
        follow(left, right, OnMissing::Ghost);

        let path = counterpart_path(left, right, "web").unwrap();
        assert!(path.ends_with("right/src/web"));
    }
}
