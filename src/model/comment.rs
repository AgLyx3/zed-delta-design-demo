//! Comments.
//!
//! A comment is anchored content plus provenance plus a delivery decision.
//! It is the only object that crosses a thread boundary, so it carries
//! everything a receiving thread needs to render it without trusting the
//! sender's line numbers.

use crate::model::actor::ActorId;
use crate::model::anchor::PortableAnchor;
use crate::model::thread::ThreadId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CommentId(pub u32);

/// Where a comment came from.
///
/// Handoff is direct-injection: a comment sent from another thread lands in
/// the recipient's comment set with no accept step. That makes provenance
/// load-bearing rather than decorative — it is the only thing distinguishing
/// a teammate's deliberate note from another thread's spillover. Mirrors
/// Delta's `add_comment_with_creator`, which exists precisely to attribute a
/// comment to an actor other than the replica performing the insertion.
#[derive(Clone, Debug)]
pub enum Origin {
    Local,
    Forwarded {
        from_thread: ThreadId,
        from_thread_title: String,
        /// Who wrote it originally, preserved across the hop.
        original_author: ActorId,
        /// Who pushed it here. Often but not always the same person.
        forwarded_by: ActorId,
    },
}

impl Origin {
    pub fn is_foreign(&self) -> bool {
        matches!(self, Origin::Forwarded { .. })
    }
}

/// Whether this comment will be handed to the model on the next turn.
///
/// Delta batches: comments accumulate as pending and ship with the next user
/// prompt. Their own plan text calls this "pending-comment delivery" and gives
/// each comment a flag for "whether the comment participates in model
/// delivery" — seeded tutorial content is excluded so it can't masquerade as
/// user intent. We reuse that flag as the recipient's escape hatch: an
/// injected comment can be withheld before the next turn fires, which is the
/// only brake on one thread steering another thread's agent.
///
/// `Queued` exists so nobody has to make this decision by hand. A comment that
/// arrives while the recipient's agent is mid-turn waits for the turn boundary
/// and then arms itself. Someone who spots a bug and wants to note it in
/// another thread should not have to reason about whether that thread is busy;
/// that is the system's job, not theirs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Landed mid-turn. Promotes to `Pending` when the recipient's turn ends.
    Queued,
    /// Put here by the workspace rather than by a person, and waiting on
    /// someone in *this* thread to approve it. Unlike `Queued` it never
    /// resolves itself -- a turn boundary must not quietly approve something
    /// nobody agreed to.
    Proposed,
    Pending,
    Withheld,
    Delivered { turn: u32 },
}

/// A reply that references its parent by id.
///
/// Delta renders these as `↪ Re: <author>` quote blocks. We do not know
/// whether theirs are structured or a prompt-level convention; we model them
/// structured so replies stay navigable and countable instead of being a
/// string the model happened to emit.
#[derive(Clone, Debug)]
pub struct Reply {
    pub author: ActorId,
    pub body: String,
    pub in_reply_to: CommentId,
}

#[derive(Clone, Debug)]
pub struct Comment {
    pub id: CommentId,
    pub anchor: PortableAnchor,
    pub body: String,
    pub author: ActorId,
    pub origin: Origin,
    pub delivery: Delivery,
    pub replies: Vec<Reply>,
    pub resolved: bool,
    /// Logical tick. No wall clock: it would make the mock nondeterministic
    /// and the scripted teammate unreproducible.
    pub created_at: u64,
}

impl Comment {
    pub fn new(
        id: CommentId,
        anchor: PortableAnchor,
        body: impl Into<String>,
        author: ActorId,
        created_at: u64,
    ) -> Self {
        Self {
            id,
            anchor,
            body: body.into(),
            author,
            origin: Origin::Local,
            delivery: Delivery::Pending,
            replies: Vec::new(),
            resolved: false,
            created_at,
        }
    }

    /// Produce the copy of this comment that lands in `to_thread`.
    ///
    /// The anchor travels verbatim and is re-resolved on arrival against the
    /// recipient's own snapshot; we deliberately do not translate line numbers
    /// here, because the sender has no view of the recipient's worktree.
    pub fn forward(
        &self,
        new_id: CommentId,
        from_thread: ThreadId,
        from_thread_title: impl Into<String>,
        forwarded_by: ActorId,
        created_at: u64,
    ) -> Comment {
        Comment {
            id: new_id,
            anchor: self.anchor.clone(),
            body: self.body.clone(),
            author: self.author,
            origin: Origin::Forwarded {
                from_thread,
                from_thread_title: from_thread_title.into(),
                original_author: self.author,
                forwarded_by,
            },
            // Injected comments arrive pending, per (A) — but see `Delivery`:
            // the recipient can withhold before the next turn.
            delivery: Delivery::Pending,
            replies: Vec::new(),
            resolved: false,
            created_at,
        }
    }
}

/// A thread's comments. Named after Delta's `Thread::comment_set`.
#[derive(Default, Debug)]
pub struct CommentSet {
    pub comments: Vec<Comment>,
    next_id: u32,
}

impl CommentSet {
    pub fn alloc_id(&mut self) -> CommentId {
        self.next_id += 1;
        CommentId(self.next_id)
    }

    pub fn insert(&mut self, comment: Comment) {
        self.comments.push(comment);
    }

    pub fn get_mut(&mut self, id: CommentId) -> Option<&mut Comment> {
        self.comments.iter_mut().find(|c| c.id == id)
    }

    /// The batch that would ship with the next prompt.
    pub fn pending(&self) -> impl Iterator<Item = &Comment> {
        self.comments
            .iter()
            .filter(|c| c.delivery == Delivery::Pending && !c.resolved)
    }

    pub fn pending_count(&self) -> usize {
        self.pending().count()
    }
}
