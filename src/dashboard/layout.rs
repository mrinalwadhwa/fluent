use super::project::{self, Filter, RowKey, SourceSnapshot};
use super::snapshot::{DashboardRow, DashboardSnapshot};
use std::collections::BTreeSet;
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
    pub list: ratatui::layout::Rect,
    pub divider: ratatui::layout::Rect,
    pub detail: ratatui::layout::Rect,
}

pub fn project_areas(area: ratatui::layout::Rect) -> ProjectAreas {
    use ratatui::layout::{Constraint, Layout, Rect};
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(area);
    let (list, divider, detail) = if area.width >= 100 {
        // Give terminal output more room than the static prototype's 73% list.
        let panes = Layout::horizontal([
            Constraint::Percentage(65),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(rows[1]);
        (panes[0], panes[1], panes[2])
    } else {
        (rows[1], Rect::default(), rows[1])
    };
    ProjectAreas {
        header: rows[0],
        body: rows[1],
        footer: rows[2],
        list,
        divider,
        detail,
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
    expanded: &BTreeSet<RowKey>,
    selected_key: Option<&RowKey>,
    size: (u16, u16),
) -> ScrollBounds {
    if size.0 < 60 || size.1 < 15 {
        return ScrollBounds::default();
    }
    let areas = project_areas(ratatui::layout::Rect::new(0, 0, size.0, size.1));
    let inner_height = areas.list.height.saturating_sub(1);
    let groups = sources.len();
    let work = sources
        .iter()
        .flat_map(|source| &source.work)
        .filter(|work| filter.matches(work.category))
        .count();
    let workers = sources
        .iter()
        .flat_map(|source| &source.work)
        .filter(|work| filter.matches(work.category) && expanded.contains(&work.key))
        .map(|work| work.workers.len())
        .sum::<usize>();
    let lines = groups + work + workers + usize::from(work == 0);
    let detail_width = areas.detail.width.max(1) as usize;
    let details = selected_key
        .map(|key| project_detail_values(sources, key))
        .unwrap_or_default();
    let detail_lines = project_detail_lines(&details, detail_width as u16).len();
    ScrollBounds {
        list_height: inner_height,
        list_max_scroll: lines
            .saturating_sub(inner_height as usize)
            .min(u16::MAX as usize) as u16,
        detail_max_scroll: detail_lines
            .saturating_sub(areas.detail.height as usize)
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

/// Wrap at display cells once so the renderer and scroll bounds agree.
pub fn project_detail_lines(values: &[String], width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    for value in values {
        for paragraph in value.split('\n') {
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
    lines
}

pub fn project_detail_values(sources: &[SourceSnapshot], key: &RowKey) -> Vec<String> {
    let Some((source, work)) = project::find_work(sources, key) else {
        return vec!["Selection unavailable".into()];
    };
    let next_action = crate::guidance::next_action_for_status_row(&work.status)
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
        });
    let mut values = vec![
        format!("Work Item: {}", work.status.id),
        format!(
            "Attempt: {}",
            key.attempt_id.as_deref().unwrap_or(&work.status.attempt)
        ),
    ];
    if key.task_id.is_none() {
        if let Some(reason) = &work.status.recovery_blocker {
            values.push(format!("Reason: {reason}"));
        } else {
            values.push(format!(
                "State: {}",
                work.phase.as_deref().unwrap_or(&work.status.action)
            ));
        }
    }
    values.push(format!("Next action: {next_action}"));
    if let Some(worker) = project::find_worker(sources, key) {
        values.extend([
            format!("Worker: {}", worker.role),
            format!(
                "Task: {}",
                worker.key.task_id.as_deref().unwrap_or("unavailable")
            ),
            format!("State: {}", worker.state),
            format!(
                "Invocation: {}",
                worker
                    .key
                    .execution_generation
                    .as_deref()
                    .unwrap_or("unavailable")
            ),
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
        ]);
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
