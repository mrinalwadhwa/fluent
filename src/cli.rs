use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "factory",
    about = "Run coding agents inside a macOS Seatbelt sandbox with session continuity"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Override the sandbox file-access root (default: pwd)
    #[arg(long)]
    pub sandbox_root: Option<String>,

    /// Print the rendered sandbox profile and exit
    #[arg(long)]
    pub dry_run: bool,

    /// Kill existing Claude Code processes before launching
    #[arg(long)]
    pub force: bool,

    /// Tail the factory log file
    #[arg(long)]
    pub logs: bool,

    /// Disable sandbox (for Fargate or Linux)
    #[arg(long)]
    pub no_sandbox: bool,

    /// Coding agent to launch: claude, codex, or pi
    #[arg(long)]
    pub coder: Option<String>,

    /// Extra args passed through to the agent
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub extra_args: Vec<String>,
}

#[derive(Subcommand)]
pub enum Commands {
    // -- WORK MODEL entities --
    /// Manage stored Work Items
    WorkItem {
        #[command(subcommand)]
        command: WorkItemCommands,
    },

    /// Manage Attempts on a Work Item
    Attempt {
        #[command(subcommand)]
        command: AttemptCommands,
    },

    /// Manage Merge Candidates for a Work Item
    MergeCandidate {
        #[command(subcommand)]
        command: MergeCandidateCommands,
    },

    /// Manage Tasks within an Attempt
    Task {
        #[command(subcommand)]
        command: TaskCommands,
    },

    /// Manage the Work Item queue
    Queue {
        #[command(subcommand)]
        command: QueueCommands,
    },

    /// Run the deterministic Tester subcommand
    Tester {
        #[command(subcommand)]
        command: TesterCommands,
    },

    /// Run the sequential scheduler
    Scheduler {
        #[command(subcommand)]
        command: SchedulerCommands,
    },

    // -- ACTIONS --
    /// Plan review Tasks or review the codebase
    Review {
        #[command(subcommand)]
        command: Option<ReviewCommands>,

        /// Work Item ID (for planning review Tasks)
        work_item_id: Option<String>,

        /// Attempt ID (for planning review Tasks)
        attempt_id: Option<String>,
    },

    /// Watch for merge-ready candidates and merge them automatically
    AutoMerge {
        /// Work Item ID (watches a single Work Item)
        work_item_id: Option<String>,

        /// Watch all Work Items in the project
        #[arg(long)]
        all: bool,

        /// Disable sandbox
        #[arg(long)]
        no_sandbox: bool,

        /// Coding agent to launch: claude or codex
        #[arg(long)]
        coder: Option<String>,

        /// Poll interval in seconds (default 30)
        #[arg(long, hide = true)]
        poll_seconds: Option<u64>,
    },

    /// Drain the pending post-merge review queue (debounced)
    PostMergeReview {
        /// Override debounce in seconds (default
        /// FACTORY_POST_MERGE_DEBOUNCE_SECONDS env var or 60)
        #[arg(long)]
        debounce_seconds: Option<u64>,
        /// Restrict to a single target branch
        #[arg(long)]
        target: Option<String>,
    },

    // -- PROJECT --
    /// Show Work Item state for a project
    Status {
        /// Path to check (default: current directory)
        path: Option<String>,
    },

    /// Initialize .factory/ directory
    Init,

    /// Clean stale Work Item artifacts and registered worktrees
    Cleanup {
        /// Apply cleanup changes instead of printing a dry run
        #[arg(long)]
        apply: bool,

        /// Prune all review-only worktrees, not just orphans
        #[arg(long)]
        prune_all_review_worktrees: bool,
    },

    /// Live TUI showing Work Item activity
    Dashboard {
        /// Path to project (default: current directory)
        path: Option<String>,
    },

    /// Print Factory version and build commit
    Version,

    /// Manage per-file observations
    Observation {
        #[command(subcommand)]
        command: ObservationCommands,
    },

    /// Prevent macOS idle sleep (caffeinate toggle)
    KeepAwake {
        #[command(subcommand)]
        command: KeepAwakeCommands,
    },

    /// Manage Fargate infrastructure
    Fargate {
        #[command(subcommand)]
        command: FargateCommands,
    },
}

