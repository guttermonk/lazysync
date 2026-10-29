//! Where things ended up on screen, so that a click can find them.
//!
//! A terminal reports a click as a column and a row, which on its own means
//! nothing: the application is what knows that row 14 of the left-hand pane is
//! `armory.php`. Rather than recompute the layout from the frame size -- which
//! would be a second copy of every `Layout::vertical` in this module, free to
//! drift from the first -- each renderer records the rectangles it just drew.
//! The map is rebuilt every frame, so what it describes is always the frame
//! the user is looking at when they click.
//!
//! Regions may overlap, and the later one wins: a row beats the pane it sits
//! in, and an overlay beats the screen behind it.

use ratatui::layout::{Position, Rect};

use crate::app::Answer;

/// Something a click can land on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A file pane. `row` is the entry under the pointer, or `None` for the
    /// border and the empty space below the listing -- clicking those still
    /// focuses the pane, which is the thing you meant.
    Pane { pane: usize, row: Option<usize> },
    /// A row of the connection list, indexed into the *filtered* view, which
    /// is what `App::bookmark_cursor` counts.
    Bookmark(usize),
    /// A field of the connection form.
    Field(usize),
    /// A line of the confirmation window: a section heading or a plan row.
    PlanEntry(usize),
    /// A run in the audit log.
    LogEntry(usize),
    /// A button of the yes/no dialog.
    Answer(Answer),
    /// The help overlay's text, which scrolls but has nothing to select.
    Help,
    /// Covered by a modal overlay. Nothing happens, and that is the point: a
    /// click aimed at a dialog must not fall through to the screen behind it
    /// and enter a directory nobody asked for.
    Blocked,
}

/// The clickable regions of the last drawn frame, in draw order.
#[derive(Debug, Default, Clone)]
pub struct HitMap {
    regions: Vec<(Rect, Target)>,
}

impl HitMap {
    pub fn push(&mut self, area: Rect, target: Target) {
        self.regions.push((area, target));
    }

    /// Register one target per visible line of a scrolled list.
    ///
    /// `offset` is the widget's own scroll, which is why callers read it back
    /// off the state *after* rendering: the first visible line is whatever
    /// ratatui decided it was, and guessing would put every click one screen
    /// out as soon as the list was long enough to scroll.
    pub fn rows(
        &mut self,
        area: Rect,
        offset: usize,
        count: usize,
        mut target: impl FnMut(usize) -> Target,
    ) {
        for line in 0..area.height {
            let index = offset + line as usize;
            if index >= count {
                break;
            }
            self.push(
                Rect {
                    y: area.y + line,
                    height: 1,
                    ..area
                },
                target(index),
            );
        }
    }

    /// What is at a point, or `None` where nothing was registered.
    pub fn at(&self, column: u16, row: u16) -> Option<Target> {
        self.regions
            .iter()
            .rev()
            .find(|(area, _)| area.contains(Position { x: column, y: row }))
            .map(|(_, target)| *target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_region_drawn_wins() {
        let mut hits = HitMap::default();
        hits.push(Rect::new(0, 0, 10, 10), Target::Pane { pane: 0, row: None });
        hits.push(
            Rect::new(0, 3, 10, 1),
            Target::Pane {
                pane: 0,
                row: Some(7),
            },
        );

        assert_eq!(
            hits.at(5, 3),
            Some(Target::Pane {
                pane: 0,
                row: Some(7)
            }),
            "the row sits on top of the pane it belongs to"
        );
        assert_eq!(hits.at(5, 4), Some(Target::Pane { pane: 0, row: None }));
        assert_eq!(hits.at(50, 4), None, "outside everything");
    }

    /// A list scrolled down by five starts at entry five, not entry zero.
    #[test]
    fn rows_are_offset_by_the_widgets_own_scroll() {
        let mut hits = HitMap::default();
        hits.rows(Rect::new(0, 2, 10, 3), 5, 100, Target::Bookmark);

        assert_eq!(hits.at(0, 2), Some(Target::Bookmark(5)));
        assert_eq!(hits.at(0, 4), Some(Target::Bookmark(7)));
    }

    /// A list shorter than its box leaves the rest of the box unclaimed,
    /// rather than offering rows that are not there.
    #[test]
    fn a_short_list_does_not_claim_the_empty_space_below_it() {
        let mut hits = HitMap::default();
        hits.rows(Rect::new(0, 0, 10, 20), 0, 2, Target::LogEntry);

        assert_eq!(hits.at(0, 1), Some(Target::LogEntry(1)));
        assert_eq!(hits.at(0, 2), None);
    }
}
