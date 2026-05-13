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

        /// Extra args passed through to the agent
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra_args: Vec<String>,
    },

    /// Show run state for a project
    Status {
        /// Path to check (default: current directory)
        path: Option<String>,
    },

    /// Poll status, notify on change
    Watch {
        /// Polling interval in seconds (default: 60)
        #[arg(default_value = "60")]
        interval: u64,
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

        /// Extra args passed through to the agent
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        extra_args: Vec<String>,
    },

    /// Initialize .factory/ directory
    Init,
}