// ---------------------------------------------------------------------------
// Work Item
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum WorkItemCommands {
    /// Create a stored Work Item
    Create {
        /// Work Item ID
        id: String,

        /// Work Item title
        #[arg(long)]
        title: String,

        /// Rich instructions to carry into write Tasks
        #[arg(long, conflicts_with = "instructions_file")]
        instructions: Option<String>,

        /// Read rich instructions from a file
        #[arg(long, value_name = "PATH")]
        instructions_file: Option<String>,

        /// Approved planning context to store on the Work Item
        #[arg(long, conflicts_with = "planning_context_file")]
        planning_context: Option<String>,

        /// Read approved planning context from a file
        #[arg(long, value_name = "PATH")]
        planning_context_file: Option<String>,

        /// Read the approved brief from a file
        #[arg(long, value_name = "PATH")]
        brief_file: Option<String>,

        /// Read the approved behaviors from a file
        #[arg(long, value_name = "PATH")]
        behaviors_file: Option<String>,

        /// Read the approved approach from a file
        #[arg(long, value_name = "PATH")]
        approach_file: Option<String>,

        /// Read the approved plan from a file
        #[arg(long, value_name = "PATH")]
        plan_file: Option<String>,
    },

    /// List stored Work Items
    List,

    /// Show one stored Work Item as JSON
    Show {
        /// Work Item ID
        id: String,
    },

    /// Mark a Work Item as intentionally abandoned
    Abandon {
        /// Work Item ID
        id: String,

        /// Reason to store with the abandoned Work Item
        #[arg(long)]
        reason: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Attempt
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum AttemptCommands {
    /// Create a planned Attempt with an initial write Task
    Create {
        /// Work Item ID
        work_item_id: String,

        /// Attempt ID (auto-assigned if omitted)
        attempt_id: Option<String>,
    },

    /// List Attempts for a Work Item
    List {
        /// Work Item ID
        work_item_id: String,
    },

    /// Show one Attempt as JSON
    Show {
        /// Work Item ID
        work_item_id: String,

        /// Attempt ID
        attempt_id: String,
    },

    /// Advance an Attempt through the next safe transitions
    Run {
        /// Work Item ID
        work_item_id: String,

        /// Attempt ID (default: most recently created Attempt)
        attempt_id: Option<String>,

        /// Disable sandbox
        #[arg(long)]
        no_sandbox: bool,

        /// Coding agent to launch: claude, codex, or pi
        #[arg(long)]
        coder: Option<String>,

        /// Coder for write Tasks: claude, codex, or pi
        #[arg(long)]
        write_coder: Option<String>,

        /// Model for write Tasks
        #[arg(long)]
        write_model: Option<String>,

        /// Coder for review Tasks: claude, codex, or pi
        #[arg(long)]
        review_coder: Option<String>,

        /// Model for review Tasks
        #[arg(long)]
        review_model: Option<String>,

        /// Coder for behavior-tests Tasks: claude, codex, or pi
        #[arg(long)]
        behavior_tests_coder: Option<String>,

        /// Model for behavior-tests Tasks
        #[arg(long)]
        behavior_tests_model: Option<String>,

        /// Execution runtime: local (default) or fargate
        #[arg(long)]
        runtime: Option<String>,

        /// Extra args passed through to the agent
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra_args: Vec<String>,
    },

    /// Download a Fargate-executed Attempt's workspace + Work state
    /// from S3 back into the project workspace.
    Pull {
        /// Work Item ID
        work_item_id: String,
        /// Attempt ID
        attempt_id: String,
    },

    /// Stop a Fargate-executed Attempt's ECS task (best-effort, idempotent).
    Stop {
        /// Work Item ID
        work_item_id: String,
        /// Attempt ID
        attempt_id: String,
    },

    /// Watch a Fargate-executed Attempt's ECS task until it stops.
    Watch {
        /// Work Item ID
        work_item_id: String,
        /// Attempt ID
        attempt_id: String,
        /// Poll interval in seconds (default 15)
        #[arg(long, default_value_t = 15)]
        interval: u64,
    },
}

// ---------------------------------------------------------------------------
// Merge Candidate
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum MergeCandidateCommands {
    /// List Merge Candidates for a Work Item
    List {
        /// Work Item ID
        work_item_id: String,
    },

    /// Show one stored Merge Candidate as JSON
    Show {
        /// Work Item ID
        work_item_id: String,

        /// Merge Candidate ID
        merge_candidate_id: String,
    },

    /// Execute a stored Merge Candidate
    Land {
        /// Work Item ID
        work_item_id: String,

        /// Merge Candidate ID (default: most recently created Merge Candidate)
        merge_candidate_id: Option<String>,

        /// Disable sandbox
        #[arg(long)]
        no_sandbox: bool,

        /// Coding agent to launch: claude or codex
        #[arg(long)]
        coder: Option<String>,

        /// Execution runtime: local (default) or fargate
        #[arg(long)]
        runtime: Option<String>,

        /// Extra args passed through to the agent
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra_args: Vec<String>,
    },

    /// Download a Fargate-executed Merge Candidate's workspace +
    /// Work state from S3 back into the project workspace.
    Pull {
        /// Work Item ID
        work_item_id: String,
        /// Merge Candidate ID
        merge_candidate_id: String,
    },

    /// Stop a Fargate-executed Merge Candidate's ECS task
    /// (best-effort, idempotent).
    Stop {
        /// Work Item ID
        work_item_id: String,
        /// Merge Candidate ID
        merge_candidate_id: String,
    },

    /// Watch a Fargate-executed Merge Candidate's ECS task until it stops.
    Watch {
        /// Work Item ID
        work_item_id: String,
        /// Merge Candidate ID
        merge_candidate_id: String,
        /// Poll interval in seconds (default 15)
        #[arg(long, default_value_t = 15)]
        interval: u64,
    },
}

// ---------------------------------------------------------------------------
// Task
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum TaskCommands {
    /// List Tasks for an Attempt
    List {
        /// Work Item ID
        work_item_id: String,

        /// Attempt ID
        attempt_id: String,
    },

    /// Show one Task as JSON
    Show {
        /// Work Item ID
        work_item_id: String,

        /// Attempt ID
        attempt_id: String,

        /// Task ID
        task_id: String,
    },

    /// Run an existing Task
    Run {
        /// Work Item ID
        work_item_id: String,

        /// Attempt ID
        attempt_id: String,

        /// Task ID
        task_id: String,

        /// Disable sandbox
        #[arg(long)]
        no_sandbox: bool,

        /// Coding agent to launch: claude, codex, or pi
        #[arg(long)]
        coder: Option<String>,

        /// Coder for write Tasks: claude, codex, or pi
        #[arg(long)]
        write_coder: Option<String>,

        /// Model for write Tasks
        #[arg(long)]
        write_model: Option<String>,

        /// Coder for review Tasks: claude, codex, or pi
        #[arg(long)]
        review_coder: Option<String>,

        /// Model for review Tasks
        #[arg(long)]
        review_model: Option<String>,

        /// Coder for behavior-tests Tasks: claude, codex, or pi
        #[arg(long)]
        behavior_tests_coder: Option<String>,

        /// Model for behavior-tests Tasks
        #[arg(long)]
        behavior_tests_model: Option<String>,

        /// Extra args passed through to the agent
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra_args: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum QueueCommands {
    /// Add a Work Item to the queue
    Add {
        /// Work Item ID
        work_item_id: String,

        /// Numeric priority (higher = sooner; default 0)
        #[arg(long)]
        priority: Option<i64>,
    },

    /// List queued Work Items
    List,

    /// Remove a Work Item from the queue
    Remove {
        /// Work Item ID
        work_item_id: String,
    },
}

// ---------------------------------------------------------------------------
// Tester
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum TesterCommands {
    /// Run the Tester subcommand for a specific Task
    Run {
        /// Work Item ID
        work_item_id: String,

        /// Attempt ID
        attempt_id: String,

        /// Task ID
        task_id: String,

        /// Disable sandbox
        #[arg(long)]
        no_sandbox: bool,
    },
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum SchedulerCommands {
    /// Poll the queue and run Work Items sequentially
    Run {
        /// Seconds between queue polls when idle (default 30)
        #[arg(long)]
        poll_seconds: Option<u64>,
    },
}

// ---------------------------------------------------------------------------
// Review
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum ReviewCommands {
    /// Create a review-only Attempt for the current codebase
    Codebase {
        /// Work Item ID
        work_item_id: String,

        /// Attempt ID
        attempt_id: String,

        /// Review the current working tree at `.` (with the source-checkout
        /// restorative guard) instead of the per-branch review-only worktree.
        #[arg(long)]
        from_working_tree: bool,
    },
}

// ---------------------------------------------------------------------------
// Observation
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum ObservationCommands {
    /// Record a new observation
    Create {
        /// Observation content (reads from stdin when absent)
        content: Option<String>,
    },

    /// Resolve an open observation
    Resolve {
        /// Observation ID or unique prefix
        id: String,

        /// Resolution context (reads from stdin when absent)
        resolution: Option<String>,
    },

    /// List open observations
    List,

    /// Print the body of one observation
    Show {
        /// Observation ID or unique prefix
        id: String,
    },

    /// Migrate monolithic observation files to per-file layout
    Migrate,
}

// ---------------------------------------------------------------------------
// Keep Awake
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum KeepAwakeCommands {
    /// Enable keep-awake (start caffeinate, install LaunchAgent)
    On,

    /// Disable keep-awake (stop caffeinate, disable LaunchAgent)
    Off,

    /// Print current keep-awake state
    Status,

    /// Remove the LaunchAgent and stop caffeinate
    Uninstall,
}

// ---------------------------------------------------------------------------
// Fargate
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum FargateCommands {
    /// Deploy infrastructure and build images
    EnsureSetup {
        /// Force rebuild of all images
        #[arg(long)]
        force_rebuild: bool,
    },

    /// Tear down Fargate infrastructure
    Teardown {
        /// Keep the ECR repository intact
        #[arg(long)]
        keep_ecr: bool,

        /// Keep the S3 bucket intact
        #[arg(long)]
        keep_s3: bool,
    },
}
