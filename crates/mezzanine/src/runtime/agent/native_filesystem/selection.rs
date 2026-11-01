//! Native selection before shell lowering, with bounded semantic success custody.
//!
//! Only actual provider dispatch selects continuation intent. Parsed patch data
//! and immutable filesystem source distinguish successful mutations independently
//! of shell commands or action labels. History never grants filesystem authority
//! and never evicts a success to permit replay; turn retirement releases it.

use super::*;
use crate::runtime::processes::native_filesystem_context::NativeFilesystemContext;
use mez_agent::ActionResult;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Finite history per selected turn; exhaustion refuses new work before dispatch.
const MAX_SUCCESSES: usize = 1024;
/// Finite queued-selection custody, independent of the four admitted workers.
const MAX_WAITING_SELECTIONS: usize = 64;

/// A deferred accepted source cannot rebase its payload or context while busy.
struct WaitingSelection {
    /// Original authority/navigation/lifetime source, never refreshed on retry.
    source: NativeFilesystemContext,
    /// Exact accepted payload rather than normalized duplicate identity.
    payload: [u8; 32],
    /// Original accepted response group, not merely a reused action label.
    group: mez_agent::ContextExecutionGroupId,
    /// Original action budget includes time waiting for finite worker capacity.
    deadline: std::time::Instant,
}

/// Volatile history owned by actual selected turns, not a global replay cache.
#[derive(Default)]
pub(in crate::runtime::agent) struct NativeFilesystemSelection {
    /// Original contexts retain their mode allocations to prevent pointer ABA.
    turns: BTreeMap<String, Vec<(NativeFilesystemContext, [u8; 32])>>,
    /// Deferred original selections; these grant no claim or mutation rights.
    waiting: BTreeMap<(String, String), WaitingSelection>,
}
impl NativeFilesystemSelection {
    /// Releases only history, never an attempt, worker or storage obligation.
    pub(in crate::runtime::agent) fn forget(&mut self, turn: &str) {
        self.turns.remove(turn);
        self.waiting.retain(|(owner, _), _| owner != turn);
    }
    /// Reports actual selector participation rather than direct queue admission.
    pub(super) fn selected(&self, turn: &str) -> bool {
        self.turns.contains_key(turn)
    }
    /// Keeps a deferred native selection native even after a mode change.
    pub(in crate::runtime::agent) fn waits_for(&self, turn: &str, action: &str) -> bool {
        self.waiting
            .contains_key(&(turn.to_string(), action.to_string()))
    }
    /// Removes only unissued selection intent; actual attempts stay independent.
    pub(in crate::runtime::agent) fn forget_waiter(&mut self, turn: &str, action: &str) {
        self.waiting.remove(&(turn.to_string(), action.to_string()));
    }
    /// Records a confirmed success once, and never recreates retired turn state.
    pub(in crate::runtime::agent) fn confirm(&mut self, work: &NativeFilesystemWork) -> Result<()> {
        let Some(successes) = self.turns.get_mut(&work.owner.turn.turn_id) else {
            return Ok(());
        };
        let semantic = semantic_identity(&work.identity.operation)?;
        if !successes
            .iter()
            .any(|(source, key)| *key == semantic && source.same_mutation_context(&work.source))
        {
            if successes.len() >= MAX_SUCCESSES {
                return Err(MezError::invalid_state(
                    "native patch success history reservation exhausted",
                ));
            }
            successes.push((work.source.clone(), semantic));
        }
        Ok(())
    }
}

/// Selection either reserves one original worker or settles a proven duplicate.
/// None means finite capacity remains owned elsewhere and this slot must wait.
pub(in crate::runtime::agent) enum Selection {
    /// One new exact queued owner; no effect has run yet.
    Queued,
    /// Original semantic mutation already succeeded in the same source context.
    Duplicate(Box<ActionResult>),
    /// Backpressure is not failure and cannot reset a turn/action budget.
    Pending,
}

