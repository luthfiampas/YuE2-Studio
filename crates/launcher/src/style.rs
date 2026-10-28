//! The launcher's visual rules in one place: panels, dialogs, hint bars and
//! table rows, so every screen shares the same border, padding and colour
//! language instead of each one inventing its own.

use crate::theme::current as theme;
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Padding, Paragraph, Row},
};

/// Space between the dashboard's main regions.
pub const GUTTER: u16 = 1;

fn title_line(text: impl Into<String>) -> Line<'static> {
    let text: String = text.into();
    Line::from(format!(" {} ", text.trim())).style(
        Style::default()
            .fg(theme().accent)
            .add_modifier(Modifier::BOLD),
    )
}

fn frame(title: impl Into<String>, border: Color, padding: Padding) -> Block<'static> {
    Block::default()
        .title(title_line(title))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .padding(padding)
}

/// A main region: rounded border, accent title, one cell of inner padding.
pub fn panel(title: impl Into<String>, border: Color) -> Block<'static> {
    frame(title, border, Padding::horizontal(1))
}

/// A dialog: the same border language as a panel, with room to breathe.
pub fn modal(title: impl Into<String>, border: Color) -> Block<'static> {
    frame(title, border, Padding::new(2, 2, 1, 1))
}

/// The single-line hint bar that sits under a table, list or dialog.
pub fn hints(items: &[(&str, &str)]) -> Paragraph<'static> {
    let mut spans = Vec::with_capacity(items.len() * 2);
    for (index, (key, label)) in items.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(
            format!("[{}]", key),
            Style::default()
                .fg(theme().accent)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {}", label),
            Style::default().fg(theme().text_dim),
        ));
    }
    Paragraph::new(Line::from(spans)).style(Style::default().bg(theme().surface))
}

/// Table header: the one style used by every table in the launcher.
pub fn table_header(cells: &[&str]) -> Row<'static> {
    let cells: Vec<Cell<'static>> = cells
        .iter()
        .map(|cell| {
            Cell::from(format!(" {}", cell)).style(
                Style::default()
                    .fg(theme().accent)
                    .add_modifier(Modifier::BOLD),
            )
        })
        .collect();
    Row::new(cells)
        .height(1)
        .bottom_margin(1)
        .style(Style::default().bg(theme().surface_high))
}

/// Table row with the shared stripe and selection backgrounds.
pub fn table_row(index: usize, selected: bool, cells: Vec<Cell<'static>>) -> Row<'static> {
    let background = if selected {
        theme().selection_bg_alt
    } else if index.is_multiple_of(2) {
        theme().row_even
    } else {
        theme().row_odd
    };
    Row::new(cells).style(Style::default().bg(background))
}

/// The selection marker used by every list and table.
pub fn marker(selected: bool) -> &'static str {
    if selected { "▶ " } else { "  " }
}

/// Width every `label` call pads to, so fields line up down a panel.
pub const LABEL_WIDTH: usize = 13;

/// A field label: muted, colon-terminated, padded to `LABEL_WIDTH`.
/// It carries no leading space: the panel's padding already supplies the
/// inset, so every line of body text starts in the same column.
pub fn label(text: &str) -> Span<'static> {
    Span::styled(
        format!("{:<width$}", format!("{}:", text), width = LABEL_WIDTH),
        Style::default().fg(theme().muted),
    )
}

/// A section heading inside a panel.
pub fn section(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default()
            .fg(theme().label_muted)
            .add_modifier(Modifier::BOLD),
    ))
}

/// The gap between two label/value pairs on one line.
pub fn column_gap() -> Span<'static> {
    Span::raw("  ")
}
