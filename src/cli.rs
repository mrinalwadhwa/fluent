use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "fluent",
    about = "Run coding agents inside a macOS Seatbelt sandbox with session continuity",
    version = crate::version::version_tag_str(),
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

    /// Tail the fluent log file
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
    /// Serve a bounded SSH source protocol (invoked by the dashboard)
    #[command(hide = true)]
    AgentSource {
        #[command(subcommand)]
        command: AgentSourceCommands,
    },
    /// List or attach to a managed worker terminal
    Terminal {
        #[command(subcommand)]
        command: TerminalCommands,
    },
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

        /// Run the detached post-merge review after each land (opt-in;
        /// omitted, auto-merge lands with post-merge review disabled)
        #[arg(long)]
        post_merge_review: bool,

        /// Poll interval in seconds (default 30)
        #[arg(long, hide = true)]
        poll_seconds: Option<u64>,
    },

    /// Drain the pending post-merge review queue (debounced)
    PostMergeReview {
        /// Override debounce in seconds (default
        /// FLUENT_POST_MERGE_DEBOUNCE_SECONDS env var or 60)
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

    /// Initialize .fluent/ directory
    Init {
        /// Coder profile: codex-balanced, codex-stronger, claude-balanced,
        /// codex-write-claude-review, claude-write-codex-review, or custom
        #[arg(long)]
        coder_profile: Option<String>,

        /// Follow-up mode for configured first-time setup: propose or execute
        #[arg(long)]
        follow_up_mode: Option<String>,

        #[arg(long)]
        write_coder: Option<String>,
        #[arg(long)]
        write_model: Option<String>,
        #[arg(long)]
        write_effort: Option<String>,
        #[arg(long)]
        review_coder: Option<String>,
        #[arg(long)]
        review_model: Option<String>,
        #[arg(long)]
        review_effort: Option<String>,
    },

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
        /// Include another local project checkout (repeatable)
        #[arg(long = "project")]
        projects: Vec<String>,
    },

    /// Adopt, install, or refresh bundled agent skills
    Skills {
        #[command(subcommand)]
        command: Option<SkillsCommands>,
    },

    /// Update Fluent to the latest release
    Update,

    /// Print Fluent version and build commit
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

    /// Operate the local Discord runner
    Discord {
        #[command(subcommand)]
        command: DiscordCommands,
    },

    /// Print local Slack Socket Mode setup material
    Slack {
        #[command(subcommand)]
        command: SlackCommands,
    },

    /// Manage Fargate infrastructure
    Fargate {
        #[command(subcommand)]
        command: FargateCommands,
    },
}

#[derive(Subcommand)]
pub enum TerminalCommands {
    /// List terminals owned by a source checkout
    List {
        #[arg(long)]
        source: PathBuf,
    },
    /// Attach to an exact worker invocation (Observe by default)
    Attach {
        session_id: String,
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        control: bool,
    },
    /// Explain why a selected worker has no live agent terminal
    Explain {
        #[arg(long)]
        source: PathBuf,
        work_item_id: String,
        attempt_id: String,
        task_id: String,
    },
}

#[derive(Subcommand)]
pub enum DiscordCommands {
    /// Print configuration and private server setup instructions
    Setup,
    /// Validate credentials, Message Content access, and configured channels
    Check,
    /// Register /fluent in the configured server
    Register,
    /// Print the local checkout identity
    CheckoutIdentity { path: PathBuf },
    /// Authenticate the provider in one bound thread's private state
    ProviderLogin {
        #[arg(long)]
        project: String,
        #[arg(long)]
        thread: String,
        #[arg(long)]
        provider: String,
    },
    /// Install and start the local runner at login
    Enable,
    /// Run the Gateway connection in the foreground
    Run,
    /// Restart the login service
    Restart,
    /// Stop the login service
    Stop,
    /// Show safe connection and project status
    Status,
}

