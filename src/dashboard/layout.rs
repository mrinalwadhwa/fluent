use super::snapshot::{DashboardRow, DashboardSnapshot};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollBounds {
    pub list_height: u16,
    pub list_max_scroll: u16,
    pub detail_max_scroll: u16,
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
