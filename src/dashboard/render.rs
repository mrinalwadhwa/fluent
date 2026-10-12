use super::{
    app::{App, Pane},
    layout,
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
    if app.project_mode() {
        draw_projects(frame, app);
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
    } else if app.errors {
        errors(frame, chunks[1], app);
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

fn draw_projects(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(area);
    let total = app
        .snapshot
        .sources
        .iter()
        .flat_map(|source| &source.work)
        .count();
    let counts = super::project::Category::FILTERS
        .iter()
        .map(|category| {
            let count = app
                .snapshot
                .sources
                .iter()
                .flat_map(|source| &source.work)
                .filter(|work| work.category == *category)
                .count();
            format!("{} {count}", category.label())
        })
        .collect::<Vec<_>>();
    let unknown = app
        .snapshot
        .sources
        .iter()
        .flat_map(|source| &source.work)
        .filter(|work| work.category == super::project::Category::Unknown)
        .count();
    let first = format!(
        "{} {}  ·  {total} Work Items{}",
        app.filter.label(),
        app.project_keys()
            .iter()
            .filter(|key| key.task_id.is_none())
            .count(),
        if unknown > 0 {
            format!(" · Unknown {unknown}")
        } else {
            String::new()
        }
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(first),
            Line::from(counts[..4].join("  ")),
            Line::from(counts[4..].join("  ")),
        ])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Fluent · Projects "),
        ),
        chunks[0],
    );
    if app.help {
        help(frame, chunks[1], app);
    } else if app.errors {
        errors(frame, chunks[1], app);
    } else if area.width >= 100 {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(43), Constraint::Percentage(57)])
            .split(chunks[1]);
        project_list(frame, panes[0], app);
        project_detail(frame, panes[1], app);
    } else if app.pane == Pane::List {
        project_list(frame, chunks[1], app);
    } else {
        project_detail(frame, chunks[1], app);
    }
    let controls = if app.help {
        "? or Esc close help"
    } else if app.errors {
        "j/k scroll  PgUp/PgDn page  e or Esc return  ? help  q quit"
    } else if app.pane == Pane::Detail && area.width < 100 {
        "Esc/Tab list  j/k scroll  ? help  q quit"
    } else if area.width < 100 {
        "j/k select  Enter expand  Tab detail  f filter  ? help  q quit"
    } else {
        "j/k select  Enter expand  f filter  a history  e errors  r refresh  ? help  q quit"
    };
    frame.render_widget(Paragraph::new(controls), chunks[2]);
}

fn project_list(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines = Vec::new();
    for source in &app.snapshot.sources {
        let work = source
            .work
            .iter()
            .filter(|work| app.filter.matches(work.category))
            .collect::<Vec<_>>();
        if work.is_empty() {
            continue;
        }
        let parent = source
            .path
            .parent()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy())
            .unwrap_or_default();
        lines.push(Line::styled(
            format!(
                "{} · {parent}/{} · {}{}",
                source.short_id(),
                source.project,
                source.host,
                if source.stale.is_some() {
                    " · STALE"
                } else {
                    ""
                }
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        for item in work {
            let selected = app.selected_key.as_ref() == Some(&item.key);
            let marker = if selected { ">" } else { " " };
            let triangle = if app.expanded.contains(&item.key) {
                "▾"
            } else {
                "▸"
            };
            lines.push(Line::from(format!(
                "{marker} {triangle} {}  {} [{}]",
                item.status.title,
                item.phase.as_deref().unwrap_or(item.category.label()),
                item.status.id
            )));
            if app.expanded.contains(&item.key) {
                for worker in &item.workers {
                    let marker = if app.selected_key.as_ref() == Some(&worker.key) {
                        ">"
                    } else {
                        " "
                    };
                    lines.push(Line::from(format!(
                        "{marker}   {} · {} [{}]",
                        worker.role,
                        worker.provider.as_deref().unwrap_or("unavailable"),
                        worker.state
                    )));
                }
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from("No Work Items in this filter"));
    }
    frame.render_widget(
        Paragraph::new(lines).scroll((app.list_scroll, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Projects / Work / Workers "),
        ),
        area,
    );
}

fn project_detail(frame: &mut Frame, area: Rect, app: &App) {
    let values = app
        .selected_key
        .as_ref()
        .map(|key| layout::project_detail_values(app, key))
        .unwrap_or_else(|| vec!["No Work Item selected".into()]);
    frame.render_widget(
        Paragraph::new(values.into_iter().map(Line::from).collect::<Vec<_>>())
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Selected details / terminal "),
            ),
        area,
    );
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
            let available = width.saturating_sub(8) as usize;
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
fn detail_lines(row: &DashboardRow) -> Vec<Line<'static>> {
    let status = &row.status;
    let mut lines = vec![Line::styled(
        status.title.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    )];
    lines.extend(
        layout::detail_values(row)
            .into_iter()
            .skip(1)
            .map(Line::from),
    );
    lines
}
fn errors(frame: &mut Frame, area: Rect, app: &App) {
    let lines = layout::error_lines(&app.snapshot, area.width)
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).scroll((app.error_scroll, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Work Item read errors "),
        ),
        area,
    );
}

fn help(frame: &mut Frame, area: Rect, app: &App) {
    let text = if app.project_mode() {
        "j/k select; Enter expand; Tab details/list; Esc list; f next filter; a history; e errors; r refresh; c copy; ? close; q quit"
    } else if app.size.0 >= 100 {
        "j/k or arrows select; PgUp/PgDn list; a Current/All; e errors; r refresh; c copy; ? close; q quit"
    } else {
        "Enter detail; Esc list; j/k select or scroll; PgUp/PgDn list; a Current/All; e errors; r refresh; ? close; q quit"
    };
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" Help ")),
        area,
    );
}
fn bar(frame: &mut Frame, area: Rect, app: &App) {
    let text = if app.help {
        "? or Esc close help"
    } else if app.errors {
        "j/k scroll  PgUp/PgDn page  e or Esc return  ? help  q quit"
    } else if app.size.0 < 100 && app.pane == Pane::Detail {
        "Esc list  j/k scroll  ? help  q quit"
    } else {
        "j/k select  PgUp/PgDn list  a all  e errors  r refresh  ? help  c copy  q quit"
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
