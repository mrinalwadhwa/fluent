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

    /// Coding agent to launch: claude or codex
    #[arg(long)]
    pub coder: Option<String>,

    /// Extra args passed through to the agent
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub extra_args: Vec<String>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Autonomous session loop
    Run {
        /// Target a specific run
        #[arg(long)]
        run_id: Option<String>,

        /// Execution runtime: local (default), fargate
        #[arg(long, default_value = "local")]
        runtime: String,

        /// Disable sandbox
        #[arg(long)]
        no_sandbox: bool,

        /// Run the session loop in the current workspace without creating a worktree
        #[arg(long, hide = true)]
        in_place: bool,

        /// Preserve existing runtime and handle metadata while running in place
        #[arg(long, hide = true)]
        preserve_run_metadata: bool,

        /// Coding agent to launch: claude or codex
        #[arg(long)]
        coder: Option<String>,

        /// Extra args passed through to the agent
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra_args: Vec<String>,
    },

    /// Run reviewers against the current codebase
    Review {
        /// Target run ID to create or reuse
        #[arg(long)]
        run_id: Option<String>,

        /// Reviewer filter, such as "tests" or "architecture,tests"
        #[arg(long)]
        reviewers: Option<String>,

        /// Brief text for a newly created review run
        #[arg(long)]
        brief: Option<String>,

        /// Disable sandbox
        #[arg(long)]
        no_sandbox: bool,

        /// Coding agent to launch: claude or codex
        #[arg(long)]
        coder: Option<String>,

        /// Extra args passed through to the agent
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra_args: Vec<String>,
    },

    /// Show run state for a project
    Status {
        /// Path to check (default: current directory)
        path: Option<String>,
    },

    /// Manage stored Work Items
    Work {
        #[command(subcommand)]
        command: WorkCommands,
    },

    /// Summarize one run from durable artifacts
    Summary {
        /// Target a specific run ID
        #[arg(long)]
        run_id: Option<String>,
    },

    /// Clean stale run artifacts and registered worktrees
    Cleanup {
        /// Target a specific run ID
        #[arg(long)]
        run_id: Option<String>,

        /// Apply cleanup changes instead of printing a dry run
        #[arg(long)]
        apply: bool,
    },

    /// Poll status, notify on change
    Watch {
        /// Polling interval in seconds (default: 60)
        #[arg(default_value = "60")]
        interval: u64,

        /// Exit after N seconds (0 = run forever)
        #[arg(long, default_value = "0")]
        timeout: u64,
    },

    /// Download completed workspace from S3 (fargate)
    Pull {
        /// Run ID to pull
        run_id: Option<String>,
    },

    /// Interactive shell into running task (fargate)
    Shell {
        /// Run ID to connect to
        run_id: Option<String>,
    },

    /// Restart a paused or failed run
    Resume {
        /// Run ID to resume
        run_id: Option<String>,

        /// Coding agent to launch: claude or codex
        #[arg(long)]
        coder: Option<String>,

        /// Extra args passed through to the agent
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra_args: Vec<String>,
    },

    /// Initialize .factory/ directory
    Init,

    /// Live TUI showing run activity
    Dashboard {
        /// Target a specific run ID
        #[arg(long)]
        run_id: Option<String>,

        /// Path to project (default: current directory)
        path: Option<String>,
    },

    /// Rebase, merge, capture artifacts, and clean up a completed run
    Land {
        /// Run ID to land (default: most recent complete run)
        run_id: Option<String>,
    },

    /// Print Factory version and build commit
    Version,
}

#[derive(Subcommand)]
pub enum WorkCommands {
    /// Create a stored Work Item
    Create {
        /// Work Item ID
        id: String,

        /// Work Item title
        #[arg(long)]
        title: String,
    },

    /// List stored Work Items
    List,

    /// Show one stored Work Item as JSON
    Show {
        /// Work Item ID
        id: String,
    },
}