#[derive(Subcommand)]
pub enum SlackCommands {
    /// Print the Slack app manifest for a Socket Mode installation
    Manifest,
    /// Print the owner-facing local Slack runner setup instructions
    Setup,
    /// Print the machine-local identity for a checkout
    CheckoutIdentity {
        /// Checkout to identify
        path: PathBuf,
    },
    /// Authenticate a provider in the private state used by one Slack thread
    ProviderLogin {
        /// Configured project label
        #[arg(long)]
        project: String,
        /// Slack thread timestamp
        #[arg(long)]
        thread: String,
        /// Enabled provider name (`codex` or `claude`)
        #[arg(long)]
        provider: String,
    },
    /// Reconfirm a restored checkout and resume its paused Slack thread
    ReconfirmCheckout {
        /// Configured project label
        #[arg(long)]
        project: String,
        /// Slack thread timestamp
        #[arg(long)]
        thread: String,
    },
    /// Install and start the local Slack runner at login
    Enable,
    /// Start the configured local Slack runner
    Run,
    /// Restart the local Slack runner
    Restart,
    /// Stop the local Slack runner
    Stop,
    /// Show the local Slack runner's safe operational status
    Status,
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

        /// Pre-land Learner policy: `capture` (default) or `no-expertise`
        #[arg(long, value_name = "MODE")]
        learner_mode: Option<crate::work_model::LearnerMode>,

        /// Preserve an existing managed artifact as immutable execution input
        #[arg(long, value_name = "PATH")]
        input_artifact: Vec<String>,

        /// Authorize creation when behavior or Plan breadth exceeds the scope limit
        #[arg(long)]
        authorize_large_scope: bool,

        /// Treat this Work Item as a release exercise with a fixed acceptance
        /// contract
        #[arg(long)]
        release: bool,

