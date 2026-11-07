use super::project::{self, Filter, RowKey, SourceSnapshot};
use super::snapshot::{DashboardRow, DashboardSnapshot};
use unicode_width::UnicodeWidthStr;

pub fn terminal_size(size: (u16, u16), identity: &str) -> (u16, u16) {
    let content = terminal_content(size, identity);
    (content.width.clamp(1, 500), content.height.clamp(1, 200))
}

/// Share project geometry between drawing, scrolling, and terminal input.
pub struct ProjectAreas {
    pub header: ratatui::layout::Rect,
    pub body: ratatui::layout::Rect,
    pub footer: ratatui::layout::Rect,
    pub health: ratatui::layout::Rect,
    pub list: ratatui::layout::Rect,
    pub divider: ratatui::layout::Rect,
    pub detail: ratatui::layout::Rect,
    pub focused_detail: ratatui::layout::Rect,
}

pub fn project_areas(area: ratatui::layout::Rect) -> ProjectAreas {
    use ratatui::layout::{Constraint, Layout, Rect};
    let rows = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    let body = rows[1];
    let (list, divider, detail) = if area.width >= 100 {
        // Give terminal output more room than the static prototype's 73% list.
        let panes = Layout::horizontal([
            Constraint::Percentage(65),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(body);
        (
            Rect {
                width: panes[0].width.saturating_sub(1),
                ..panes[0]
            },
            panes[1],
            Rect {
                x: panes[2].x + 1,
                width: panes[2].width.saturating_sub(1),
                ..panes[2]
            },
        )
    } else {
        (body, Rect::default(), body)
    };
    ProjectAreas {
        header: rows[0],
        body,
        health: rows[2],
        footer: rows[3],
        list,
        divider,
        detail,
        focused_detail: body,
    }
}

/// Reserve a fixed feedback row without changing the worker terminal viewport.
pub fn project_detail_content(
    area: ratatui::layout::Rect,
    feedback: bool,
) -> ratatui::layout::Rect {
    let reserved = u16::from(feedback).min(area.height);
    ratatui::layout::Rect {
        y: area.y + reserved,
        height: area.height - reserved,
        ..area
    }
}

pub fn terminal_content(size: (u16, u16), identity: &str) -> ratatui::layout::Rect {
    let areas = project_areas(ratatui::layout::Rect::new(0, 0, size.0, size.1));
    crate::terminal_view::content_area(areas.detail, identity)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollBounds {
    pub list_height: u16,
    pub list_max_scroll: u16,
    pub detail_max_scroll: u16,
}

pub fn project_scroll_bounds(
    sources: &[SourceSnapshot],
    filter: Filter,
    details: &[DetailRow],
    size: (u16, u16),
    focused_details: bool,
    feedback: bool,
) -> ScrollBounds {
    if size.0 < 60 || size.1 < 15 {
        return ScrollBounds::default();
    }
    let areas = project_areas(ratatui::layout::Rect::new(0, 0, size.0, size.1));
    let inner_height = areas.list.height.saturating_sub(1);
    let groups = sources
        .iter()
        .filter(|source| source.work.iter().any(|work| filter.matches(work.category)))
        .count();
    let work = sources
        .iter()
        .flat_map(|source| &source.work)
        .filter(|work| filter.matches(work.category))
        .count();
    let lines = groups + work + usize::from(work == 0);
    let detail = if focused_details {
        areas.focused_detail
    } else {
        areas.detail
    };
    let detail = project_detail_content(detail, feedback);
    let detail_width = detail.width.max(1) as usize;
    let detail_lines = wrap_detail_rows(details, detail_width as u16).len();
    ScrollBounds {
        list_height: inner_height,
        list_max_scroll: lines
            .saturating_sub(inner_height as usize)
            .min(u16::MAX as usize) as u16,
        detail_max_scroll: detail_lines
            .saturating_sub(detail.height as usize)
            .min(u16::MAX as usize) as u16,
    }
}

pub fn scroll_bounds(
    snapshot: &DashboardSnapshot,
    all_work: bool,
    selected_id: Option<&str>,
    size: (u16, u16),
) -> ScrollBounds {
    if size.0 < 60 || size.1 < 15 {
        return ScrollBounds::default();
    }
    let body_height = size.1.saturating_sub(4);
    let inner_height = body_height.saturating_sub(2);
    let detail_width = detail_width(size);
    let list_lines = list_line_count(snapshot, all_work);
    let detail_lines = selected_id
        .and_then(|id| snapshot.rows.iter().find(|row| row.status.id == id))
        .map(|row| wrapped_detail_line_count(row, detail_width))
        .unwrap_or(1);
    ScrollBounds {
        list_height: inner_height,
        list_max_scroll: list_lines.saturating_sub(inner_height),
        detail_max_scroll: detail_lines.saturating_sub(inner_height),
    }
}

pub fn detail_values(row: &DashboardRow) -> Vec<String> {
    let status = &row.status;
    let mut values = vec![
        status.title.clone(),
        format!("ID: {}", status.id),
        format!("Action: {}", status.action),
        format!("Attempt: {}", status.attempt),
        format!("Task: {}", status.task),
        format!("Review: {}", status.review),
        format!("Merge Candidate: {}", status.merge_candidate),
        format!("Merge: {}", status.merge),
        format!(
            "Metrics: reviews {}, input {}, output {}",
            status.metrics.review_rounds, status.metrics.input_tokens, status.metrics.output_tokens
        ),
    ];
    values.extend(
        status
            .compatibility_warnings
            .iter()
            .map(|warning| format!("Warning: {warning}")),
    );
    if let Some(release) = &status.release {
        values.push(format!(
            "Release: {} criteria, {} blockers",
            release.criteria, release.blockers
        ));
    }
    if let Some(history) = &status.evidence_history {
        values.push(format!("Landed commit: {}", history.merged_commit));
        if let Some(path) = &history.tester_results {
            values.push(format!("Evidence history: Tester result {path}"));
        }
    }
    if let Some(diagnostic) = &status.recovery_blocker {
        let label = if status.evidence_history.is_some() {
            "Evidence history"
        } else {
            "Evidence"
        };
        values.push(format!("{label}: {diagnostic}"));
    }
    if let Some(latest) = &row.latest_transition {
        let transition = &latest.transition;
        values.push(format!(
            "Latest transition: {} {} → {} (origin {}, elapsed {}s)",
            transition.object_kind,
            transition.object_id,
            transition.new_state,
            transition.origin,
            transition.elapsed_seconds.unwrap_or(0)
        ));
    }
    values.push(
        row.next_action
            .clone()
            .unwrap_or_else(|| "No operator action".into()),
    );
    values
}

fn list_line_count(snapshot: &DashboardSnapshot, all_work: bool) -> u16 {
    let group_lines = snapshot
        .groups(all_work)
        .into_iter()
        .map(|(_, rows)| 1 + rows.len())
        .sum::<usize>();
    let empty_lines = usize::from(group_lines == 0);
    let error_lines = if snapshot.errors.is_empty() {
        0
    } else {
        1 + snapshot.errors.len()
    };
    (group_lines + empty_lines + error_lines) as u16
}

fn detail_width(size: (u16, u16)) -> u16 {
    if size.0 >= 100 {
        size.0.saturating_sub(size.0.saturating_mul(43) / 100)
    } else {
        size.0
    }
    .saturating_sub(2)
}

fn wrapped_detail_line_count(row: &DashboardRow, width: u16) -> u16 {
    let width = width.max(1) as usize;
    detail_values(row)
        .iter()
        .map(|value| {
            UnicodeWidthStr::width(value.as_str())
                .max(1)
                .div_ceil(width)
        })
        .sum::<usize>() as u16
}

/// Wrap diagnostics at display-cell boundaries so scrolling and drawing agree.
pub fn error_lines(snapshot: &DashboardSnapshot, width: u16) -> Vec<String> {
    let width = width.saturating_sub(2).max(1) as usize;
    let mut lines = Vec::new();
    for error in &snapshot.errors {
        for paragraph in error.lines() {
            let mut line = String::new();
            let mut used = 0;
            for character in paragraph.chars() {
                let columns = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
                if used + columns > width && !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                    used = 0;
                }
                line.push(character);
                used += columns;
            }
            lines.push(line);
        }
    }
    if lines.is_empty() {
        lines.push("No Work Item read errors".into());
    }
    lines
}

pub fn error_max_scroll(snapshot: &DashboardSnapshot, size: (u16, u16)) -> u16 {
    error_lines(snapshot, size.0)
        .len()
        .saturating_sub(size.1.saturating_sub(6) as usize)
        .min(u16::MAX as usize) as u16
}

pub fn project_error_max_scroll(snapshot: &DashboardSnapshot, size: (u16, u16)) -> u16 {
    error_lines(snapshot, size.0.saturating_add(2))
        .len()
        .saturating_sub(size.1.saturating_sub(5) as usize)
        .min(u16::MAX as usize) as u16
}

/// Wrap at words with hanging continuation; split only tokens wider than a line.
/// Drawing and scroll bounds consume the same lines.
pub fn project_detail_lines(values: &[String], width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    for value in values {
        for paragraph in value.split('\n') {
            let mut remaining = paragraph;
            let mut continuation = false;
            loop {
                let indent = if continuation {
                    " ".repeat(2.min(width.saturating_sub(1)))
                } else {
                    String::new()
                };
                let available = width - indent.len();
                if UnicodeWidthStr::width(remaining) <= available {
                    lines.push(format!("{indent}{remaining}"));
                    break;
                }
                let mut used = 0;
                let mut end = 0;
                let mut word_break = None;
                for (index, character) in remaining.char_indices() {
                    let columns = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
                    if used + columns > available {
                        break;
                    }
                    used += columns;
                    end = index + character.len_utf8();
                    if character.is_whitespace() && !remaining[..index].trim().is_empty() {
                        word_break = Some(index);
                    }
                }
                // At an exact word boundary, retain the complete fitting word.
                let split = if remaining[end..].starts_with(char::is_whitespace) {
                    end
                } else {
                    word_break.unwrap_or(end)
                };
                if split == 0 {
                    // Retain a wide character on its own when continuation
                    // indentation leaves no room; never discard diagnostic text.
                    let character = remaining.chars().next().unwrap();
                    remaining = &remaining[character.len_utf8()..];
                    lines.push(character.to_string());
                } else {
                    lines.push(format!("{indent}{}", remaining[..split].trim_end()));
                    remaining = remaining[split..].trim_start();
                }
                continuation = true;
            }
        }
    }
    lines
}

#[cfg(test)]
pub fn project_detail_values(sources: &[SourceSnapshot], key: &RowKey) -> Vec<String> {
    project_detail_rows(sources, key)
        .into_iter()
        .map(|row| row.text)
        .collect()
}

pub fn project_detail_rows(sources: &[SourceSnapshot], key: &RowKey) -> Vec<DetailRow> {
    let Some((source, work)) = project::find_work(sources, key) else {
        return vec![DetailRow::text("Selection unavailable")];
    };
    let next_action = next_action(work);
    let worker = project::find_worker(sources, key);
    let mut values = if let Some(worker) = worker {
        let mut values = project_worker_values(worker, work)
            .into_iter()
            .map(DetailRow::text)
            .collect::<Vec<_>>();
        values.insert(8, DetailRow::text(""));
        values.insert(9, DetailRow::heading("Technical identifiers"));
        values
    } else {
        vec![
            work.status.title.clone(),
            work.status
                .recovery_blocker
                .as_ref()
                .map(|reason| format!("Reason: {reason}"))
                .unwrap_or_else(|| {
                    format!(
                        "State: {}",
                        work.phase.as_deref().unwrap_or(&work.status.action)
                    )
                }),
            format!("Next action: {next_action}"),
        ]
        .into_iter()
        .map(DetailRow::text)
        .collect()
    };
    if worker.is_none() {
        values.push(DetailRow::heading(format!(
            "Workers ({})",
            work.workers.len()
        )));
    }
    values.extend(
        project_metadata(source, work, key, worker.is_some())
            .into_iter()
            .map(DetailRow::text),
    );
    values
}

fn next_action(work: &project::ProjectWork) -> String {
    crate::guidance::next_action_for_status_row(&work.status)
        .map(|action| {
            action
                .trim()
                .trim_start_matches("→ Next:")
                .trim()
                .to_string()
        })
        .unwrap_or_else(|| match work.category {
            project::Category::Queued => "Awaiting queue dispatch".into(),
            project::Category::Running => "Workflow running".into(),
            project::Category::History => "No operator action".into(),
            _ => "unavailable".into(),
        })
}

fn project_metadata(
    source: &SourceSnapshot,
    work: &project::ProjectWork,
    key: &RowKey,
    worker: bool,
) -> Vec<String> {
    let mut values = vec![
        format!("Work Item: {}", work.status.id),
        format!(
            "Attempt: {}",
            key.attempt_id.as_deref().unwrap_or(&work.status.attempt)
        ),
    ];
    if !worker {
        values.push(format!("Merge Candidate: {}", work.status.merge_candidate));
    }
    values.push(format!(
        "Source: {} · {}",
        source.host,
        source.path.display()
    ));
    values.extend(
        work.status
            .compatibility_warnings
            .iter()
            .map(|warning| format!("Warning: {warning}")),
    );
    if let Some(stale) = &source.stale {
        values.push(format!("Stale source: {stale}"));
    }
    values.extend(
        source
            .diagnostics
            .iter()
            .map(|diagnostic| format!("Source diagnostic: {diagnostic}")),
    );
    values
}

/// Keep semantic row roles independent of unrestricted display text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailKind {
    Text,
    Heading,
    SelectedWorker,
}

#[derive(Clone, Debug)]
pub struct DetailRow {
    pub text: String,
    pub kind: DetailKind,
}
impl DetailRow {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: DetailKind::Text,
        }
    }
    pub fn heading(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: DetailKind::Heading,
        }
    }
}

