//! The help overlay.
//!
//! Built entirely from the resolved keymap, so a rebinding in config shows up
//! here with no second place to update -- and a binding can never be
//! documented with a key it does not have.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
    Frame,
};

use crate::{
    app::App,
    keys::{Action, Mode},
};

pub fn draw(frame: &mut Frame<'_>, app: &App, scroll: usize, hits: &mut super::HitMap) {
    let area = overlay_area(frame.area());
    // Nothing here is selectable, but the overlay still claims its own area
    // so the wheel scrolls the help rather than the pane behind it.
    hits.push(area, super::Target::Help);
    let lines = build(app, Mode::Browser);

    // The visible height, after the border.
    let height = area.height.saturating_sub(2) as usize;
    // Clamp here rather than in the action handler, because only the renderer
    // knows how long the text turned out to be. This is also what makes
    // `MoveBottom`'s usize::MAX land on the last page instead of past it.
    let max_scroll = lines.len().saturating_sub(height);
    let scroll = scroll.min(max_scroll);

    let position = if max_scroll == 0 {
        String::new()
    } else {
        format!(" {}/{} ", scroll + 1, max_scroll + 1)
    };

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).scroll((scroll as u16, 0)).block(
            Block::bordered()
                .border_style(Style::default().fg(app.palette.border_focused))
                .title_top(Line::from(Span::styled(
                    " keybindings ",
                    Style::default()
                        .fg(app.palette.accent)
                        .add_modifier(Modifier::BOLD),
                )))
                .title_bottom(Line::from(vec![
                    Span::styled(position, Style::default().fg(app.palette.dim)),
                    Span::styled(
                        // The help screen listing the wrong keys would be the
                        // worst instance of all, since it is where somebody
                        // goes to recover from the others.
                        format!("{} ", scroll_and_close(app.keymaps.get(Mode::Help))),
                        Style::default().fg(app.palette.dim),
                    ),
                ])),
        ),
        area,
    );
}

/// The overlay's own two keys, which are in `Mode::Help` rather than the mode
/// being documented.
fn scroll_and_close(keymap: &crate::keys::Keymap) -> String {
    let scroll = [
        super::hint::key_for(keymap, Action::MoveDown),
        super::hint::key_for(keymap, Action::MoveUp),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("/");

    let mut parts = Vec::new();
    if !scroll.is_empty() {
        parts.push(format!("{scroll} scroll"));
    }
    if let Some(close) = super::hint::key_for(keymap, Action::Cancel) {
        parts.push(format!("{close} close"));
    }
    format!(" {}", parts.join(" \u{b7} "))
}

/// One line per action, grouped by [`Action::group`].
fn build<'a>(app: &App, mode: Mode) -> Vec<Line<'a>> {
    let keymap = app.keymaps.get(mode);
    let actions = ordered_actions(app, mode);

    // Width from the longest binding list rather than a fixed number, so a
    // rebinding cannot push a description into the key column. `h, left,
    // backspace` is already 18 characters with the defaults.
    let key_width = actions
        .iter()
        .map(|action| combo_list(keymap, *action).chars().count())
        .max()
        .unwrap_or(0)
        .max(8);

    let mut lines = Vec::new();
    let mut current_group = None;

    for action in actions {
        let group = action.group();
        if current_group != Some(group) {
            if current_group.is_some() {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled(
                format!("  {group}"),
                Style::default()
                    .fg(app.palette.accent)
                    .add_modifier(Modifier::BOLD),
            )));
            let note = group_note(group);
            for line in note {
                lines.push(Line::from(Span::styled(
                    format!("    {line}"),
                    Style::default().fg(app.palette.dim),
                )));
            }
            if !note.is_empty() {
                // Or the prose and the first binding read as one block.
                lines.push(Line::from(""));
            }
            current_group = Some(group);
        }

        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(
                format!(
                    "{:<width$}  ",
                    combo_list(keymap, action),
                    width = key_width
                ),
                Style::default()
                    .fg(app.palette.marked)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                action.describe().to_string(),
                Style::default().fg(app.palette.fg),
            ),
        ]));
    }

    lines
}

