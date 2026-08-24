//! The overlap index.
//!
//! Both prototype paths reduce to the same question — *who else has touched
//! this range?* The two answers differ only in tense:
//!
//!   * a thread editing it **right now**  -> merge-conflict warning
//!   * a person who edited it **in the past** -> expertise suggestion
//!
//! Keeping them in one index is the point. It means conflict detection and
//! expert routing cannot drift apart, and a single anchor query serves both.

use std::ops::Range;

use crate::model::actor::ActorId;
use crate::model::thread::ThreadId;

/// A range some thread's worktree is currently dirty in.
#[derive(Clone, Debug)]
pub struct LiveTouch {
    pub thread: ThreadId,
    pub file: String,
    pub lines: Range<u32>,
    /// Turn on which this thread last wrote here.
    pub turn: u32,
}

/// A range someone authored historically. In the real thing this comes from
/// `git blame`; here it is hand-authored seed data, so treat the *ranking* as
/// illustrative and only the *interaction* as evaluated.
#[derive(Clone, Debug)]
pub struct HistoricalTouch {
    pub author: ActorId,
    pub file: String,
    pub lines: Range<u32>,
    pub commits: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    /// Ranges intersect. A real merge conflict is likely.
    Overlapping,
    /// Ranges are within the proximity window but do not intersect. Git would
    /// merge these cleanly; a human might still want to know.
    Adjacent,
}

#[derive(Clone, Debug)]
pub struct Conflict {
    pub other_thread: ThreadId,
    pub file: String,
    pub our_lines: Range<u32>,
    pub their_lines: Range<u32>,
    pub kind: ConflictKind,
    pub their_turn: u32,
}

#[derive(Clone, Debug)]
pub struct Expert {
    pub author: ActorId,
    pub lines_authored: u32,
    pub commits: u32,
    /// Why this person surfaced, in words. Ranking that cannot explain itself
    /// reads as wrong even when it is right, so the reason is part of the
    /// result rather than something the UI reconstructs.
    pub reason: String,
}

pub struct OverlapIndex {
    pub live: Vec<LiveTouch>,
    pub history: Vec<HistoricalTouch>,
    /// How many lines apart two edits can be and still be worth mentioning.
    /// Line-exact overlap alone is too strict — two threads rewriting adjacent
    /// arms of the same `match` will merge cleanly and still be a problem.
    /// Purely a judgement call; tune it here.
    pub proximity: u32,
}

impl OverlapIndex {
    pub fn new(proximity: u32) -> Self {
        Self {
            live: Vec::new(),
            history: Vec::new(),
            proximity,
        }
    }

    /// Path 1: which other live threads collide with `lines` in `file`.
    pub fn conflicts(&self, asking: ThreadId, file: &str, lines: &Range<u32>) -> Vec<Conflict> {
        let mut out: Vec<Conflict> = self
            .live
            .iter()
            .filter(|t| t.thread != asking && t.file == file)
            .filter_map(|t| {
                let kind = if intersects(lines, &t.lines) {
                    ConflictKind::Overlapping
                } else if gap(lines, &t.lines) <= self.proximity {
                    ConflictKind::Adjacent
                } else {
                    return None;
                };
                Some(Conflict {
                    other_thread: t.thread,
                    file: file.to_string(),
                    our_lines: lines.clone(),
                    their_lines: t.lines.clone(),
                    kind,
                    their_turn: t.turn,
                })
            })
            .collect();
        // Overlapping before adjacent; most recent activity first.
        out.sort_by_key(|c| {
            (
                c.kind != ConflictKind::Overlapping,
                std::cmp::Reverse(c.their_turn),
            )
        });
        out
    }

    /// Path 2: who knows about `lines` in `file`.
    pub fn experts(&self, file: &str, lines: &Range<u32>) -> Vec<Expert> {
        let mut tally: Vec<Expert> = Vec::new();
        for touch in self.history.iter().filter(|t| t.file == file) {
            let overlap = overlap_len(lines, &touch.lines);
            if overlap == 0 {
                continue;
            }
            match tally.iter_mut().find(|e| e.author == touch.author) {
                Some(existing) => {
                    existing.lines_authored += overlap;
                    existing.commits += touch.commits;
                }
                None => tally.push(Expert {
                    author: touch.author,
                    lines_authored: overlap,
                    commits: touch.commits,
                    reason: String::new(),
                }),
            }
        }
        let span = (lines.end - lines.start).max(1);
        for expert in &mut tally {
            expert.reason = format!(
                "wrote {} of these {} lines, across {} commit{}",
                expert.lines_authored,
                span,
                expert.commits,
                if expert.commits == 1 { "" } else { "s" }
            );
        }
        tally.sort_by_key(|e| std::cmp::Reverse(e.lines_authored));
        tally
    }
}

fn intersects(a: &Range<u32>, b: &Range<u32>) -> bool {
    a.start < b.end && b.start < a.end
}

fn gap(a: &Range<u32>, b: &Range<u32>) -> u32 {
    if intersects(a, b) {
        0
    } else if a.end <= b.start {
        b.start - a.end
    } else {
        a.start - b.end
    }
}

fn overlap_len(a: &Range<u32>, b: &Range<u32>) -> u32 {
    let start = a.start.max(b.start);
    let end = a.end.min(b.end);
    end.saturating_sub(start)
}
