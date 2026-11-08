//! Actor-owned canonical compaction publication through the sole persistence lane.
//! Request selection precedes inference. Workers retain the exact proposal and
//! physical attempt lifetime; only the actor can adopt a receipt into live state.

use crate::runtime::RuntimeSessionService;
use crate::storage::transcript::replay_epoch::{
    ReplayEpochCommit, ReplayEpochPublication, ReplaySourceOrigin,
};
use crate::{MezError, Result};
use mez_agent::{AgentContext, ReplayConversationId, ReplayEpochContext, ReplayHeadPublication};
use std::sync::Arc;

mod adoption;
mod manual;
mod recovery;
pub(in crate::runtime::commands::compaction) use manual::manual_replay_task;
mod source;
mod worker;
pub(super) use source::Preparation;
pub(crate) use source::RuntimeCompactionReplaySource;
pub(crate) use worker::{
    RuntimeReplayEpochAttempt, RuntimeReplayEpochCompletion, RuntimeReplayEpochPublication,
};
