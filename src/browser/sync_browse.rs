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
    /// The leader stepped above its anchor, so there is no relative path to
    /// follow. The follower holds and **the pairing is kept**, so coming back
    /// down re-couples the panes.
    OutsideRoot,
}

/// Move the follower to match the leader.
///
/// `leader` is the pane that just navigated; `follower` is the other one.
pub fn follow(leader: &mut Pane, follower: &mut Pane, on_missing: OnMissing) -> Followed {
    // Above the anchor there is no relative path to follow -- it would be
    // `../..`-shaped, and the two roots are not required to share a name, so
    // nothing can reconstruct the correspondence by walking back down.
    //
    // **The anchors are therefore left alone.** This used to re-anchor both
    // panes where they stood, on the reasoning that navigating out implied a
    // new pairing. It does not, and the cost was severe: one press of the
    // parent key destroyed the pairing the bookmark had established, and
    // because the roots usually have *different* names -- `sandbox
    // subdomain` paired with `www` -- walking back into the directory you
    // just left pointed the follower at a sibling that does not exist. The
    // panes looked broken and the only way back was to reopen the bookmark.
    //
    // Holding the pairing instead means stepping out is a temporary
    // excursion: the follower stays put, and the moment the leader is back
    // inside its root the normal relative follow resumes and the panes
    // re-couple on their own. That matches what the ghost mechanism already
    // does when you descend into a directory the other side lacks, which is
    // the same problem pointed the other way.
    //
    // Deliberately re-pairing is still available, and is now something you
    // ask for rather than something a stray keystroke does: toggling
    // synchronized browsing off and on calls `anchor_both` and pairs the two
    // directories you are actually looking at.
    let Some(relative) = leader.relative_to_sync_root() else {
        return Followed::OutsideRoot;
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

/// Put the follower's cursor and viewport where the leader's are.
///
/// [`follow`] keeps the two panes in the same *directory*; this keeps them on
/// the same *row* of it. Browsing a pair of corresponding trees side by side
/// is a comparison, and a comparison wants row N next to row N -- scrolling
/// one pane forty files down while the other sits at the top means reading
/// two unrelated parts of two directories and calling it synchronized.
///
/// **By ordinal position, not by name.** Lining equal names up against each
/// other is a real thing to want, and lazysync already has it: union view
/// merges both listings into one set of rows with one cursor, which is the
/// honest way to do it because it can show that a name exists on one side
/// only. A second, subtler alignment here would be a feature that looks like
/// union view and disagrees with it in exactly the asymmetric cases that
/// matter.
///
/// Both are clamped by the follower's own length, so a shorter directory
/// stops at its last entry rather than scrolling into blank space. A ghosted
/// follower has no entries at all, which clamps to zero and costs nothing.
pub fn follow_cursor(leader: &Pane, follower: &mut Pane) {
    follower.set_cursor(leader.cursor());

    // The renderer would pull a too-large offset back on its own to keep the
    // selection visible, but only after drawing one frame from it. Clamping
    // here means the follower never flashes a blank listing.
    follower.scroll = leader.scroll.min(follower.len().saturating_sub(1));
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

    /// Stepping above the anchor must not destroy the pairing.
    #[test]
    fn leaving_the_anchor_keeps_both_anchors() {
        let (_dir, mut panes) = trees();
        anchor_both(&mut panes);

        let [left, right] = &mut panes;
        let left_anchor = left.sync_root.clone();
        let right_anchor = right.sync_root.clone();
        let right_was = right.path().to_path_buf();

        left.parent().unwrap();
        let result = follow(left, right, OnMissing::Ghost);

        assert_eq!(result, Followed::OutsideRoot);
        assert_eq!(left.sync_root, left_anchor, "the pairing must survive");
        assert_eq!(right.sync_root, right_anchor);
        assert_eq!(right.path(), right_was, "the follower holds its place");
    }

    /// The reported bug, as a regression test.
    ///
    /// Back out of the paired root and walk straight back into it. The two
    /// roots have *different names* here -- `left` pairs with `right` -- which
    /// is what made the old re-anchoring behaviour destructive: after it, the
    /// follower was asked for `right/left`, which does not exist, and the
    /// panes could not be re-coupled without reopening the bookmark.
    #[test]
    fn backing_out_of_the_root_and_returning_recouples() {
        let (_dir, mut panes) = trees();
        anchor_both(&mut panes);

        let [left, right] = &mut panes;
        let root_name = left
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        // Out of the paired root...
        left.parent().unwrap();
        assert_eq!(follow(left, right, OnMissing::Ghost), Followed::OutsideRoot);

        // ...and back into it.
        enter(left, &root_name);
        let result = follow(left, right, OnMissing::Ghost);

        assert!(
            matches!(result, Followed::Moved(_)),
            "returning to the root must re-couple, got {result:?}"
        );
        assert!(!right.is_ghost(), "the follower must not be stranded");
        assert!(
            right.path().ends_with("right"),
            "the follower belongs at its own root, not at right/{root_name}: {}",
            right.path().display()
        );
        assert!(in_step(&panes));
    }

    /// And descending again after the excursion still maps correctly, which
    /// is the part that silently pointed at a sibling before.
    #[test]
    fn descending_after_an_excursion_maps_to_the_right_directory() {
        let (_dir, mut panes) = trees();
        anchor_both(&mut panes);

        let [left, right] = &mut panes;
        let root_name = left
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        left.parent().unwrap();
        follow(left, right, OnMissing::Ghost);
        enter(left, &root_name);
        follow(left, right, OnMissing::Ghost);

        enter(left, "src");
        follow(left, right, OnMissing::Ghost);

        assert!(
            right.path().ends_with("right/src"),
            "expected right/src, got {}",
            right.path().display()
        );
        assert_eq!(common_relative(&panes).unwrap(), Path::new("src"));
    }

    /// Toggling synchronized browsing is how you deliberately re-pair, now
    /// that a stray parent-key press no longer does it.
    #[test]
    fn anchoring_again_is_how_a_new_pairing_is_established() {
        let (_dir, mut panes) = trees();
        anchor_both(&mut panes);

        let [left, right] = &mut panes;
        left.parent().unwrap();
        follow(left, right, OnMissing::Ghost);

        anchor_both(&mut panes);

        assert_eq!(panes[0].sync_root, panes[0].path());
        assert_eq!(panes[1].sync_root, panes[1].path());
        assert!(in_step(&panes));
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