impl RuntimeSessionService {
    /// Selects native patches before shell planning, probes or executable hooks.
    /// Parse/source failures propagate before admission; real execution policy is
    /// still rechecked by the actual original actor claim and every commit.
    pub(in crate::runtime::agent) fn select_native_filesystem_action(
        &mut self,
        turn: &AgentTurnRecord,
        action: &AgentAction,
        execution: &AgentTurnExecution,
    ) -> Result<Selection> {
        let AgentActionPayload::ApplyPatch { patch, strip } = &action.payload else {
            return Err(MezError::invalid_args(
                "native patch selector requires apply_patch",
            ));
        };
        let operation = NativePatchOperation::parse(patch, *strip)?;
        let source = self.capture_native_filesystem_context(turn)?;
        let semantic = semantic_identity(&operation)?;
        let key = (turn.turn_id.clone(), action.id.clone());
        let payload: [u8; 32] = Sha256::digest(patch.as_bytes()).into();
        let group = super::super::provider_execution::provider_log_execution_group_id(execution)?;
        let waiting_deadline = self
            .agent
            .native_filesystem_actions
            .selection
            .waiting
            .get(&key)
            .map(|original| original.deadline);
        if waiting_deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return Err(MezError::conflict(
                "deferred native patch original deadline elapsed",
            ));
        }
        if let Some(original) = self
            .agent
            .native_filesystem_actions
            .selection
            .waiting
            .get(&key)
            && (original.payload != payload
                || original.group != group
                || !original.source.same_mutation_context(&source))
        {
            return Err(MezError::conflict(
                "deferred native patch original source changed",
            ));
        }
        if let Some(successes) = self
            .agent
            .native_filesystem_actions
            .selection
            .turns
            .get(&turn.turn_id)
        {
            if successes
                .iter()
                .any(|(original, key)| *key == semantic && original.same_mutation_context(&source))
            {
                if !self.native_filesystem_context_is_current(turn, &source)? {
                    return Err(MezError::conflict(
                        "native duplicate source changed before settlement",
                    ));
                }
                let mut result = ActionResult::succeeded(turn, action, vec!["duplicate file mutation skipped because the same mutation already succeeded".into()], Some(serde_json::json!({
                    "guard":"native_patch_semantic_success", "reason":"repeated_successful_file_mutation",
                    "execution_target":"native_runtime", "sent_to_pane":false,
                    "dispatch_state":"suppressed_duplicate", "automatic_replay_allowed":false,
                }).to_string()));
                result.permission_evaluation = execution
                    .action_results
                    .iter()
                    .find(|slot| slot.action_id == action.id)
                    .and_then(|slot| slot.permission_evaluation.clone());
                self.agent
                    .native_filesystem_actions
                    .selection
                    .waiting
                    .remove(&key);
                return Ok(Selection::Duplicate(Box::new(result)));
            }
            if successes.len() >= MAX_SUCCESSES {
                return Err(MezError::conflict(
                    "native patch success history capacity exhausted",
                ));
            }
        }
        if !self.agent.native_filesystem_actions.has_capacity() {
            let waiting = &mut self.agent.native_filesystem_actions.selection.waiting;
            if !waiting.contains_key(&key) {
                if waiting.len() >= MAX_WAITING_SELECTIONS {
                    return Err(MezError::conflict(
                        "native patch pending selection capacity exhausted",
                    ));
                }
                let remaining = if turn.deadline_at_unix_millis == 0 {
                    30_000
                } else {
                    turn.deadline_at_unix_millis
                        .saturating_sub(crate::runtime::current_unix_millis())
                        .min(30_000)
                };
                if remaining == 0 {
                    return Err(MezError::conflict("native pending turn deadline elapsed"));
                }
                waiting.insert(
                    key,
                    WaitingSelection {
                        source,
                        payload,
                        group,
                        deadline: std::time::Instant::now()
                            .checked_add(std::time::Duration::from_millis(remaining))
                            .ok_or_else(|| {
                                MezError::invalid_state("native pending deadline overflow")
                            })?,
                    },
                );
            }
            return Ok(Selection::Pending);
        }
        // Provider finalization still owns its working execution. Install the
        // accepted canonical source before reserving any asynchronous producer;
        // existing groups must validate their original dispatcher transition.
        if self.completion_owns_transcript_policy(&turn.turn_id) {
            let expected = self
                .agent_turn_executions()
                .get(&turn.turn_id)
                .cloned()
                .ok_or_else(|| {
                    MezError::invalid_state("native selector lost original execution")
                })?;
            if expected != *execution {
                self.adopt_completion_canonical_transition(turn, &expected, execution)?;
            }
        } else {
            self.agent_turn_executions_mut()
                .insert(turn.turn_id.clone(), execution.clone());
        }
        self.queue_native_filesystem_action_with_deadline(
            turn,
            action,
            execution,
            waiting_deadline,
        )?;
        self.agent
            .native_filesystem_actions
            .selection
            .waiting
            .remove(&key);
        self.agent
            .native_filesystem_actions
            .approvals
            .remove(&(turn.turn_id.clone(), action.id.clone()));
        self.agent
            .native_filesystem_actions
            .selection
            .turns
            .entry(turn.turn_id.clone())
            .or_default();
        self.retain_completion_settlement_intent(
            &turn.turn_id,
            super::super::completion::CompletionSettlementIntent::Ordinary,
        );
        self.record_agent_loop_apply_patch_for_turn(&turn.turn_id);
        Ok(Selection::Queued)
    }

    /// Wakes deferred original selections when an actual transfer frees capacity.
    /// Each dispatcher retains its own canonical/storage failure; reporting an
    /// unrelated waiter's error cannot revoke the completed producer's ack.
    pub(in crate::runtime::agent) fn resume_waiting_filesystem_selections(&mut self) {
        let turns = self
            .agent
            .native_filesystem_actions
            .selection
            .waiting
            .keys()
            .map(|(turn, _)| turn.clone())
            .collect::<std::collections::BTreeSet<_>>();
        for turn in turns {
            if !self.agent.native_filesystem_actions.has_capacity() {
                break;
            }
            if let Err(error) = self.dispatch_stored_running_shell_actions(&turn) {
                let _ = self.append_runtime_diagnostic_event(
                    serde_json::json!({
                        "kind": "native_filesystem_capacity_resume_failed", "turn_id": turn,
                        "error_kind": format!("{:?}", error.kind()),
                    })
                    .to_string(),
                );
            }
        }
    }

    /// Continues only actual selected live work after exact own transfer. Original
    /// archive/failure/publication gates remain authoritative; detached backing
    /// never selects a replacement response or starts new native effects.
    pub(in crate::runtime::agent) fn continue_selected_filesystem_execution(
        &mut self,
        turn: &str,
    ) -> Result<()> {
        if !self
            .agent
            .native_filesystem_actions
            .selection
            .selected(turn)
            || !self.completion_settlement_binding_current(turn)
            || self.agent_turn_is_human_paused(turn)
            || !self.completion_group_publication_ready(turn)
        {
            return Ok(());
        }
        self.ensure_completion_required_gates_ready(turn)?;
        if let Some(execution) = self.agent_turn_executions().get(turn).cloned()
            && self.execution_has_pending_shell_dispatch(turn, &execution)
        {
            self.dispatch_stored_running_shell_actions(turn)?;
        }
        self.recover_retained_completion_settlement(turn)?;
        Ok(())
    }
}

