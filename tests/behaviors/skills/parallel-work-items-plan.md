# Scenario: Plan execution for independent Work Items

## Opening statement
The approach for admin audit logs and billing exports is approved. Help
me plan the execution.

## Hidden context
- The approach identified two independent work areas: admin audit logs
  and billing exports.
- Audit logs need a Work Item that records admin actions, exposes them
  in the admin UI, and verifies event persistence plus UI visibility.
- Billing exports need a separate Work Item that produces downloadable
  CSV exports and verifies export contents plus authorization checks.
- The two efforts share only a user identity contract and can proceed in
  parallel once that contract is named.
- The user wants parallel execution, but the normal Work-model shape
  should be peer Work Items with their own Attempts, Workspaces, and
  Merge Candidates.
- The plan should include a sync point for validating the shared user
  identity contract before either Work Item lands.
- Legacy child-run groups are appropriate only if the agent explicitly
  says the Work model cannot express the required coordination.
- Work-model Task dependencies are not available as a default execution
  structure; likely follow-up Tasks may be notes only.
- Each peer Work Item should own its own Attempt, Workspace, Merge
  Candidate expectation, and Task notes. Shared sequencing belongs in
  sync points or interface notes, not in a shared Attempt/Task section.

## Evaluation criteria
- Did the agent assess that the two independent areas should become
  peer Work Items rather than one Work Item with parallel Tasks?
- Did it avoid presenting Work-model Tasks with executable dependencies
  as the default parallel structure?
- Did it avoid using legacy child-run groups as the default plan shape?
- Did it name separate Work Items, Attempts, Workspaces, and Merge
  Candidates for the independent efforts?
- Did it avoid creating a shared Attempt or Task sequence across the
  peer Work Items?
- Did it define a sync point around the shared user identity contract
  and say whether it blocks either Work Item from merging?
- Did it keep the discussion paced by asking about the split before
  finalizing the full plan?
- Did the final plan use Work Item planning vocabulary and include
  verification for each independent Work Item?
