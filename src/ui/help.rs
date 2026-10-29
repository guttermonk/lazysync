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

    // The same, for the column of action names. Measured over the actions
    // actually listed rather than over every action there is, so a keymap
    // that binds none of the long ones gets a narrower table.
    let name_width = actions
        .iter()
        .map(|action| action.name().chars().count())
        .max()
        .unwrap_or(0);

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
            // The action's name, which is deliberately the exact spelling
            // `[keys.*]` accepts rather than a prettified one. This is the
            // screen somebody is on when they decide to rebind something, and
            // `SyncMirror` can be typed straight into `set = [["SyncMirror",
            // "sm"]]` -- "Sync Mirror" would have to be translated first, and
            // the error for getting that wrong is a key that does nothing.
            Span::styled(
                format!("{:<width$}  ", action.name(), width = name_width),
                Style::default().fg(app.palette.accent),
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

/// A box with a margin, so the panes stay visible around the edges.
///
/// **Deliberately not centred vertically.** The frame's last three rows are
/// the panes' bottom border, the key bar and the status line, and a centred
/// overlay landed its own bottom border exactly on the first of those: at 25
/// rows the old arithmetic gave rows 2..=22, and the panes draw their border
/// on row 22. Two borders of the same colour on the same row read as one
/// shape with a seam in it rather than as a window over a screen, which is
/// how this was reported.
///
/// So the margin below is larger than the margin above, and the gap is in
/// rows rather than in a fraction of the height -- the thing being cleared is
/// a fixed number of rows of furniture, so a proportion would go on
/// overlapping in exactly the small terminals where it is tightest.
fn overlay_area(area: Rect) -> Rect {
    /// Above the box: enough to see which panes are behind it.
    const TOP: u16 = 2;
    /// Below it: the panes' bottom border, the key bar, the status line, and
    /// one blank row so the two borders are not merely adjacent either.
    const BOTTOM: u16 = 4;

    let width = area.width.saturating_sub(area.width / 5).max(1);

    // The margins give way before the content does: in a terminal too short
    // for both, a readable box beats a tidy gap.
    let height = area
        .height
        .saturating_sub(TOP + BOTTOM)
        .max(3)
        .min(area.height);

    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + area.height.saturating_sub(height).min(TOP),
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

    /// The pane border at the very corner of the frame, which no overlay ever
    /// covers -- so whatever happens to it is dimming rather than occlusion.
    mod dimming {
        use ratatui::{
            backend::TestBackend,
            buffer::Buffer,
            style::{Color, Modifier},
            Terminal,
        };

        use crate::{
            app::{App, Overlay, Prompt, PromptKind},
            config::Config,
        };

        fn frame_with(overlay: Overlay) -> (Buffer, App) {
            let dir = tempfile::tempdir().unwrap();
            let here = dir.path().to_path_buf();
            let mut app = App::new(Config::default(), here.clone(), here).unwrap();
            app.overlay = overlay;

            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            (buffer, app)
        }

        /// Reported as the help overlay's border and the panes' borders being
        /// the same colour and reading as one shape. With the panes muted,
        /// the only thing at full strength is the window in front.
        #[test]
        fn the_panes_behind_the_help_overlay_are_muted() {
            let (plain, app) = frame_with(Overlay::None);
            assert_eq!(
                plain[(0, 0)].fg,
                app.palette.border_focused,
                "the focused pane's border is normally the accent colour"
            );

            let (dimmed, app) = frame_with(Overlay::Help { scroll: 0 });
            assert_eq!(dimmed[(0, 0)].fg, app.palette.dim);
        }

        /// Bold and reverse are how the cursor row and the focused border
        /// shout. A background still shouting is the whole complaint.
        #[test]
        fn nothing_behind_a_modal_keeps_a_loud_modifier() {
            let (buffer, _) = frame_with(Overlay::Help { scroll: 0 });

            // The left edge, top to bottom: entirely outside an overlay that
            // is 80% of the width and centred.
            for y in 0..buffer.area.height {
                let cell = &buffer[(0, y)];
                assert!(
                    !cell.modifier.contains(Modifier::BOLD)
                        && !cell.modifier.contains(Modifier::REVERSED),
                    "row {y} is still emphasised behind the overlay"
                );
            }
        }

        /// On the default `terminal` preset the background is `Reset`, which
        /// is what lets a transparent terminal show through. Dimming must not
        /// trade that for a grey rectangle.
        #[test]
        fn a_transparent_background_stays_transparent() {
            let (buffer, app) = frame_with(Overlay::Help { scroll: 0 });

            assert_eq!(app.palette.bg, Color::Reset, "the default preset");
            assert_eq!(buffer[(0, 0)].bg, Color::Reset);
        }

        /// The password window is a window too, so it gets the same
        /// treatment -- this is "dim the background for any popup", not "for
        /// the help overlay".
        #[test]
        fn the_password_window_dims_what_is_behind_it() {
            let (buffer, app) = frame_with(Overlay::Prompt(Prompt {
                kind: PromptKind::Password {
                    bookmark: "sandbox".into(),
                },
                input: String::new(),
            }));

            assert_eq!(buffer[(0, 0)].fg, app.palette.dim);
        }

        /// And search does not, because a search is read *against* the
        /// listing it is filtering. Dimming that would hide the thing being
        /// searched, which is the opposite of helping.
        #[test]
        fn a_search_prompt_leaves_the_listing_alone() {
            let (buffer, app) = frame_with(Overlay::Prompt(Prompt {
                kind: PromptKind::Search,
                input: "arm".into(),
            }));

            assert_eq!(buffer[(0, 0)].fg, app.palette.border_focused);
        }
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

    /// The requested column, and the order the three read in: key, what the
    /// command is called, what it does.
    #[test]
    fn every_binding_names_its_action_between_the_key_and_the_description() {
        let rendered = text(&build(&app(), Mode::Browser));
        let row = rendered
            .iter()
            .find(|line| line.contains("SyncMirror"))
            .unwrap_or_else(|| panic!("no SyncMirror row in {rendered:#?}"));

        let key = row.find("sm").expect("the key");
        let name = row.find("SyncMirror").expect("the name");
        let description = row.find("One-way").expect("the description");

        assert!(
            key < name && name < description,
            "the columns are out of order: {row:?}"
        );
    }

    /// The spelling is the one `[keys.*]` accepts, because this is the screen
    /// you are on when you decide to rebind something. `Sync Mirror` would
    /// have to be translated before it could be typed into a config.
    #[test]
    fn the_action_column_is_the_name_config_accepts() {
        let rendered = text(&build(&app(), Mode::Browser)).join("\n");

        assert!(rendered.contains("SyncMirror"), "{rendered}");
        assert!(
            !rendered.contains("Sync Mirror"),
            "a prettified name cannot be pasted into a keymap: {rendered}"
        );
    }

    /// The column is a column: every description in a group starts in the
    /// same place, however long the name beside it is.
    #[test]
    fn the_descriptions_line_up_under_each_other() {
        let rendered = text(&build(&app(), Mode::Browser));

        let starts: Vec<usize> = rendered
            .iter()
            .filter(|line| line.contains("SyncMirror") || line.contains("SyncPush"))
            .map(|line| {
                line.find("One-way")
                    .unwrap_or_else(|| panic!("no description in {line:?}"))
            })
            .collect();

        assert_eq!(starts.len(), 2, "both sync rows should be found");
        assert_eq!(starts[0], starts[1], "the description column is ragged");
    }

    /// The reported collision: a centred overlay put its bottom border on the
    /// row the panes draw theirs on, and two borders of one colour on one row
    /// read as a single shape.
    #[test]
    fn the_overlay_stops_short_of_the_panes_bottom_border() {
        for height in 12..=60u16 {
            let area = overlay_area(Rect {
                x: 0,
                y: 0,
                width: 100,
                height,
            });
            let last = area.y + area.height - 1;
            // The panes' bottom border, then the key bar, then the status
            // line -- the three rows an overlay must not reach.
            let panes_border = height - 3;

            assert!(
                last < panes_border,
                "at {height} rows the overlay ends on row {last} and the panes' \
                 border is on row {panes_border}"
            );
        }
    }

    /// The margins are what give way in a short terminal, not the box.
    #[test]
    fn a_short_terminal_still_gets_a_readable_box() {
        for height in 4..12u16 {
            let area = overlay_area(Rect {
                x: 0,
                y: 0,
                width: 100,
                height,
            });
            assert!(area.height >= 3, "at {height} rows the box collapsed");
            assert!(
                area.y + area.height <= height,
                "at {height} rows the box is drawn outside the frame"
            );
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