/// Hashes typed parsed syntax, never generated shell source. Structured framing
/// distinguishes operation kinds, ordered paths, hunk anchors/ranges and content;
/// normalized CRLF/outer wrappers produce the same identity within this process.
fn semantic_identity(operation: &NativePatchOperation) -> Result<[u8; 32]> {
    use mez_agent::semantic_patch::MezPatchOperation;
    let operations = operation
        .patch
        .operations
        .iter()
        .map(|operation| match operation {
            MezPatchOperation::Add { path, content } => serde_json::json!(["add", path, content]),
            MezPatchOperation::Delete { path } => serde_json::json!(["delete", path]),
            MezPatchOperation::Update {
                path,
                move_to,
                hunks,
                trailing_newline,
            } => serde_json::json!([
                "update",
                path,
                move_to,
                trailing_newline,
                hunks
                    .iter()
                    .map(|hunk| serde_json::json!([
                        hunk.anchors,
                        hunk.range_hint.map(|range| range.old_start),
                        hunk.replace_whole_file,
                        hunk.old,
                        hunk.new
                    ]))
                    .collect::<Vec<_>>()
            ]),
        })
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&serde_json::json!([
        "mez-native-patch-semantic-v1",
        operations
    ]))
    .map_err(|_| MezError::invalid_state("native patch semantic identity cannot be encoded"))?;
    Ok(Sha256::digest(bytes).into())
}
