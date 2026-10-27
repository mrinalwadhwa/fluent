pub mod assignment_context_recovery;
pub mod atomic_write;
pub mod auto_merge;
pub(crate) mod baseline_cache;
pub mod candidate_ancestry_recovery;
pub mod candidate_gate;
pub mod candidate_metadata_recovery;
pub(crate) mod check_applicability;
pub mod claude_auth;
pub mod claude_diagnostics;
pub(crate) mod claude_session;
pub mod cleanup;
pub mod cli;
pub mod coder;
pub mod codex_worker;
pub mod config;
pub mod content;
pub mod credential;
pub mod dashboard;
pub mod dashboard_sources;
pub mod execution;
pub mod fargate;
pub mod fargate_bootstrap;
pub mod follow_up;
pub mod git;
pub mod guidance;
pub mod hooks;
pub mod host_evidence;
pub(crate) mod installed_tools;
pub mod keep_awake;
pub mod land_lock;
pub mod learner;
pub mod lease;
pub(crate) mod lifecycle_recovery;
pub mod lineage_lock;
pub mod native_agent;
pub mod notify;
pub mod os;
pub mod plan;
pub(crate) mod plan_command;
pub mod planning_approach_recovery;
pub mod planning_reference_recovery;
pub mod post_merge_review;
pub mod prep;
pub(crate) mod process_settlement;
pub(crate) mod provider_admission;
pub mod provider_evidence;
pub mod provider_readiness;
pub mod review;
pub mod review_diff_command;
pub(crate) mod review_inspection;
pub(crate) mod review_invalidation;
pub mod review_only_worktree;
pub mod transcript;
// Crate-private: the byte pump, its typed `TranscriptPumpError`, and the pump
// handle are used only by coder supervision, retry classification, and config
// resolution inside this crate. They are deliberately not part of the public API.
pub(crate) mod transcript_pump;
pub mod version;
pub mod work_attempt_loop;
pub mod work_merge_executor;
pub mod work_model;
pub mod work_status;
pub mod work_task_executor;
pub mod worktree;
pub(crate) mod writer_cache;
pub mod writer_completion_recovery;
pub(crate) mod writer_context;

pub mod observations;
pub mod queue;
pub(crate) mod recovery_contracts;
pub mod recovery_replay;
pub mod scheduler;
pub mod scheduler_service;
pub mod scope_calibration;
pub mod setup;
pub mod skill_install;
pub mod slack_collaboration;
pub mod terminal_sessions;
pub mod terminal_view;
#[cfg(test)]
pub(crate) mod test_lock_probe;
pub mod tester;
pub(crate) mod tester_evidence;
pub mod tester_harness_recovery;
pub mod update;
pub mod usage;
mod usage_limit_contracts;
pub mod usage_limit_recovery;

pub mod ownership_recovery;

pub mod verification;
pub(crate) mod verification_policy;

pub mod verification_assessment;
pub mod verification_reference;

pub mod conversation;
pub mod discord_collaboration;

pub mod work_approval;
