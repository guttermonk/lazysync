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

pub fn draw(frame: &mut Frame<'_>, app: &App, scroll: usize) {
    let area = overlay_area(frame.area());
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
