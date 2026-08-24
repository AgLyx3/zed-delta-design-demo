//! Portable anchors.
//!
//! An anchor identifies a span of content. The hard requirement is that an
//! anchor must survive being *sent to another thread*, whose worktree has
//! diverged from the one the anchor was captured in. Delta's own anchors
//! (`DiffAnchor`) resolve against the thread's replica; ours must resolve
//! against an arbitrary snapshot, degrading gracefully when it can't.
//!
//! Therefore: never store a bare line number as the source of truth. Line
//! ranges appear here only as `captured_lines`, a hint used to bias the
//! search. Resolution is always content-first.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;

/// What kind of block an anchor points at. Delta's anchors are block-level in
/// every screenshot we have (a whole doc-comment block, a whole list item), so
/// we model blocks rather than character ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    ListItem,
    Heading,
    CodeBlock,
    DiffHunk,
}

/// A span of content that can be re-resolved in a different snapshot.
#[derive(Clone, Debug)]
pub struct PortableAnchor {
    /// Repo-relative path, POSIX-delimited.
    pub file: String,
    pub kind: BlockKind,
    /// Hash of the anchored text at capture time. Primary identity.
    pub content_hash: u64,
    /// The anchored text itself, so an orphaned anchor can still be quoted.
    pub content: String,
    /// Lines immediately before/after at capture time, used to re-find the
    /// block when its content changed but its surroundings didn't.
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
    /// Where it was when captured. A hint for search order, never authoritative.
    pub captured_lines: Range<u32>,
    /// True when the span was auto-expanded from a caret rather than selected
    /// by hand. Delta carries this as `target_was_auto_selected`; a loosely
    /// chosen span should re-anchor more permissively than a deliberate one.
    pub auto_selected: bool,
}

/// The outcome of resolving an anchor against a snapshot. Callers must handle
/// `Orphaned` — that is the whole point of the type.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolution {
    /// Content hash matched. Same block, possibly moved.
    Exact { lines: Range<u32> },
    /// Content changed but surrounding context located it.
    Shifted { lines: Range<u32>, similarity: f32 },
    /// Nothing matched well enough. Render as a quoted snippet with a backlink
    /// to the originating thread.
    Orphaned,
}

impl Resolution {
    pub fn lines(&self) -> Option<Range<u32>> {
        match self {
            Resolution::Exact { lines } => Some(lines.clone()),
            Resolution::Shifted { lines, .. } => Some(lines.clone()),
            Resolution::Orphaned => None,
        }
    }

    pub fn is_orphaned(&self) -> bool {
        matches!(self, Resolution::Orphaned)
    }
}

pub fn hash_content(text: &str) -> u64 {
    // Whitespace-insensitive so reformatting doesn't orphan every anchor.
    let normalized: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    hasher.finish()
}

/// A file as it exists in one thread's worktree. Anchors resolve against this.
pub struct Snapshot {
    pub file: String,
    pub lines: Vec<String>,
}

impl Snapshot {
    pub fn new(file: impl Into<String>, text: &str) -> Self {
        Self {
            file: file.into(),
            lines: text.lines().map(str::to_string).collect(),
        }
    }

    fn slice(&self, range: &Range<u32>) -> String {
        let start = (range.start as usize).min(self.lines.len());
        let end = (range.end as usize).min(self.lines.len());
        self.lines[start..end].join("\n")
    }
}

impl PortableAnchor {
    /// Capture an anchor from a snapshot.
    pub fn capture(
        snapshot: &Snapshot,
        kind: BlockKind,
        lines: Range<u32>,
        auto_selected: bool,
    ) -> Self {
        let content = snapshot.slice(&lines);
        let ctx = 3usize;
        // Clamp both ends the way `slice` does. `capture` is handed ranges that
        // came from another worktree's line numbers, so `lines` is not required
        // to be in bounds for *this* snapshot -- an unclamped `lines.start`
        // would panic here rather than degrading to a shorter context.
        let len = snapshot.lines.len();
        let start = (lines.start as usize).min(len);
        let end = (lines.end as usize).min(len);
        let before_start = start.saturating_sub(ctx);
        let after_end = (end + ctx).min(len);
        Self {
            file: snapshot.file.clone(),
            kind,
            content_hash: hash_content(&content),
            content,
            context_before: snapshot.lines[before_start..start].to_vec(),
            context_after: snapshot.lines[end..after_end].to_vec(),
            captured_lines: lines,
            auto_selected,
        }
    }

