//! Threads. One thread owns one worktree — Delta creates `.delta/worktrees/<id>`
//! per thread, which is what makes cross-thread conflict structural rather
//! than incidental.

use crate::model::actor::ActorId;
use crate::model::comment::CommentSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ThreadId(pub u32);

/// Zed ships `AgentThreadStatus { Completed, Running, WaitingForConfirmation,
/// Error }`. `Conflicting` is ours: the sidebar already carries a status dot
/// per thread, so conflict detection extends an existing vocabulary rather
/// than inventing a new indicator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadStatus {
    Completed,
    Running,
    WaitingForConfirmation,
    Error,
    Conflicting,
}

#[derive(Clone, Debug)]
pub struct Thread {
    pub id: ThreadId,
    pub title: String,
    pub project: String,
    pub status: ThreadStatus,
    /// `.delta/worktrees/<slug>` — one per thread.
    pub worktree: String,
    pub branch: String,
    pub participants: Vec<ActorId>,
    pub added: u32,
    pub removed: u32,
    /// Turn counter. Pending comments ship at turn boundaries, so this is what
    /// conflict warnings and delivery batches key off.
    pub turn: u32,
    pub unread: bool,
    pub shared: bool,
}

pub struct ThreadState {
    pub thread: Thread,
    pub comments: CommentSet,
}