        /// Accepted release criterion in `id=statement` form (repeatable)
        #[arg(long, value_name = "ID=STATEMENT", requires = "release")]
        release_criterion: Vec<String>,
    },

    /// List stored Work Items
    List,

    /// Show one stored Work Item as JSON
    Show {
        /// Work Item ID
        id: String,
    },

    /// Upgrade one Work Item's legacy split records to the current model format
    Upgrade {
        /// Work Item ID; omit to upgrade every readable Work Item
        id: Option<String>,
    },

    /// Rebuild persisted status metrics after upgrade or manual artifact changes
    RebuildMetrics {
        /// Rebuild only this Work Item; omit to rebuild all
        id: Option<String>,
    },

    /// Mark a Work Item as intentionally abandoned
    Abandon {
        /// Work Item ID
        id: String,

        /// Reason to store with the abandoned Work Item
        #[arg(long)]
        reason: Option<String>,
    },

    /// Authorize a proposed Work Item to execute and enqueue it. Never
    /// authorizes landing.
    Authorize {
        /// Work Item ID
        id: String,
    },

    /// Classify a release finding. Findings are proposed follow-ups unless
    /// explicitly mapped to an accepted release criterion.
    ClassifyFinding {
        /// Release Work Item ID
        id: String,

        /// Stable finding identifier
        #[arg(long)]
        finding_id: String,

        /// One-line finding summary
        #[arg(long)]
        summary: String,

        /// Accepted release criterion that makes this finding a blocker
        #[arg(long, value_name = "CRITERION_ID")]
        blocker_for: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Attempt
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum AttemptCommands {
    /// Preview or explicitly approve responsibility for saved Plan obligations
    Ownership {
        #[command(subcommand)]
        command: OwnershipCommands,
    },
    /// Run, attach, or classify host-evidence recovery
    Evidence {
        #[command(subcommand)]
        command: AttemptEvidenceCommands,
    },
    /// Create a planned Attempt with an initial write Task
    Create {
        /// Work Item ID
        work_item_id: String,

        /// Attempt ID (auto-assigned if omitted)
        attempt_id: Option<String>,

        /// Agent terminal execution mode for this Attempt
        #[arg(long, value_parser = ["headless", "native"])]
        terminal_mode: Option<String>,

        /// Coding agent to launch: claude, codex, or pi
        #[arg(long)]
        coder: Option<String>,

        /// Model to use for all roles
        #[arg(long)]
        model: Option<String>,

        /// Reasoning effort to use for all roles
        #[arg(long)]
        effort: Option<String>,

        /// Coder for write Tasks: claude, codex, or pi
        #[arg(long)]
        write_coder: Option<String>,

        /// Model for write Tasks
        #[arg(long)]
        write_model: Option<String>,

        /// Effort for write Tasks
        #[arg(long)]
        write_effort: Option<String>,

        /// Coder for review Tasks: claude, codex, or pi
        #[arg(long)]
        review_coder: Option<String>,

        /// Model for review Tasks
        #[arg(long)]
        review_model: Option<String>,

        /// Effort for review Tasks
        #[arg(long)]
        review_effort: Option<String>,
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

    /// Approve a bounded, audited extension for a paused write-round cap
    Extend {
        /// Work Item ID
        work_item_id: String,
        /// Attempt ID
        attempt_id: String,
        /// Writer rounds to authorize (1 through 3)
        #[arg(long)]
        additional_write_rounds: usize,
        /// Human decision that directs the corrective Writer at this boundary
        #[arg(long)]
        resolution: String,
    },

    /// Approve one tracked project Tester repair through a corrective Writer
    RepairTester {
        /// Work Item ID
        work_item_id: String,
        /// Attempt ID
        attempt_id: String,
        /// Operator decision that scopes the Tester repair Writer
        #[arg(long)]
        resolution: String,
    },

    /// Repair candidate ancestry or restore a settled interrupted Writer
    RepairCandidateAncestry {
        /// Work Item ID
        work_item_id: String,
        /// Attempt ID
        attempt_id: String,
        /// Operator decision approving the exact ancestry repair
        #[arg(long)]
        resolution: String,
        /// Restore a settled interrupted Writer to its exact saved candidate
        #[arg(long)]
        restore_interrupted: bool,
        /// Exact rejected HEAD inspected by the operator (never a target commit)
        #[arg(
            long,
            requires = "restore_interrupted",
            required_if_eq("restore_interrupted", "true")
        )]
        rejected_candidate: Option<String>,
    },

    /// Authorize a tree-identical commit-metadata rewrite for a rejected candidate
    RepairCandidateMetadata {
        /// Work Item ID
        work_item_id: String,
        /// Attempt ID
        attempt_id: String,
        /// Operator decision approving the exact metadata-only rewrite
        #[arg(long)]
        resolution: String,
    },

    /// Approve one retry of a Writer interrupted by a proven provider usage limit
    RecoverUsageLimit {
        work_item_id: String,
        attempt_id: String,
        /// Operator decision authorizing this exact interrupted Writer
        #[arg(long)]
        resolution: String,
    },

    /// Recover one saved mutable context input from its proven original bytes
    RecoverAssignmentContext {
        work_item_id: String,
        attempt_id: String,
        #[arg(long)]
        task: String,
        #[arg(long)]
        candidate: String,
        #[arg(long)]
        assignment_digest: String,
        #[arg(long)]
        input: String,
        #[arg(long)]
        original_file: std::path::PathBuf,
        #[arg(long)]
        resolution: String,
    },

    /// Retry a paused Writer that did not produce a valid completion result
    RepairWriterCompletion {
        /// Work Item ID
        work_item_id: String,
        /// Attempt ID
        attempt_id: String,
        /// Human decision authorizing the exact Writer retry
        #[arg(long)]
        resolution: String,
    },

    /// Revise selected behavior Test references on a paused Attempt
    ReviseTestReferences {
        /// Work Item ID
        work_item_id: String,
        /// Attempt ID
        attempt_id: String,
        /// Replacement in `behavior-requirement-id=Test reference` form
        #[arg(long, value_name = "ID=REFERENCE", required = true)]
        reference: Vec<String>,
        /// Human decision authorizing the exact planning correction
        #[arg(long)]
        resolution: String,
    },

    /// Replace an approved Approach at an audited review frontier
    ///
    /// Supports uncertain Architecture review and failed-review round-cap
    /// frontiers with their existing recovery safeguards.
    /// For a failed-review round-cap frontier, the complete flow is:
    /// `fluent attempt extend <work-item-id> <attempt-id> --additional-write-rounds N --resolution '<authorization>'`
    /// `fluent attempt revise-approach <work-item-id> <attempt-id> --approach-file <path> --resolution '<authorization>'`
    ReviseApproach {
        /// Work Item ID
        work_item_id: String,
        /// Attempt ID
        attempt_id: String,
        /// Complete replacement Approach document
        #[arg(long, value_name = "PATH")]
        approach_file: std::path::PathBuf,
        /// Human decision authorizing the exact Approach revision
        #[arg(long)]
        resolution: String,
    },

    /// Advance an Attempt through the next safe transitions
    ///
    /// Recovers a released 0.6.0 Writer stranded by an exact zero-turn
    /// missing Claude conversation after checking its candidate and retained evidence.
    /// Status reports this command when that guarded recovery is available.
    Run {
        /// Work Item ID
        work_item_id: String,

        /// Attempt ID (default: most recently created Attempt)
        attempt_id: Option<String>,

        /// Disable sandbox
        #[arg(long)]
        no_sandbox: bool,

        /// Run in the background and print how to watch and stop it
        #[arg(long)]
        detach: bool,

        /// Retry only the paused Tester Task without Fluent's Tester sandbox
        #[arg(long, hide = true)]
        tester_no_sandbox: bool,

        /// Scheduler dispatch that admitted this launch
        #[arg(long, hide = true)]
        dispatch_id: Option<String>,

        /// Coding agent to launch: claude, codex, or pi
        #[arg(long)]
        coder: Option<String>,

        /// Model to use for all roles
        #[arg(long)]
        model: Option<String>,

        /// Reasoning effort to use for all roles
        #[arg(long)]
        effort: Option<String>,

        /// Coder for write Tasks: claude, codex, or pi
        #[arg(long)]
        write_coder: Option<String>,

        /// Model for write Tasks
        #[arg(long)]
        write_model: Option<String>,

        /// Effort for write Tasks
        #[arg(long)]
        write_effort: Option<String>,

        /// Coder for review Tasks: claude, codex, or pi
        #[arg(long)]
        review_coder: Option<String>,

        /// Model for review Tasks
        #[arg(long)]
        review_model: Option<String>,

        /// Effort for review Tasks
        #[arg(long)]
        review_effort: Option<String>,

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

    /// Persist a stop, settle all owned local workers, and suspend for explicit resume.
    /// Repeating cancellation after settlement leaves history unchanged.
    Cancel {
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

#[derive(Debug, Subcommand)]
pub enum OwnershipCommands {
    /// Read a proposed split and print a candidate-bound preview without adoption
    Preview {
        work_item_id: String,
        attempt_id: String,
        #[arg(long)]
        proposal_file: std::path::PathBuf,
    },
    /// Adopt the exact preview with a recorded operator decision
    Approve {
        work_item_id: String,
        attempt_id: String,
        #[arg(long)]
        preview_file: std::path::PathBuf,
        #[arg(long)]
        resolution: String,
    },
}

#[derive(Subcommand)]
pub enum AttemptEvidenceCommands {
    /// Classify paused failed host evidence as an authorized source correction
    Classify {
        /// Work Item ID
        work_item_id: String,
        /// Attempt ID
        attempt_id: String,
        /// Exact candidate commit
        #[arg(long)]
        candidate: String,
        /// Completed evidence-targeted review artifact
        #[arg(long, value_name = "PATH")]
        review_artifact: String,
        /// Digest of the attached immutable host evidence
        #[arg(long)]
        evidence_digest: String,
        /// Operator authorization for the source correction
        #[arg(long)]
        resolution: String,
    },
    /// Schedule one failed Tests Review's approved check as a verification Task.
    /// Run the printed Task command, then resume the Attempt to validate its receipt.
    Run {
        /// Work Item ID
        work_item_id: String,
        /// Attempt ID
        attempt_id: String,
        /// Failed Tests Review artifact this check addresses
        #[arg(long, value_name = "PATH")]
        review_artifact: String,
        /// Exact one-line command for a legacy review without Evidence-Command
        #[arg(long)]
        command: Option<String>,
        /// Candidate-relative directory for a legacy review without Evidence-Working-Directory
        #[arg(long, value_name = "PATH")]
        working_directory: Option<String>,
        /// Maximum command duration
        #[arg(long, default_value_t = 600)]
        timeout_seconds: u64,
    },
    /// Attach one versioned JSON evidence document to an unchanged candidate
    Attach {
        /// Work Item ID
        work_item_id: String,
        /// Attempt ID
        attempt_id: String,
        /// Exact candidate commit
        #[arg(long)]
        candidate: String,
        /// Host-produced evidence document
        #[arg(long, value_name = "PATH")]
        evidence_file: String,
        /// Failed review artifact this evidence addresses
        #[arg(long, value_name = "PATH")]
        review_artifact: Vec<String>,
        /// Failed-command recheck manifest this evidence addresses
        #[arg(long, value_name = "PATH")]
        recheck_artifact: Option<String>,
        /// Required Plan completion row completed by this authorized-host run
        #[arg(long, value_name = "ROW", conflicts_with_all = ["review_artifact", "recheck_artifact"])]
        plan_row: Option<String>,
        /// Stable canonical check selector printed in evidence recovery guidance
        #[arg(long, value_name = "ID", conflicts_with_all = ["review_artifact", "recheck_artifact", "plan_row"])]
        required_check: Option<String>,
        /// Pending Tests finding in the current Writer's immutable assignment
        #[arg(long, value_name = "ID", conflicts_with_all = ["review_artifact", "recheck_artifact", "plan_row", "required_check"])]
        finding: Option<String>,
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

        /// Run the detached post-merge review after landing (opt-in;
        /// omitted, the land stops after Learner handoff processing)
        #[arg(long)]
        post_merge_review: bool,

        /// Compatibility spelling for the default; conflicts with
        /// --post-merge-review
        #[arg(long, hide = true)]
        no_post_merge_review: bool,

        /// Coding agent to launch: claude or codex
        #[arg(long)]
        coder: Option<String>,

        /// Model to use for this land invocation
        #[arg(long)]
        model: Option<String>,

        /// Reasoning effort to use for this land invocation
        #[arg(long)]
        effort: Option<String>,

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

    /// Check the active Writer's current candidate before ending its Task
    CheckHandoff {
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

        /// Model to use for all roles
        #[arg(long)]
        model: Option<String>,

        /// Reasoning effort to use for all roles
        #[arg(long)]
        effort: Option<String>,

        /// Coder for write Tasks: claude, codex, or pi
        #[arg(long)]
        write_coder: Option<String>,

        /// Model for write Tasks
        #[arg(long)]
        write_model: Option<String>,

        /// Effort for write Tasks
        #[arg(long)]
        write_effort: Option<String>,

        /// Coder for review Tasks: claude, codex, or pi
        #[arg(long)]
        review_coder: Option<String>,

        /// Model for review Tasks
        #[arg(long)]
        review_model: Option<String>,

        /// Effort for review Tasks
        #[arg(long)]
        review_effort: Option<String>,

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

    /// Explicitly queue recovery of one suspended Attempt
    Resume {
        /// Work Item ID
        work_item_id: String,

        /// Exact needs-user Attempt to resume
        #[arg(long)]
        attempt: String,

        /// Numeric priority (higher = sooner; default 0)
        #[arg(long)]
        priority: Option<i64>,

        /// Approve this tester-harness recovery without Fluent's Tester sandbox
        #[arg(long)]
        no_sandbox: bool,
    },

    /// List queued Work Items
    List,

    /// Show the latest dispatch for a Work Item as JSON
    Show {
        /// Work Item ID
        work_item_id: String,
    },

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
    /// Verify retained release acceptance without running tests or changing Work
    Verify {
        /// Work Item that owns the reviewed release
        work_item_id: String,
        /// Successful Attempt that owns the exact candidate
        attempt_id: String,
        /// Full commit SHA of the clean current release checkout
        #[arg(long)]
        candidate: String,
    },
    /// Validate Tester readiness and run declared test commands
    Check {
        /// Disable sandbox
        #[arg(long)]
        no_sandbox: bool,
    },
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
    /// Poll the queue and run Work Items up to configured capacity
    Run {
        /// Seconds between queue polls when idle (default 30)
        #[arg(long)]
        poll_seconds: Option<u64>,
    },
    /// Show the elected coordinator and its exact Fluent build
    Status,
    /// Stop new claims and drain running Work in this project (or all registered projects)
    Stop {
        /// Drain the shared scheduler service across all registered projects
        #[arg(long)]
        all: bool,
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
        /// Requires a clean working tree outside `.fluent` and Fluent-managed
        /// skill trees.
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
// Skills
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum SkillsCommands {
    /// Refresh existing skills, or install with --agent and --project/--global
    Add {
        /// Install into the selected agents' global skill directories
        #[arg(short = 'g', long, conflicts_with = "project")]
        global: bool,

        /// Install into the selected agents' project skill directories
        #[arg(long, conflicts_with = "global")]
        project: bool,

        /// Select agents to install for: codex, claude, comma-separated, or *
        #[arg(long)]
        agent: Option<String>,
    },

    /// Print the path to a bundled skill's data directory
    Show {
        /// Print the path instead of the content
        #[arg(long)]
        path: bool,

        /// Skill name
        name: String,
    },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_reconfirm_checkout_route_parses_scoped_identity() {
        let cli = Cli::try_parse_from([
            "fluent",
            "slack",
            "reconfirm-checkout",
            "--project",
            "app",
            "--thread",
            "1712345678.123456",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Slack {
                command: SlackCommands::ReconfirmCheckout { project, thread }
            }) if project == "app" && thread == "1712345678.123456"
        ));
    }

    fn parsed_tester_recovery(args: &[&str]) -> (bool, Vec<String>) {
        let cli = Cli::try_parse_from(args).expect("Tester recovery command should parse");
        match cli.command {
            Some(Commands::Attempt {
                command:
                    AttemptCommands::Run {
                        tester_no_sandbox,
                        extra_args,
                        ..
                    },
            }) => (tester_no_sandbox, extra_args),
            _ => panic!("expected attempt run"),
        }
    }

    #[test]
    fn tester_recovery_capability_must_precede_trailing_attempt_arguments() {
        let (approved, extra_args) = parsed_tester_recovery(&[
            "fluent",
            "attempt",
            "run",
            "--tester-no-sandbox",
            "work-1",
            "attempt-1",
        ]);
        assert!(approved);
        assert!(extra_args.is_empty());
    }

    #[test]
    fn evidence_attach_parses_failed_recheck_frontier() {
        let cli = Cli::try_parse_from([
            "fluent",
            "attempt",
            "evidence",
            "attach",
            "work-1",
            "attempt-1",
            "--candidate",
            "abc123",
            "--evidence-file",
            "evidence.json",
            "--recheck-artifact",
            ".fluent/work/artifacts/work-1/attempt-1/recheck/failed-command-recheck.json",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Attempt {
                command: AttemptCommands::Evidence {
                    command: AttemptEvidenceCommands::Attach {
                        review_artifact,
                        recheck_artifact: Some(recheck),
                        ..
                    }
                }
            }) if review_artifact.is_empty() && recheck.ends_with("failed-command-recheck.json")
        ));
    }

    #[test]
    fn evidence_attach_parses_authorized_host_plan_row() {
        let cli = Cli::try_parse_from([
            "fluent",
            "attempt",
            "evidence",
            "attach",
            "work-1",
            "attempt-1",
            "--candidate",
            "abc123",
            "--evidence-file",
            "evidence.json",
            "--plan-row",
            "plan:step-6",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Attempt {
                command: AttemptCommands::Evidence {
                    command: AttemptEvidenceCommands::Attach {
                        plan_row: Some(row),
                        review_artifact,
                        recheck_artifact: None,
                        ..
                    }
                }
            }) if row == "plan:step-6" && review_artifact.is_empty()
        ));
    }

    #[test]
    fn evidence_classify_parses_candidate_bound_inputs() {
        let cli = Cli::try_parse_from([
            "fluent",
            "attempt",
            "evidence",
            "classify",
            "work-1",
            "attempt-1",
            "--candidate",
            "abc123",
            "--review-artifact",
            "review.md",
            "--evidence-digest",
            "sha256:evidence",
            "--resolution",
            "Correct the source.",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Attempt {
                command: AttemptCommands::Evidence {
                    command: AttemptEvidenceCommands::Classify { evidence_digest, .. }
                }
            }) if evidence_digest == "sha256:evidence"
        ));
    }

    #[test]
    fn candidate_ancestry_repair_parses_operator_resolution() {
        let cli = Cli::try_parse_from([
            "fluent",
            "attempt",
            "repair-candidate-ancestry",
            "work-1",
            "attempt-1",
            "--resolution",
            "Preserve the protected base.",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Attempt {
                command: AttemptCommands::RepairCandidateAncestry {
                    work_item_id,
                    attempt_id,
                    resolution,
                    restore_interrupted: false,
                    rejected_candidate: None,
                }
            }) if work_item_id == "work-1"
                && attempt_id == "attempt-1"
                && resolution == "Preserve the protected base."
        ));
    }

    #[test]
    fn candidate_metadata_repair_parses_operator_resolution() {
        let cli = Cli::try_parse_from([
            "fluent",
            "attempt",
            "repair-candidate-metadata",
            "work-1",
            "attempt-1",
            "--resolution",
            "Rewrite only malformed commit messages.",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Attempt {
                command: AttemptCommands::RepairCandidateMetadata {
                    work_item_id,
                    attempt_id,
                    resolution,
                }
            }) if work_item_id == "work-1"
                && attempt_id == "attempt-1"
                && resolution == "Rewrite only malformed commit messages."
        ));
    }

    #[test]
    fn test_reference_revision_parses_repeated_selected_requirements() {
        let cli = Cli::try_parse_from([
            "fluent",
            "attempt",
            "revise-test-references",
            "work-1",
            "attempt-1",
            "--reference",
            "behavior:readiness:b1=python3 tests/readiness.py",
            "--reference",
            "behavior:readiness:b2=python3 tests/readiness.py",
            "--resolution",
            "Use the owning public workflow.",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Attempt {
                command: AttemptCommands::ReviseTestReferences {
                    work_item_id,
                    attempt_id,
                    reference,
                    resolution,
                }
            }) if work_item_id == "work-1"
                && attempt_id == "attempt-1"
                && reference.len() == 2
                && resolution == "Use the owning public workflow."
        ));
    }

    #[test]
    fn approach_revision_parses_authorized_replacement() {
        let cli = Cli::try_parse_from([
            "fluent",
            "attempt",
            "revise-approach",
            "work-1",
            "attempt-1",
            "--approach-file",
            "replacement.md",
            "--resolution",
            "Adopt the reviewed design.",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Attempt {
                command: AttemptCommands::ReviseApproach {
                    work_item_id,
                    attempt_id,
                    approach_file,
                    resolution,
                }
            }) if work_item_id == "work-1"
                && attempt_id == "attempt-1"
                && approach_file == std::path::PathBuf::from("replacement.md")
                && resolution == "Adopt the reviewed design."
        ));
    }
}

#[derive(Subcommand)]
pub enum AgentSourceCommands {
    Terminal {
        #[arg(long)]
        project_root: PathBuf,
        #[arg(long)]
        identity: String,
        #[arg(long)]
        columns: u16,
        #[arg(long)]
        rows: u16,
    },
    Snapshot {
        #[arg(long)]
        project_root: PathBuf,
    },
}
