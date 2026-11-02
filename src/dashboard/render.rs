use super::{
    layout,
    model::{App, Pane},
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
            Paragraph::new("Resize terminal to 60 columns and 15 rows. q quit").block(
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
    let areas = layout::project_areas(area);
    let mut tabs = Vec::new();
    let mut selected_end = 0;
    for view in super::project::Filter::VIEWS {
        if !tabs.is_empty() {
            tabs.push(Span::raw("  "));
        }
        let count = app
            .snapshot
            .sources
            .iter()
            .flat_map(|source| &source.work)
            .filter(|work| view.matches(work.category))
            .count();
        let style = if app.filter == view {
            Style::default()
                .fg(Color::White)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        tabs.push(Span::styled(format!("{} {count}", view.label()), style));
        if app.filter == view {
            selected_end = tabs.iter().map(Span::width).sum::<usize>();
        }
    }
    let tab_line = Line::from(tabs);
    let hint = "Tab/⇧Tab view";
    let mut tab_line = tab_line;
    if tab_line.width() + hint.width() + 2 <= area.width as usize {
        tab_line.spans.push(Span::raw("  "));
        tab_line
            .spans
            .push(Span::styled(hint, Style::default().fg(Color::DarkGray)));
    }
    let position = if area.width < 100 {
        if app.pane == Pane::List {
            " · List"
        } else {
            " · Details · ←/Esc list"
        }
    } else {
        ""
    };
    let mut title = format!("Fluent work{position}");
    if !app.terminal_visible() {
        if let Some(display) = &app.terminal {
            title.push_str(&format!(" · {}", display.state));
        }
    }
    if app.local_read.pending {
        title.push_str(" · Loading local Work");
    }
    if let Some(error) = &app.stale_error {
        title.push_str(&format!(" · {error}"));
    }
    frame.render_widget(Block::default().borders(Borders::BOTTOM), areas.header);
    frame.render_widget(
        Paragraph::new(title),
        Rect {
            height: 1,
            ..areas.header
        },
    );
    frame.render_widget(
        Paragraph::new(tab_line)
            .scroll((0, selected_end.saturating_sub(area.width as usize) as u16)),
        Rect {
            y: areas.header.y + 1,
            height: 1,
            ..areas.header
        },
    );
    if app.help {
        help(frame, areas.body, app);
    } else if app.errors {
        errors(frame, areas.body, app);
    } else if area.width >= 100 {
        frame.render_widget(
            Paragraph::new(vec![Line::from("│"); areas.divider.height as usize]),
            areas.divider,
        );
        project_list(frame, areas.list, app);
        project_detail(frame, areas.detail, app);
    } else if app.pane == Pane::List {
        project_list(frame, areas.list, app);
    } else {
        project_detail(frame, areas.detail, app);
    }
    let controls = if app.help {
        "? or Esc close help"
    } else if app.errors {
        "↑/↓ scroll  PgUp/PgDn page  e/Esc back  ? help  q quit"
    } else if app.terminal_visible() {
        "←/Esc back  i Control  f full-screen  ? help  q quit"
    } else if app.pane == Pane::Detail && area.width < 100 {
        "←/Esc list  ↑/↓ scroll  Tab view  ? help  q quit"
    } else if area.width < 100 {
        "↑/↓ select  Enter workers  → details  ? help  q quit"
    } else {
        "↑/↓ select  Enter workers  → details/session  ← back  ? help  q quit"
    };
    frame.render_widget(Paragraph::new(controls), areas.footer);
}

fn updated_time(value: Option<&str>) -> String {
    value
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|time| {
            time.with_timezone(&chrono::Utc)
                .format("%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "—".into())
}

fn project_row(
    title: &str,
    status: &str,
    updated: &str,
    marker: &str,
    width: u16,
    selected: bool,
    tone: Color,
) -> Line<'static> {
    let updated_width = if width >= 60 { 12 } else { 0 };
    let title_width = usize::from(width).saturating_sub(4 + 14 + updated_width);
    let title = compact(title, title_width);
    let mut spans = vec![
        Span::raw(format!(
            "{marker} {title}{}  ",
            " ".repeat(title_width.saturating_sub(title.width()))
        )),
        Span::raw(format!("{status:<14}")),
    ];
    if updated_width > 0 {
        spans.push(Span::raw(format!("{updated:>12}")));
    }
    Line::from(spans).style(if selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Rgb(74, 165, 239))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(tone)
    })
}

fn project_list(frame: &mut Frame, area: Rect, app: &App) {
    let columns = project_row(
        "Tasks",
        "Status",
        "Updated UTC",
        " ",
        area.width,
        false,
        Color::DarkGray,
    );
    frame.render_widget(Paragraph::new(columns), Rect { height: 1, ..area });
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
        // Keep staleness at the front so a long source name cannot hide it.
        lines.push(Line::styled(
            format!(
                "{}{} · {parent}/{} · {}",
                if source.stale.is_some() {
                    "STALE · "
                } else {
                    ""
                },
                source.short_id(),
                source.project,
                source.host
            ),
            Style::default().fg(Color::DarkGray),
        ));
        for item in work {
            use super::project::Category;
            let tone = match item.category {
                Category::NeedsYou => Color::LightRed,
                Category::Running => Color::LightBlue,
                Category::Ready | Category::ReadyToLand => Color::LightGreen,
                _ => Color::Gray,
            };
            let triangle = if app.expanded.contains(&item.key) {
                "▾"
            } else {
                "▸"
            };
            let selected = app.selected_key.as_ref() == Some(&item.key);
            lines.push(project_row(
                &format!("{triangle} {}", item.status.title),
                item.category.label(),
                &updated_time(item.updated_at.as_deref()),
                if selected { ">" } else { " " },
                area.width,
                selected,
                tone,
            ));
            if app.expanded.contains(&item.key) {
                for worker in &item.workers {
                    let selected = app.selected_key.as_ref() == Some(&worker.key);
                    let available = worker.terminal.as_ref().is_some_and(|t| t.can_observe);
                    lines.push(project_row(
                        &format!(
                            "  ↳ {} · {}",
                            worker.role,
                            if available { "session" } else { "no session" }
                        ),
                        &worker.state,
                        &updated_time(worker.updated_at.as_deref()),
                        if selected { ">" } else { " " },
                        area.width,
                        selected,
                        Color::Gray,
                    ));
                }
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from("No Work Items in this view"));
    }
    frame.render_widget(
        Paragraph::new(lines).scroll((app.list_scroll, 0)),
        Rect {
            y: area.y + 1,
            height: area.height.saturating_sub(1),
            ..area
        },
    );
}

fn project_detail(frame: &mut Frame, area: Rect, app: &App) {
    if let Some(display) = &app.terminal {
        crate::terminal_view::draw(frame, area, display);
        return;
    }
    let values = app
        .selected_key
        .as_ref()
        .map(|key| layout::project_detail_values(&app.snapshot.sources, key))
        .unwrap_or_else(|| vec!["No Work Item selected".into()]);
    frame.render_widget(
        Paragraph::new(
            layout::project_detail_lines(&values, area.width)
                .into_iter()
                .map(Line::from)
                .collect::<Vec<_>>(),
        )
        .scroll((app.detail_scroll, 0)),
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
    let width = if app.project_mode() {
        area.width.saturating_add(2)
    } else {
        area.width
    };
    let lines = layout::error_lines(&app.snapshot, width)
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();
    if app.project_mode() {
        frame.render_widget(
            Paragraph::new("Work Item read errors"),
            Rect { height: 1, ..area },
        );
        frame.render_widget(
            Paragraph::new(lines).scroll((app.error_scroll, 0)),
            Rect {
                y: area.y + 1,
                height: area.height.saturating_sub(1),
                ..area
            },
        );
    } else {
        frame.render_widget(
            Paragraph::new(lines).scroll((app.error_scroll, 0)).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Work Item read errors "),
            ),
            area,
        );
    }
}

fn help(frame: &mut Frame, area: Rect, app: &App) {
    let text = if app.project_mode() {
        "↑/↓ or j/k select; Enter workers; Tab/Shift+Tab view; → details/session; ←/Esc list; t terminal/reconnect; i Control; Ctrl-] navigate; ] literal; o Observe; f full-screen (open view); f/F next view; a History/All; e errors; r refresh; c copy; ? close; q quit"
    } else if app.size.0 >= 100 {
        "j/k or arrows select; PgUp/PgDn list; a Current/All; e errors; r refresh; c copy; ? close; q quit"
    } else {
        "Enter detail; Esc list; j/k select or scroll; PgUp/PgDn list; a Current/All; e errors; r refresh; ? close; q quit"
    };
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    if app.project_mode() {
        frame.render_widget(Paragraph::new("Help"), Rect { height: 1, ..area });
        frame.render_widget(
            paragraph,
            Rect {
                y: area.y + 1,
                height: area.height.saturating_sub(1),
                ..area
            },
        );
    } else {
        frame.render_widget(
            paragraph.block(Block::default().borders(Borders::ALL).title(" Help ")),
            area,
        );
    }
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