/// Preserve row roles through the same wrapping used by drawing and scrolling.
pub fn wrap_detail_rows(rows: &[DetailRow], width: u16) -> Vec<DetailRow> {
    rows.iter()
        .flat_map(|row| {
            project_detail_lines(std::slice::from_ref(&row.text), width)
                .into_iter()
                .map(|text| DetailRow {
                    text,
                    kind: row.kind,
                })
        })
        .collect()
}

/// Build the action, worker table, and secondary context as explicit sections.
pub fn project_overview_rows(
    sources: &[SourceSnapshot],
    key: &RowKey,
    selected: Option<&RowKey>,
    width: u16,
) -> Vec<DetailRow> {
    let Some((source, work)) = project::find_work(sources, key) else {
        return vec![DetailRow::text("Selection unavailable")];
    };
    let condition = work
        .status
        .recovery_blocker
        .as_ref()
        .map(|reason| format!("Reason: {reason}"))
        .unwrap_or_else(|| {
            format!(
                "State: {}",
                work.phase.as_deref().unwrap_or(work.category.label())
            )
        });
    let mut rows = Vec::new();
    let mut secondary = Vec::new();
    for (label, value) in [("Title", &work.status.title), ("Condition", &condition)] {
        if value.width() > usize::from(width) {
            secondary.push(format!("{label}: {value}"));
        }
        rows.push(DetailRow::text(compact(value, usize::from(width))));
    }
    rows.push(DetailRow::text(""));
    let actions = super::actions::offered(source, work);
    if !actions.is_empty() {
        rows.push(DetailRow::heading("Next action · a Actions"));
        let mut commands = actions
            .iter()
            .map(|action| format!("  {}: {}", action.kind.label(), action.command_text()))
            .collect::<Vec<_>>();
        commands.push(format!(
            "Source checkout: {} · {}",
            source.host,
            source.path.display()
        ));
        if width < 100 || project_detail_lines(&commands, width).len() > 3 {
            rows.push(DetailRow::text(format!(
                "  {}",
                actions
                    .iter()
                    .map(|action| action.kind.label())
                    .collect::<Vec<_>>()
                    .join(" · ")
            )));
            secondary.push("Commands and source · a Actions to inspect or copy".into());
            secondary.extend(commands);
        } else {
            rows.extend(commands.into_iter().map(DetailRow::text));
        }
        if let Some(guidance) = &work.status.next_action {
            secondary.push(format!("Guidance: {guidance}"));
        }
    } else {
        rows.push(DetailRow::heading("Next action"));
        let next = next_action(work);
        if next.width() > usize::from(width.saturating_sub(2)) {
            secondary.push(format!("Guidance: {next}"));
        }
        rows.push(DetailRow::text(format!(
            "  {}",
            compact(&next, usize::from(width.saturating_sub(2)))
        )));
    }
    rows.push(DetailRow::text(""));
    rows.push(DetailRow::heading(format!(
        "Workers ({})",
        work.workers.len()
    )));
    rows.push(DetailRow::text(if width >= 100 {
        "  Worker                             State          Elapsed  Terminal"
    } else {
        "  Worker                             State"
    }));
    for worker in &work.workers {
        let task = worker.key.task_id.as_deref().unwrap_or("unavailable");
        let identity = if worker.role == task {
            task.into()
        } else {
            format!("{} ({task})", worker.role)
        };
        let identity = compact(&identity, 34);
        let is_selected = selected == Some(&worker.key);
        let mut text = format!(
            "{} {}{}  {}",
            if is_selected { ">" } else { " " },
            identity,
            " ".repeat(34usize.saturating_sub(identity.width())),
            compact(&worker.state, 14)
        );
        if width >= 100 {
            text.push_str(&format!(
                "{}  {:>7}  {}",
                " ".repeat(14usize.saturating_sub(worker.state.width())),
                worker
                    .elapsed_ms
                    .map(|ms| format!("{}s", ms / 1000))
                    .unwrap_or_else(|| "—".into()),
                if worker
                    .terminal
                    .as_ref()
                    .is_some_and(|terminal| terminal.can_observe)
                {
                    "Observe"
                } else {
                    "Unavailable"
                }
            ));
        }
        rows.push(DetailRow {
            text,
            kind: if is_selected {
                DetailKind::SelectedWorker
            } else {
                DetailKind::Text
            },
        });
    }
    if work.workers.is_empty() {
        rows.push(DetailRow::text("  No workers recorded"));
    }
    rows.push(DetailRow::text(""));
    rows.extend(secondary.into_iter().map(DetailRow::text));
    rows.push(DetailRow::heading("Technical identifiers"));
    rows.extend(
        project_metadata(source, work, key, false)
            .into_iter()
            .map(DetailRow::text),
    );
    rows
}

