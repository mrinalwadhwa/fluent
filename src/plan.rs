use anyhow::{bail, Result};

/// A parsed execution plan with sequential groups of parallelizable steps.
pub struct Plan {
    pub groups: Vec<Group>,
}

/// A group of steps that execute concurrently.
pub struct Group {
    pub steps: Vec<Step>,
}

/// A single step within a group.
pub struct Step {
    pub title: String,
    pub brief: String,
}

impl Plan {
    /// Whether the plan requires parallel execution.
    ///
    /// Returns true when the plan has more than one group or any group
    /// contains more than one step.
    pub fn is_parallel(&self) -> bool {
        self.groups.len() > 1 || self.groups.iter().any(|g| g.steps.len() > 1)
    }
}

/// Parse a plan.md into structured groups and steps.
///
/// Groups are delimited by `## ` headings, steps by `### ` headings.
/// Everything after a step heading until the next heading is the step's
/// brief content.
///
/// ```text
/// ## Group 1
///
/// ### Step: Add auth endpoints
///
/// Implement login and logout REST endpoints.
///
/// ### Step: Add database schema
///
/// Create the users table.
///
/// ## Group 2
///
/// ### Step: Add frontend login
///
/// Create the login component.
/// ```
pub fn parse_plan(content: &str) -> Result<Plan> {
    let mut groups: Vec<Group> = Vec::new();
    let mut current_steps: Vec<Step> = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_brief: Vec<String> = Vec::new();
    let mut in_group = false;

    for line in content.lines() {
        if line.starts_with("## ") && !line.starts_with("### ") {
            // New group — flush current step and group
            flush_step(&mut current_title, &mut current_brief, &mut current_steps);
            if in_group && !current_steps.is_empty() {
                groups.push(Group { steps: current_steps });
                current_steps = Vec::new();
            }
            in_group = true;
        } else if line.starts_with("### ") {
            // New step — flush previous step
            flush_step(&mut current_title, &mut current_brief, &mut current_steps);
            let heading = line.trim_start_matches('#').trim();
            let title = heading
                .strip_prefix("Step:")
                .or_else(|| heading.strip_prefix("Step "))
                .map(|t| t.trim().to_string())
                .unwrap_or_else(|| heading.to_string());
            current_title = Some(title);
        } else if current_title.is_some() {
            current_brief.push(line.to_string());
        }
    }

    // Flush remaining step and group
    flush_step(&mut current_title, &mut current_brief, &mut current_steps);
    if !current_steps.is_empty() {
        groups.push(Group { steps: current_steps });
    }

    if groups.is_empty() {
        bail!("No groups found in plan");
    }

    Ok(Plan { groups })
}

fn flush_step(
    title: &mut Option<String>,
    brief_lines: &mut Vec<String>,
    steps: &mut Vec<Step>,
) {
    if let Some(title) = title.take() {
        let brief = brief_lines.join("\n").trim().to_string();
        steps.push(Step { title, brief });
        brief_lines.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_two_groups() {
        let content = "\
## Group 1

### Step: Add auth

Implement login endpoints.

### Step: Add schema

Create the users table.

## Group 2

### Step: Add frontend

Build the login page.
";
        let plan = parse_plan(content).unwrap();
        assert_eq!(plan.groups.len(), 2);
        assert_eq!(plan.groups[0].steps.len(), 2);
        assert_eq!(plan.groups[0].steps[0].title, "Add auth");
        assert_eq!(plan.groups[0].steps[0].brief, "Implement login endpoints.");
        assert_eq!(plan.groups[0].steps[1].title, "Add schema");
        assert_eq!(plan.groups[0].steps[1].brief, "Create the users table.");
        assert_eq!(plan.groups[1].steps.len(), 1);
        assert_eq!(plan.groups[1].steps[0].title, "Add frontend");
        assert_eq!(plan.groups[1].steps[0].brief, "Build the login page.");
    }

    #[test]
    fn test_parse_single_group_multiple_steps() {
        let content = "\
## Group 1

### Step: Task A

Do task A.

### Step: Task B

Do task B.

### Step: Task C

Do task C.
";
        let plan = parse_plan(content).unwrap();
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].steps.len(), 3);
    }

    #[test]
    fn test_parse_single_group_single_step() {
        let content = "\
## Group 1

### Step: Only task

Do the thing.
";
        let plan = parse_plan(content).unwrap();
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].steps.len(), 1);
        assert!(!plan.is_parallel());
    }

    #[test]
    fn test_is_parallel_multiple_steps() {
        let content = "\
## Group 1

### Step: A

Brief A.

### Step: B

Brief B.
";
        let plan = parse_plan(content).unwrap();
        assert!(plan.is_parallel());
    }

    #[test]
    fn test_is_parallel_multiple_groups() {
        let content = "\
## Group 1

### Step: A

Brief A.

## Group 2

### Step: B

Brief B.
";
        let plan = parse_plan(content).unwrap();
        assert!(plan.is_parallel());
    }

    #[test]
    fn test_parse_no_groups() {
        let content = "Just some text without any groups.";
        let result = parse_plan(content);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_multiline_brief() {
        let content = "\
## Group 1

### Step: Complex task

This is line one.
This is line two.

And a paragraph after a blank line.
";
        let plan = parse_plan(content).unwrap();
        assert_eq!(
            plan.groups[0].steps[0].brief,
            "This is line one.\nThis is line two.\n\nAnd a paragraph after a blank line."
        );
    }

    #[test]
    fn test_parse_step_without_colon_prefix() {
        let content = "\
## Group 1

### Add authentication

Implement auth.
";
        let plan = parse_plan(content).unwrap();
        assert_eq!(plan.groups[0].steps[0].title, "Add authentication");
    }

    #[test]
    fn test_parse_ignores_h1_headings() {
        let content = "\
# Plan title

## Group 1

### Step: Do stuff

The brief.
";
        let plan = parse_plan(content).unwrap();
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].steps[0].title, "Do stuff");
    }

    #[test]
    fn test_parse_text_before_first_group_ignored() {
        let content = "\
Some preamble text.

## Group 1

### Step: First

Brief here.
";
        let plan = parse_plan(content).unwrap();
        assert_eq!(plan.groups.len(), 1);
        assert_eq!(plan.groups[0].steps[0].brief, "Brief here.");
    }
}
