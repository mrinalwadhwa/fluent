use super::{
    app::{App, Pane},
    snapshot::DashboardRow,
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollBounds {
    pub list_height: u16,
    pub list_max_scroll: u16,
    pub detail_max_scroll: u16,
}

pub fn scroll_bounds(app: &App) -> ScrollBounds {
    if app.size.0 < 60 || app.size.1 < 15 {
        return ScrollBounds::default();
    }
    let body_height = app.size.1.saturating_sub(4);
    let inner_height = body_height.saturating_sub(2);
    let list_width = if app.size.0 >= 100 {
        app.size.0.saturating_mul(43) / 100
    } else {
        app.size.0
    }
    .saturating_sub(2);
    let detail_width = if app.size.0 >= 100 {
        app.size
            .0
            .saturating_sub(app.size.0.saturating_mul(43) / 100)
    } else {
        app.size.0
    }
    .saturating_sub(2);
    let list_lines = list_lines(app, list_width).len() as u16;
    let detail_lines = wrapped_detail_line_count(app, detail_width);
    ScrollBounds {
        list_height: inner_height,
        list_max_scroll: list_lines.saturating_sub(inner_height),
        detail_max_scroll: detail_lines.saturating_sub(inner_height),
    }
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < 60 || area.height < 15 {
        frame.render_widget(
            Paragraph::new("Resize terminal to at least 60 columns and 15 rows.  q quit").block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Fluent Dashboard "),
            ),
            area,
        );
        return;
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(area);
    header(frame, chunks[0], app);
    if app.help {
        help(frame, chunks[1], app);
    } else if area.width >= 100 {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(43), Constraint::Percentage(57)])
            .split(chunks[1]);
        list(frame, panes[0], app);
        detail(frame, panes[1], app);
    } else if app.pane == Pane::List {
        list(frame, chunks[1], app);
    } else {
        detail(frame, chunks[1], app);
    }
    bar(frame, chunks[2], app);
}
fn header(frame: &mut Frame, area: Rect, app: &App) {
    let visible = app.snapshot.visible_rows(app.all_work()).len();
    let stale = app
        .stale_error
        .as_ref()
        .map(|e| format!(" STALE: {e}"))
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                if app.all_work() {
                    "All Work"
                } else {
                    "Current Work"
                },
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(" ({visible}){stale}")),
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Fluent Dashboard "),
        ),
        area,
    );
}
fn list(frame: &mut Frame, area: Rect, app: &App) {
    let lines = list_lines(app, area.width.saturating_sub(2));
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.list_scroll, 0))
            .block(Block::default().borders(Borders::ALL).title(" Work Items ")),
        area,
    );
}
fn list_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (group, rows) in app.snapshot.groups(app.all_work()) {
        lines.push(Line::styled(
            format!("{} ({})", group.title(), rows.len()),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        for row in rows {
            let marker = if app.selected_id() == Some(row.status.id.as_str()) {
                "> "
            } else {
                "  "
            };
            let available = width.saturating_sub(6) as usize;
            let id_width = available.min(14).max(1);
            let action_width = available.saturating_sub(id_width).min(12).max(1);
            let title_width = available.saturating_sub(id_width + action_width).max(1);
            lines.push(Line::from(format!(
                "{marker}{} — {} [{}]",
                compact(&row.status.id, id_width),
                compact(&row.status.title, title_width),
                compact(&row.status.action, action_width)
            )));
        }
    }
    if lines.is_empty() {
        let message = if app.snapshot.rows.is_empty() {
            "No Work Items found"
        } else {
            "No Current Work. Press a for All Work."
        };
        lines.push(Line::from(message));
    }
    if !app.snapshot.errors.is_empty() {
        lines.push(Line::from(format!(
            "Work Item read errors ({}):",
            app.snapshot.errors.len()
        )));
        lines.extend(app.snapshot.errors.iter().map(|e| Line::from(e.clone())));
    }
    lines
}
fn detail(frame: &mut Frame, area: Rect, app: &App) {
    let selected = app
        .snapshot
        .visible_rows(app.all_work())
        .into_iter()
        .find(|row| Some(row.status.id.as_str()) == app.selected_id());
    let lines = match selected {
        Some(row) => detail_lines(row),
        None => vec![Line::from("No Work Item selected")],
    };
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Work Item detail "),
            ),
        area,
    );
}
fn wrapped_detail_line_count(app: &App, width: u16) -> u16 {
    let Some(row) = app
        .selected_id()
        .and_then(|id| app.snapshot.rows.iter().find(|row| row.status.id == id))
    else {
        return 1;
    };
    let width = width.max(1) as usize;
    detail_lines(row)
        .iter()
        .map(|line| {
            let value = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            UnicodeWidthStr::width(value.as_str())
                .max(1)
                .div_ceil(width)
        })
        .sum::<usize>() as u16
}
fn detail_lines(row: &DashboardRow) -> Vec<Line<'static>> {
    let status = &row.status;
    let mut lines = vec![
        Line::styled(
            status.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::from(format!("ID: {}", status.id)),
        Line::from(format!("Action: {}", status.action)),
        Line::from(format!("Attempt: {}", status.attempt)),
        Line::from(format!("Task: {}", status.task)),
        Line::from(format!("Review: {}", status.review)),
        Line::from(format!("Merge Candidate: {}", status.merge_candidate)),
        Line::from(format!("Merge: {}", status.merge)),
        Line::from(format!(
            "Metrics: reviews {}, input {}, output {}",
            status.metrics.review_rounds, status.metrics.input_tokens, status.metrics.output_tokens
        )),
    ];
    for warning in &status.compatibility_warnings {
        lines.push(Line::from(format!("Warning: {warning}")));
    }
    if let Some(release) = &status.release {
        lines.push(Line::from(format!(
            "Release: {} criteria, {} blockers",
            release.criteria, release.blockers
        )));
    }
    lines.push(Line::from(
        row.next_action
            .clone()
            .unwrap_or_else(|| "No operator action".into()),
    ));
    lines
}
fn help(frame: &mut Frame, area: Rect, app: &App) {
    let text = if app.size.0 >= 100 {
        "j/k or arrows select; a Current/All; r refresh; c copy; ? close; q quit"
    } else {
        "Enter detail; Esc list; j/k scroll; a Current/All; r refresh; ? close; q quit"
    };
    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(" Help ")),
        area,
    );
}
fn bar(frame: &mut Frame, area: Rect, app: &App) {
    let text = if app.help {
        "? or Esc close help"
    } else if app.size.0 < 100 && app.pane == Pane::Detail {
        "Esc list  j/k scroll  ? help  q quit"
    } else {
        "j/k select  a all  r refresh  ? help  c copy  q quit"
    };
    frame.render_widget(Paragraph::new(text), area);
}
fn compact(value: &str, width: usize) -> String {
    if UnicodeWidthStr::width(value) <= width {
        return value.into();
    }
    let mut out = String::new();
    let mut used = 0;
    for character in value.chars() {
        let size = UnicodeWidthStr::width(character.encode_utf8(&mut [0; 4]));
        if used + size + 1 > width {
            break;
        }
        out.push(character);
        used += size;
    }
    out.push('…');
    out
}
