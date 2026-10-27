//! The two file listings.

use ratatui::{
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Cell, Paragraph, Row, Table, TableState, Wrap},
    Frame,
};

use super::{format_size, shorten_path};
use crate::{
    app::App,
    browser::{Pane, PaneState},
    config::Palette,
    vfs::{Entry, EntryKind},
};

/// Draw both panes.
pub fn draw(frame: &mut Frame<'_>, app: &mut App, areas: [Rect; 2]) {
    if app.union_view {
        draw_union(frame, app, areas);
        return;
    }

    for (index, area) in areas.into_iter().enumerate() {
        draw_pane(frame, app, index, area);
    }
}

fn draw_pane(frame: &mut Frame<'_>, app: &mut App, index: usize, area: Rect) {
    let focused = index == app.focus;
    let block = pane_block(app, index, focused, area.width);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // A ghosted or failed pane has no rows to show; say why instead of
    // leaving an empty box that looks like an empty directory. The difference
    // matters: "this directory is empty" and "this directory is not here" are
    // very different facts when you are about to sync.
    let pane = &app.panes[index];
    if let Some(message) = placeholder(pane, inner.width) {
        frame.render_widget(
            Paragraph::new(message)
                .style(
                    Style::default()
                        .fg(app.palette.ghost)
                        .add_modifier(Modifier::ITALIC),
                )
                // Without wrapping, the explanation is cut off at the pane
                // border and the user sees a mystery empty box.
                .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }

    let rows: Vec<Row<'_>> = pane
        .entries()
        .iter()
        .map(|entry| entry_row(entry, pane.is_marked(&entry.name), &app.palette))
        .collect();

    let table = Table::new(rows, columns())
        .row_highlight_style(
            Style::default()
                .fg(app.palette.cursor_fg)
                .bg(app.palette.cursor_bg),
        )
        .highlight_symbol("");

    let mut state = TableState::default()
        .with_selected(Some(pane.cursor()))
        .with_offset(pane.scroll);

    frame.render_stateful_widget(table, inner, &mut state);

    // Carry the widget's scroll decision back, so the next frame starts where
    // this one ended rather than snapping to the top.
    app.panes[index].scroll = state.offset();
}

/// Union view: both panes show the same merged rows, each rendering its own
/// side's attributes and a marker for which sides the row exists on.
fn draw_union(frame: &mut Frame<'_>, app: &mut App, areas: [Rect; 2]) {
    let rows_data = app.union_rows();
    let cursor = app.union_cursor.min(rows_data.len().saturating_sub(1));

    for (index, area) in areas.into_iter().enumerate() {
        let focused = index == app.focus;
        let block = pane_block(app, index, focused, area.width);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let rows: Vec<Row<'_>> = rows_data
            .iter()
            .map(|row| {
                let marker = Span::styled(
                    row.origin().marker(),
                    Style::default().fg(if row.origin().is_one_sided() {
                        app.palette.warn
                    } else {
                        app.palette.dim
                    }),
                );

                match row.side(index) {
                    // Present on this side: render it normally, prefixed with
                    // the origin marker.
                    Some(entry) => {
                        let mut cells = entry_cells(entry, false, &app.palette);
                        cells.insert(0, Cell::from(Line::from(marker)));
                        Row::new(cells)
                    }
                    // Absent here: show the name dimmed with no size or date,
                    // so it is obvious the row is a hole on this side.
                    None => Row::new(vec![
                        Cell::from(Line::from(marker)),
                        Cell::from(Line::from(Span::styled(
                            // Trailing slash from the side that does have it,
                            // so a directory reads as a directory in both
                            // panes rather than looking like a file here.
                            if row.is_dir_like() {
                                format!("{}/", row.name)
                            } else {
                                row.name.clone()
                            },
                            Style::default()
                                .fg(app.palette.ghost)
                                .add_modifier(Modifier::ITALIC),
                        ))),
                        Cell::from(Line::from(Span::styled(
                            "\u{2014}",
                            Style::default().fg(app.palette.ghost),
                        ))),
                        Cell::from(Line::from(Span::styled(
                            "\u{2014}",
                            Style::default().fg(app.palette.ghost),
                        ))),
                    ]),
                }
            })
            .collect();

        let table = Table::new(rows, union_columns()).row_highlight_style(
            Style::default()
                .fg(app.palette.cursor_fg)
                .bg(app.palette.cursor_bg),
        );

        let mut state = TableState::default().with_selected(Some(cursor));
        frame.render_stateful_widget(table, inner, &mut state);
    }
}

/// What a pane is, taken from the pane rather than from which side it is on.
///
/// The right-hand pane used to be labelled `remote` unconditionally. With no
/// connection open it is a second local pane -- which is a useful thing to
/// have, it is how you sync two directories on one machine -- and calling it
/// `remote` while it listed `~` was the application asserting something
/// false about where those files were. On a tool whose entire job is copying
/// files between two places, a mislabelled side is not a cosmetic problem.
///
/// So the label is the backend's own name: `local`, `sftp`, `ftp`, `rclone`.
/// That is strictly more informative than `remote` as well -- the scheme was
/// not shown anywhere in the title before.
fn pane_label(pane: &crate::browser::pane::Pane) -> &str {
    pane.vfs.scheme()
}

fn pane_block<'a>(app: &App, index: usize, focused: bool, width: u16) -> Block<'a> {
    let pane = &app.panes[index];
    let palette = &app.palette;

    let border = if focused {
        palette.border_focused
    } else {
        palette.border
    };

    // Budget for the title: the box corners, the side label, and the
    // `[absent]` badge all take columns the path cannot have.
    let label = pane_label(pane);
    let reserved = 4 + label.len() + if pane.is_ghost() { 10 } else { 1 };
    let room = (width as usize).saturating_sub(reserved);

    let path = shorten_path(&pane.vfs.display_path(pane.path()), room);
    let mut title = vec![
        Span::styled(
            format!(" {label} "),
            Style::default()
                .fg(if focused { palette.accent } else { palette.dim })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(path, Style::default().fg(palette.fg)),
    ];

    if pane.is_ghost() {
        title.push(Span::styled(
            " [absent] ",
            Style::default().fg(palette.warn),
        ));
    } else {
        title.push(Span::raw(" "));
    }

    Block::bordered()
        .border_style(Style::default().fg(border))
        .title_top(Line::from(title))
}

/// The message shown instead of a listing, when there is no listing to show.
fn placeholder(pane: &Pane, width: u16) -> Option<String> {
    let room = width.saturating_sub(4) as usize;
    match &pane.state {
        PaneState::Ghost => Some(format!(
            "  {} does not exist on this side.\n\n  Synchronized browsing is still following \
             along: go back up and the panes re-couple.",
            // The path relative to the anchor, not the absolute one. It is
            // both shorter and the more useful fact: the absolute path is
            // already in the title, and what you want to know here is which
            // part of the shared tree is missing.
            pane.relative_to_sync_root().map_or_else(
                || shorten_path(&pane.path().display().to_string(), room),
                |relative| relative.display().to_string(),
            )
        )),
        PaneState::Failed(message) => Some(format!("  {message}")),
        PaneState::Listed if pane.is_empty() => Some("  (empty)".to_string()),
        PaneState::Listed => None,
    }
}

fn columns() -> [Constraint; 3] {
    [
        Constraint::Min(10),
        Constraint::Length(8),
        Constraint::Length(16),
    ]
}

fn union_columns() -> [Constraint; 4] {
    [
        Constraint::Length(2),
        Constraint::Min(10),
        Constraint::Length(8),
        Constraint::Length(16),
    ]
}

fn entry_row<'a>(entry: &Entry, marked: bool, palette: &Palette) -> Row<'a> {
    Row::new(entry_cells(entry, marked, palette))
}

fn entry_cells<'a>(entry: &Entry, marked: bool, palette: &Palette) -> Vec<Cell<'a>> {
    let name_color = match entry.kind {
        EntryKind::Dir => palette.dir,
        EntryKind::Symlink => palette.symlink,
        EntryKind::File if entry.is_executable() => palette.executable,
        EntryKind::File => palette.file,
    };

    let mut name = String::new();
    if marked {
        name.push('*');
    }
    name.push_str(&entry.name);
    if entry.is_dir_like() {
        name.push('/');
    }
    // A symlink shows where it points, which is the thing you want to know
    // before syncing one.
    if let Some(target) = &entry.symlink_target {
        name.push_str(" -> ");
        name.push_str(&target.display().to_string());
    }

    let name_style = if marked {
        Style::default()
            .fg(palette.marked)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(name_color)
    };

    let size = if entry.is_dir_like() {
        // Not "0 B": a directory's apparent size is meaningless, and showing
        // zero suggests it is empty.
        "\u{2014}".to_string()
    } else {
        format_size(entry.size)
    };

    // A backend with no timestamps renders a dash rather than a fabricated
    // date -- S3 listings have no mtime, and inventing one would make the
    // confirmation window's comparison look better founded than it is.
    let modified = entry.mtime.map_or_else(
        || "\u{2014}".to_string(),
        |time| time.format("%Y-%m-%d %H:%M").to_string(),
    );

    vec![
        Cell::from(Line::from(Span::styled(name, name_style))),
        Cell::from(Line::from(Span::styled(
            size,
            Style::default().fg(palette.dim),
        ))),
        Cell::from(Line::from(Span::styled(
            modified,
            Style::default().fg(palette.dim),
        ))),
    ]
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, Terminal};

    use ratatui::layout::Constraint;

    use crate::{app::App, config::Config};

    /// With no connection open, the right-hand pane is a second *local* pane.
    /// Labelling it `remote` while it lists your home directory is the
    /// application asserting something false about where those files are.
    #[test]
    fn an_unconnected_pane_is_not_labelled_remote() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().to_path_buf();
        let mut app = App::new(Config::default(), here.clone(), here).unwrap();

        let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
        terminal
            .draw(|frame| {
                let [left, right] = ratatui::layout::Layout::horizontal([
                    Constraint::Percentage(50),
                    Constraint::Percentage(50),
                ])
                .areas(frame.area());
                super::draw(frame, &mut app, [left, right]);
            })
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        let screen: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !screen.contains("remote"),
            "nothing here is remote, so nothing may say so:\n{screen}"
        );
        assert_eq!(
            screen.matches("local").count(),
            2,
            "both panes are local and both should say so:\n{screen}"
        );
    }
}
