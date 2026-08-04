use crate::{
    guidance,
    work_status::{WorkItemStatus, WorkStatus},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    NeedsYou,
    Running,
    Ready,
    Terminal,
}
impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Self::NeedsYou => "Needs you",
            Self::Running => "Running",
            Self::Ready => "Ready",
            Self::Terminal => "Terminal",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardRow {
    pub status: WorkItemStatus,
    pub group: Group,
    pub next_action: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardSnapshot {
    pub rows: Vec<DashboardRow>,
    pub errors: Vec<String>,
}

impl DashboardSnapshot {
    pub fn from_status(status: WorkStatus) -> Self {
        Self {
            rows: status
                .rows
                .into_iter()
                .map(|row| {
                    let group = group_for(&row.action);
                    let next_action = guidance::next_action_for_status_row(&row);
                    DashboardRow {
                        status: row,
                        group,
                        next_action,
                    }
                })
                .collect(),
            errors: status.errors,
        }
    }
    pub fn visible_rows(&self, all: bool) -> Vec<&DashboardRow> {
        self.rows
            .iter()
            .filter(|row| all || row.group != Group::Terminal)
            .collect()
    }
    pub fn ordered_rows(&self, all: bool) -> Vec<&DashboardRow> {
        self.groups(all)
            .into_iter()
            .flat_map(|(_, rows)| rows)
            .collect()
    }
    pub fn groups(&self, all: bool) -> Vec<(Group, Vec<&DashboardRow>)> {
        [
            Group::NeedsYou,
            Group::Running,
            Group::Ready,
            Group::Terminal,
        ]
        .into_iter()
        .filter(|group| all || *group != Group::Terminal)
        .filter_map(|group| {
            let rows = self
                .visible_rows(all)
                .into_iter()
                .filter(|row| row.group == group)
                .collect::<Vec<_>>();
            (!rows.is_empty()).then_some((group, rows))
        })
        .collect()
    }
    pub fn selected_line(&self, all: bool, id: &str) -> Option<usize> {
        let mut line = 0;
        for (_, rows) in self.groups(all) {
            line += 1;
            if let Some(index) = rows.iter().position(|row| row.status.id == id) {
                return Some(line + index);
            }
            line += rows.len();
        }
        None
    }
}

fn group_for(action: &str) -> Group {
    match action {
        "complete" | "merged" | "abandoned" => Group::Terminal,
        "executing" | "reviewing" | "merging" => Group::Running,
        "task-ready" | "merge-ready" | "learner-not-ready" | "planned" | "not-started" => {
            Group::Ready
        }
        _ => Group::NeedsYou,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_status::WorkItemStatus;
    fn row(action: &str) -> WorkItemStatus {
        WorkItemStatus {
            id: action.into(),
            title: action.into(),
            attempt: "-".into(),
            task: "-".into(),
            review: "-".into(),
            merge_candidate: "-".into(),
            merge: "-".into(),
            action: action.into(),
            next_action: None,
            metrics: Default::default(),
            compatibility_warnings: vec![],
            release: None,
        }
    }
    #[test]
    fn all_work_adds_terminal_group() {
        let snapshot = DashboardSnapshot::from_status(WorkStatus {
            rows: vec![row("planned"), row("complete")],
            errors: vec![],
        });
        assert_eq!(snapshot.groups(false).len(), 1);
        assert_eq!(snapshot.groups(true).len(), 2);
    }
}