/// Project a selected worker without repeating its parent's guidance.
pub fn project_worker_values(worker: &project::Worker, work: &project::ProjectWork) -> Vec<String> {
    vec![
        format!("Worker: {}", worker.role),
        format!("State: {}", worker.state),
        format!(
            "Agent terminal: {}",
            worker
                .terminal
                .as_ref()
                .map(|terminal| if terminal.can_observe {
                    "Observe available".into()
                } else {
                    worker.unavailable_reason.clone()
                })
                .unwrap_or_else(|| worker.unavailable_reason.clone())
        ),
        format!(
            "Provider: {}",
            worker.provider.as_deref().unwrap_or("unavailable")
        ),
        format!(
            "Model: {}",
            worker.model.as_deref().unwrap_or("unavailable")
        ),
        format!(
            "Effort: {}",
            worker.effort.as_deref().unwrap_or("unavailable")
        ),
        format!(
            "Elapsed: {}",
            worker
                .elapsed_ms
                .map(|ms| format!("{}s", ms / 1000))
                .unwrap_or_else(|| "unavailable".into())
        ),
        format!(
            "Work provider wait: {}",
            if work.status.metrics.provider_wait_ms > 0 {
                format!("{}s", work.status.metrics.provider_wait_ms / 1000)
            } else {
                "unavailable".into()
            }
        ),
        format!(
            "Task: {}",
            worker.key.task_id.as_deref().unwrap_or("unavailable")
        ),
        format!(
            "Invocation: {}",
            worker
                .key
                .execution_generation
                .as_deref()
                .unwrap_or("unavailable")
        ),
    ]
}

pub fn compact(value: &str, width: usize) -> String {
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