/// Prose under a group heading, where one line per key is not enough.
///
/// The sync group is the one that needs it. "Plan a mirror" and "plan a
/// two-way sync" describe what each key does without saying how the two
/// differ, and the difference is which files get deleted -- the single thing
/// somebody choosing between them needs to know, and the one they cannot
/// discover by trying it.
///
/// Lines rather than a paragraph: the overlay does not wrap, so the width is
/// decided here rather than by the terminal.
fn group_note(group: &str) -> &'static [&'static str] {
    match group {
        "Sync" => &[
            "Push and pull copy one way and never delete.",
            "Mirror is a push that also deletes: whatever is on",
            "the remote and not here is removed there.",
            "Two-way sends each side's changes to the other, and",
            "propagates a deletion once it has a snapshot of a",
            "previous run -- the first run only ever copies.",
            "Profile and watch need a sync profile named on the",
            "connection; both say so if there is none.",
        ],
        _ => &[],
    }
}

/// Every combo bound to `action`, so a user who added one sees both.
fn combo_list(keymap: &crate::keys::Keymap, action: Action) -> String {
    keymap
        .combos_for(action)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The actions of `mode`, sorted so that groups come out contiguous while
/// each group keeps its binding order.
fn ordered_actions(app: &App, mode: Mode) -> Vec<Action> {
    let mut actions = app.keymaps.get(mode).actions();

    // Group order is the order each group is first encountered, so the table
    // in `defaults.rs` decides the layout rather than an enum discriminant.
    let mut group_order: Vec<&'static str> = Vec::new();
    for action in &actions {
        if !group_order.contains(&action.group()) {
            group_order.push(action.group());
        }
    }

    actions.sort_by_key(|action| {
        group_order
            .iter()
            .position(|g| *g == action.group())
            .unwrap_or(usize::MAX)
    });
    actions
}

/// A centred box with a margin, so the panes stay visible around the edges.
fn overlay_area(area: Rect) -> Rect {
    let width = area.width.saturating_sub(area.width / 5).min(area.width);
    let height = area.height.saturating_sub(area.height / 6).min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn app() -> App {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().to_path_buf();
        App::new(Config::default(), here.clone(), here).unwrap()
    }

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    /// The question the overlay failed to answer: `sm` and `sb` both have a
    /// one-line description, and neither says which one deletes what.
    #[test]
    fn the_sync_group_explains_the_difference_between_its_modes() {
        let rendered = text(&build(&app(), Mode::Browser)).join("\n");

        assert!(
            rendered.contains("Mirror is a push that also deletes"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Two-way sends each side's changes"),
            "{rendered}"
        );
        assert!(
            rendered.contains("need a sync profile"),
            "the two keys that can do nothing without one should say so: {rendered}"
        );
    }

    /// The overlay does not wrap, so a note wider than the box would be cut
    /// off mid-sentence. The box is 80% of the terminal, and 80 columns is
    /// the narrow case worth holding to.
    #[test]
    fn no_note_is_wider_than_a_narrow_terminal_allows() {
        for group in ["Motion", "Sync", "Plan", "Marks", "General"] {
            for line in group_note(group) {
                assert!(
                    // Four columns of indent, two of border, and the 20% the
                    // overlay leaves around itself.
                    line.chars().count() + 6 <= 64,
                    "{line:?} would be truncated in an 80-column terminal"
                );
            }
        }
    }

    /// A group with nothing to add gets no blank line of its own.
    #[test]
    fn a_group_without_a_note_is_unchanged() {
        let rendered = text(&build(&app(), Mode::Browser));
        let marks = rendered.iter().position(|l| l == "  Marks").unwrap();

        assert!(
            rendered[marks + 1].contains("Mark or unmark"),
            "the first binding should follow the heading directly: {:?}",
            &rendered[marks..marks + 2]
        );
    }
}
