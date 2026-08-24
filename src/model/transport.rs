//! The seam a real backend plugs into.
//!
//! Sending a comment to another thread is the one operation that will not stay
//! in-process. Today the recipient is a `Vec` in the same window; tomorrow it is
//! another machine, and later it is an agent doing the sending rather than a
//! person. So it is expressed as a trait with an explicit envelope and receipt
//! rather than as a method that reaches into the other thread's state.
//!
//! Two things are deliberately *returned* rather than assumed by the sender:
//! where the anchor landed, and whether the comment is armed. Only the
//! recipient can answer either -- the sender cannot see the other worktree, and
//! does not know whether that agent is mid-turn.

use crate::model::actor::ActorId;
use crate::model::anchor::{PortableAnchor, Resolution};
use crate::model::comment::{CommentId, Delivery};
use crate::model::thread::ThreadId;

/// One thing handed from one thread to another.
#[derive(Clone, Debug)]
pub struct Envelope {
    pub from: ThreadId,
    pub to: ThreadId,
    /// Who pushed it. Often, but not always, the author.
    pub sender: ActorId,
    /// Who wrote it originally, preserved across every hop.
    pub author: ActorId,
    /// Travels verbatim and is re-resolved on arrival, never translated here.
    pub anchor: PortableAnchor,
    /// Empty means the snippet *is* the message.
    pub body: String,
    pub mentions: Vec<ActorId>,
    /// Title of the thread it came from, for provenance the recipient can read
    /// without a second lookup.
    pub from_title: String,
}

impl Envelope {
    pub fn is_snippet_only(&self) -> bool {
        self.body.trim().is_empty()
    }
}

/// What the recipient did with it.
#[derive(Clone, Debug)]
pub struct Receipt {
    pub comment: CommentId,
    /// Where the anchor landed *there*. `Orphaned` is a success, not a failure:
    /// the content arrived, it just has nowhere to attach.
    pub resolution: Resolution,
    /// Armed, or waiting for that thread's turn to end.
    pub delivery: Delivery,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendError {
    /// A thread cannot send to itself.
    SameThread,
    /// An agent tried to hand a comment back to where it came from. A person
    /// may do this deliberately; an agent doing it is the A -> B -> A loop.
    WouldLoop,
    UnknownThread(ThreadId),
}

/// Anything that can accept a cross-thread send.
///
/// The mock implements this in-process. A real Delta would implement it over
/// the wire, and nothing above this line would need to know which.
pub trait ThreadTransport {
    fn send(&mut self, envelope: Envelope) -> Result<Receipt, SendError>;
}
