//! Participants. A thread's actors are humans plus exactly one agent.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ActorId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActorKind {
    Human,
    Agent,
}

#[derive(Clone, Debug)]
pub struct Actor {
    pub id: ActorId,
    pub handle: String,
    pub kind: ActorKind,
    /// Cursor/attribution colour. Sourced from Delta's brand card, which uses
    /// the same four swatches as the per-participant cursors in `collab.mp4`.
    pub color: u32,
}

/// The four corner swatches from Delta's social card, in order.
pub const CURSOR_COLORS: [u32; 4] = [0xD4A017, 0xD4D4B0, 0x2E6FBF, 0xE33F26];
