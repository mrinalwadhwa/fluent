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
            lines.push(Line::from(format!(
                "{marker}{} — {} [{}]",
                compact(&row.status.id, 18),
                compact(&row.status.title, 30),
                row.status.action
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
        lines.push(Line::from("Work Item read errors:"));
        lines.extend(app.snapshot.errors.iter().map(|e| Line::from(e.clone())));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((app.list_scroll, 0))
            .block(Block::default().borders(Borders::ALL).title(" Work Items ")),
        area,
    );
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