    /// Re-resolve this anchor against a (possibly divergent) snapshot.
    ///
    /// Strategy, in order:
    ///   1. exact content hash at the captured position (cheap, common case)
    ///   2. exact content hash anywhere in the file (block moved)
    ///   3. context match (block edited, neighbours intact)
    ///   4. give up -> Orphaned
    pub fn resolve(&self, snapshot: &Snapshot) -> Resolution {
        if snapshot.file != self.file {
            return Resolution::Orphaned;
        }
        let span = (self.captured_lines.end - self.captured_lines.start).max(1) as usize;

        // 1. same place, same content
        if hash_content(&snapshot.slice(&self.captured_lines)) == self.content_hash {
            return Resolution::Exact {
                lines: self.captured_lines.clone(),
            };
        }

        // 2. same content, moved
        if snapshot.lines.len() >= span {
            for start in 0..=(snapshot.lines.len() - span) {
                let range = start as u32..(start + span) as u32;
                if hash_content(&snapshot.slice(&range)) == self.content_hash {
                    return Resolution::Exact { lines: range };
                }
            }
        }

        // 3. content drifted; locate by surrounding context
        // A hand-selected span demands a stronger match than an auto-expanded one.
        let threshold = if self.auto_selected { 0.4 } else { 0.6 };
        let mut best: Option<(Range<u32>, f32)> = None;
        if snapshot.lines.len() >= span {
            for start in 0..=(snapshot.lines.len() - span) {
                let range = start as u32..(start + span) as u32;
                let score = self.context_score(snapshot, &range);
                if best.as_ref().is_none_or(|(_, b)| score > *b) {
                    best = Some((range, score));
                }
            }
        }
        match best {
            Some((lines, similarity)) if similarity >= threshold => {
                Resolution::Shifted { lines, similarity }
            }
            _ => Resolution::Orphaned,
        }
    }

    /// Fraction of captured context lines still present around `candidate`.
    fn context_score(&self, snapshot: &Snapshot, candidate: &Range<u32>) -> f32 {
        let total = self.context_before.len() + self.context_after.len();
        if total == 0 {
            return 0.0;
        }
        let mut hits = 0usize;
        for (i, line) in self.context_before.iter().rev().enumerate() {
            let idx = candidate.start as isize - 1 - i as isize;
            if idx >= 0 && snapshot.lines.get(idx as usize).map(|l| l.trim()) == Some(line.trim()) {
                hits += 1;
            }
        }
        for (i, line) in self.context_after.iter().enumerate() {
            let idx = candidate.end as usize + i;
            if snapshot.lines.get(idx).map(|l| l.trim()) == Some(line.trim()) {
                hits += 1;
            }
        }
        hits as f32 / total as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &str = "\
fn humanize_model_id(model_id: &str) -> String {
    let mut result = String::with_capacity(model_id.len());
    let mut previous_numeric = false;
    for segment in model_id.split(['-', '_']) {
        if segment.is_empty() {
            continue;
        }
    }
    result
}";

    fn anchor_at(text: &str, lines: Range<u32>, auto: bool) -> (PortableAnchor, Snapshot) {
        let snap = Snapshot::new("crates/delta_common/src/model_catalog.rs", text);
        let anchor = PortableAnchor::capture(&snap, BlockKind::CodeBlock, lines, auto);
        (anchor, snap)
    }

    #[test]
    fn resolves_exactly_in_its_own_snapshot() {
        let (anchor, snap) = anchor_at(ORIGINAL, 1..3, false);
        assert_eq!(anchor.resolve(&snap), Resolution::Exact { lines: 1..3 });
    }

    #[test]
    fn follows_a_block_that_moved() {
        // Another thread prepended a doc comment: same content, new position.
        let shifted = format!("/// Renders a persisted model id.\n/// Title-cased.\n{ORIGINAL}");
        let (anchor, _) = anchor_at(ORIGINAL, 1..3, false);
        let other = Snapshot::new("crates/delta_common/src/model_catalog.rs", &shifted);
        assert_eq!(anchor.resolve(&other), Resolution::Exact { lines: 3..5 });
    }

    #[test]
    fn re_anchors_when_the_other_worktree_edited_the_block() {
        // The receiving thread rewrote the anchored lines but left the
        // surrounding function intact. This is the case that matters for
        // forwarding: the anchor must land, not orphan.
        let diverged = ORIGINAL.replace(
            "    let mut previous_numeric = false;",
            "    let mut previous_numeric = false; // reset per segment",
        );
        let (anchor, _) = anchor_at(ORIGINAL, 1..3, false);
        let other = Snapshot::new("crates/delta_common/src/model_catalog.rs", &diverged);
        match anchor.resolve(&other) {
            Resolution::Shifted { lines, .. } | Resolution::Exact { lines } => {
                assert_eq!(lines, 1..3)
            }
            Resolution::Orphaned => panic!("should have re-anchored via context"),
        }
    }

    #[test]
    fn orphans_rather_than_guessing_when_the_file_is_unrecognisable() {
        let (anchor, _) = anchor_at(ORIGINAL, 1..3, false);
        let rewritten = Snapshot::new(
            "crates/delta_common/src/model_catalog.rs",
            "// file was rewritten from scratch\npub struct Catalog;\nimpl Catalog {}\n",
        );
        assert!(anchor.resolve(&rewritten).is_orphaned());
    }

    #[test]
    fn never_resolves_across_files() {
        let (anchor, _) = anchor_at(ORIGINAL, 1..3, false);
        let elsewhere = Snapshot::new("crates/delta/src/delta.rs", ORIGINAL);
        assert!(anchor.resolve(&elsewhere).is_orphaned());
    }

    #[test]
    fn hashing_ignores_reformatting() {
        assert_eq!(hash_content("let  x =   1;"), hash_content("let x = 1;"));
    }
}
