//! Seed content transcribed from Delta's launch post, so the mock is
//! recognisable against the real screenshots rather than being lorem ipsum.
//!
//! Two threads deliberately touch **the same file** (`model_catalog.rs`) with
//! divergent worktrees. That is not decoration: forwarding a comment between
//! them is the only way to exercise `PortableAnchor::resolve`, and a mock where
//! every anchor lands trivially would demonstrate nothing.

pub type DiffLine = (u32, Option<bool>, &'static str);

#[derive(Clone, Copy, PartialEq)]
pub enum Status {
    Idle,
    Running,
    Conflicting,
}

/// Agent prose, one block per paragraph.
pub struct Block {
    pub text: &'static str,
    pub code: bool,
}

const fn p(text: &'static str) -> Block {
    Block { text, code: false }
}
const fn c(text: &'static str) -> Block {
    Block { text, code: true }
}

// ---------------------------------------------------------------- actors ---

/// Participants. Colours are the four corner swatches from Delta's social card,
/// the same ones the per-participant cursors use in `collab.mp4`.
pub struct SeedActor {
    pub handle: &'static str,
    pub color: u32,
    /// Kept for the agent-initiated forwarding rules. Agents are never named in
    /// the UI and are never mention targets -- you address a thread instead.
    pub agent: bool,
    /// Blame answers "who wrote this". Role answers "whose call is this". They
    /// are different questions, and a design divergence wants the second.
    pub role: &'static str,
}

pub static ACTORS: &[SeedActor] = &[
    SeedActor {
        handle: "danilo-leal",
        color: 0xD4A017,
        agent: false,
        role: "design",
    },
    SeedActor {
        handle: "franciskafyi",
        color: 0x2E6FBF,
        agent: false,
        role: "engineering",
    },
    SeedActor {
        handle: "as-cii",
        color: 0xE33F26,
        agent: false,
        role: "engineering",
    },
    SeedActor {
        handle: "mikayla",
        color: 0x4A9E5C,
        agent: false,
        role: "product",
    },
    SeedActor {
        handle: "delta",
        color: 0xD4D4B0,
        agent: true,
        role: "agent",
    },
];

/// Whoever is driving this window.
pub const ME: usize = 0;

pub const MODEL_CATALOG: &str = "crates/delta_common/src/model_catalog.rs";
pub const COMMENT_SUMMARY: &str = "crates/delta/src/comment_summary.rs";

/// Historical authorship, feeding `OverlapIndex::experts`. In the real thing
/// this is `git blame`; here it is hand-authored, so treat the *ranking* as
/// illustrative and only the *interaction* as evaluated.
///
/// Ranges are real file line numbers, so they stay comparable across worktrees
/// whose snapshots are indexed differently.
///
/// `(actor, file, start, end, commits)`
pub static BLAME: &[(usize, &str, u32, u32, u32)] = &[
    (1, MODEL_CATALOG, 79, 92, 4),
    (2, MODEL_CATALOG, 92, 110, 2),
    (2, MODEL_CATALOG, 77, 80, 1),
    (1, COMMENT_SUMMARY, 12, 26, 3),
];

// --------------------------------------------------------------- threads ---

pub struct SeedThread {
    pub title: &'static str,
    /// What you type after `@`. Thread titles have spaces; slugs do not.
    pub slug: &'static str,
    /// Who is in this thread. You may only address a thread you are in -- the
    /// same rule a Slack channel has.
    pub participants: &'static [usize],
    /// Delta lets you branch a thread to retry an approach. A branch inherits
    /// its parent's transcript, so the two share a coordinate system and prose
    /// anchors can legitimately re-resolve between them -- unlike two unrelated
    /// threads, which must orphan.
    pub branched_from: Option<&'static str>,
    pub status: Status,
    pub unread: bool,
    pub shared: bool,
    pub blocks: &'static [Block],
    pub diff_dir: &'static str,
    pub diff_file: &'static str,
    pub diff_lines: &'static [DiffLine],
    pub added: usize,
    pub removed: usize,
}

/// A thread with no worktree changes yet.
const fn bare(title: &'static str, status: Status, unread: bool, shared: bool) -> SeedThread {
    SeedThread {
        title,
        slug: "",
        participants: &[1],
        branched_from: None,
        status,
        unread,
        shared,
        blocks: &[],
        diff_dir: "",
        diff_file: "",
        diff_lines: &[],
        added: 0,
        removed: 0,
    }
}

pub struct SeedProject {
    pub name: &'static str,
    pub threads: &'static [SeedThread],
}

// -- thread 1: Prevent Unknown Model Labels ---------------------------------

static LABELS_BLOCKS: &[Block] = &[
    p(
        "2. A retired/foreign model key (cases 1 and 4). Point a thread or the workspace default at a key nothing resolves. Realistic ways: open a synced thread that last ran on a provider you don't have configured, or sign out of a provider a thread was using.",
    ),
    p(
        "3. Quickest of all: a temporary code override. Two throwaway lines in ModelSelector::render forcing the key:",
    ),
    c("let model_key = Some(LanguageModelKey::from(\"ollama/llama-3.3-70b\"));"),
    p(
        "right after the existing model_key binding at model_selector.rs:194. Run the app once on main (shows \"Unknown Model\", no icon) and once on this change (shows \"Llama 3.3 70b\").",
    ),
    // The agent keeps working after a comment lands. Without this the last
    // thing on screen is the comment, and it reads like a prompt that ended
    // the turn rather than a note on output the agent already produced.
    p(
        "I went with the code override to keep the repro self-contained. humanize_model_id now lives in model_catalog.rs and is called from the selector, so the fallback is one function rather than a branch at every call site.",
    ),
    c("assert_eq!(humanize_model_id(\"claude-sonnet-4-5\"), \"Claude Sonnet 4.5\");"),
    p(
        "Acronyms come from a small table and consecutive numeric segments join with dots, which is what turns 4-5 into 4.5. Next I will look at the selector's empty state, since an unresolvable key still leaves the icon slot blank.",
    ),
];

static LABELS_DIFF: &[DiffLine] = &[
    (77, None, "}"),
    (78, None, ""),
    (
        79,
        Some(true),
        "/// Renders a persisted model id in display-name form (`claude-sonnet-4-5`",
    ),
    (
        80,
        Some(true),
        "/// becomes \"Claude Sonnet 4.5\") for keys that no registered provider can",
    ),
    (
        81,
        Some(true),
        "/// resolve to a real display name. Alphabetic segments are title-cased",
    ),
    (
        82,
        Some(true),
        "/// (known acronyms upper-cased), and consecutive numeric segments join with",
    ),
    (
        83,
        Some(true),
        "/// dots since ids encode version dots as hyphens.",
    ),
    (
        84,
        Some(true),
        "pub fn humanize_model_id(model_id: &str) -> String {",
    ),
    (
        85,
        Some(true),
        "    let mut result = String::with_capacity(model_id.len());",
    ),
    (86, Some(true), "    let mut previous_numeric = false;"),
    (
        87,
        Some(true),
        "    for segment in model_id.split(['-', '_']) {",
    ),
    (88, Some(true), "        if segment.is_empty() {"),
    (89, Some(true), "            continue;"),
    (90, Some(true), "        }"),
    (
        91,
        Some(true),
        "        let numeric = segment.chars().all(|c| c.is_ascii_digit());",
    ),
    (92, None, "        if !result.is_empty() {"),
    (
        93,
        None,
        "            result.push(if numeric && previous_numeric {",
    ),
    (94, None, "                '.'"),
    (95, None, "            } else {"),
    (96, None, "                ' '"),
    (97, None, "            });"),
    (98, None, "        }"),
    (99, None, "        match segment {"),
    (
        100,
        None,
        "            \"gpt\" => result.push_str(\"GPT\"),",
    ),
    (
        101,
        Some(false),
        "            \"chatgpt\" => result.push_str(\"ChatGPT\"),",
    ),
    (102, None, "            _ => {"),
    (
        103,
        None,
        "                let mut chars = segment.chars();",
    ),
    (104, None, "            }"),
    (105, None, "        }"),
    (106, None, "        previous_numeric = numeric;"),
    (107, None, "    }"),
    (108, None, "    result"),
    (109, None, "}"),
];

// -- thread 1b: a branch of thread 1 ---------------------------------------
//
// Delta branches a thread to retry an approach. The first three blocks are the
// parent's verbatim but shifted down by the branch note, and the last one
// diverges -- so a prose anchor forwarded from the parent resolves `Exact` at a
// new offset, or `Shifted` where the text was rewritten. This is the one place
// cross-thread prose re-anchoring is meaningful, and it works because a branch
// shares its parent's transcript path.

static RETRY_BLOCKS: &[Block] = &[
    p("Branched off the label thread: rather than formatting at the call site, try teaching the registry to resolve the key and fall back once, centrally."),
    p("2. A retired/foreign model key (cases 1 and 4). Point a thread or the workspace default at a key nothing resolves. Realistic ways: open a synced thread that last ran on a provider you don't have configured, or sign out of a provider a thread was using."),
    p("3. Quickest of all: a temporary code override. Two throwaway lines in ModelSelector::render forcing the key:"),
    c("let model_key = Some(LanguageModelKey::from(\"ollama/llama-3.3-70b\"));"),
    p("right after the existing model_key binding at model_selector.rs:194. On this branch I skipped the override entirely and pointed the registry at a stub provider, which exercises the fallback rather than the formatter."),
];

/// What the branch's stated approach looks like as code. Note the file: it
/// shares **no lines and no symbols** with the call-site fix in thread 1. Git
/// merges both without a murmur, and the codebase ends up resolving unknown
/// model keys in two places.
static REGISTRY_DIFF: &[DiffLine] = &[
    (40, None, "impl ModelRegistry {"),
    (41, None, "    pub fn resolve(&self, key: &LanguageModelKey) -> Option<ModelInfo> {"),
    (42, None, "        if let Some(provider) = self.providers.get(key.provider()) {"),
    (43, None, "            return provider.model(key);"),
    (44, None, "        }"),
    (45, Some(true), "        // Nothing registered resolves this key. Rather than letting every"),
    (46, Some(true), "        // call site invent its own label, fall back once, centrally."),
    (47, Some(true), "        Some(ModelInfo::unresolved(key))"),
    (48, Some(false), "        None"),
    (49, None, "    }"),
    (50, None, "}"),
];

// -- thread 2: Fix Compaction Divider Spacing Jitter ------------------------
//
// Same file, diverged worktree. The doc-comment block survives verbatim (so an
// anchor on it resolves `Exact` at a *new* position), while the
// `previous_numeric` line grew a trailing comment (so an anchor covering it
// falls through to context matching and resolves `Shifted`).

static JITTER_BLOCKS: &[Block] = &[
    p(
        "The jitter comes from the divider measuring itself before the compaction summary has laid out, so the first frame uses the pre-collapse height and the second corrects it.",
    ),
    p(
        "Dropping DIVIDER_GAP from 8 to 6 hides it at the default density but not at Comfortable. The real fix is to defer the measure by a frame:",
    ),
    c("cx.on_next_frame(|this, cx| this.remeasure_divider(cx));"),
    p(
        "I also touched humanize_model_id in this worktree while chasing a label that wrapped oddly in the summary — that overlap is why this thread is flagged as conflicting.",
    ),
];

static JITTER_DIFF: &[DiffLine] = &[
    (58, None, "use std::borrow::Cow;"),
    (59, None, ""),
    (
        60,
        None,
        "/// Vertical gap either side of a compaction divider.",
    ),
    (61, Some(false), "const DIVIDER_GAP: f32 = 8.0;"),
    (62, Some(true), "const DIVIDER_GAP: f32 = 6.0;"),
    (63, None, ""),
    (
        64,
        None,
        "/// Renders a persisted model id in display-name form (`claude-sonnet-4-5`",
    ),
    (
        65,
        None,
        "/// becomes \"Claude Sonnet 4.5\") for keys that no registered provider can",
    ),
    (
        66,
        None,
        "/// resolve to a real display name. Alphabetic segments are title-cased",
    ),
    (
        67,
        None,
        "/// (known acronyms upper-cased), and consecutive numeric segments join with",
    ),
    (
        68,
        None,
        "/// dots since ids encode version dots as hyphens.",
    ),
    (
        69,
        None,
        "pub fn humanize_model_id(model_id: &str) -> String {",
    ),
    (
        70,
        None,
        "    let mut result = String::with_capacity(model_id.len());",
    ),
    (
        71,
        Some(true),
        "    let mut previous_numeric = false; // reset per segment",
    ),
    (72, None, "    for segment in model_id.split(['-', '_']) {"),
    (73, None, "        if segment.is_empty() {"),
    (74, None, "            continue;"),
    (75, None, "        }"),
    (
        76,
        Some(true),
        "        let numeric = segment.bytes().all(|b| b.is_ascii_digit());",
    ),
    (77, None, "        if !result.is_empty() {"),
    (
        78,
        None,
        "            result.push(if numeric && previous_numeric {",
    ),
    (79, None, "                '.'"),
    (80, None, "            } else {"),
    (81, None, "                ' '"),
    (82, None, "            });"),
    (83, None, "        }"),
];

// -- thread 3: Investigate Comment Summary Wrapping -------------------------
//
// A different file entirely, so anchors forwarded here have nowhere to land and
// must degrade to `Orphaned` rather than guessing.

static WRAPPING_BLOCKS: &[Block] = &[
    p(
        "The summary wraps mid-handle because it measures the rendered label rather than the grapheme count, so a mention chip counts as one character.",
    ),
    p(
        "Switching to a width-aware truncation fixes it, but it needs the actor colour to survive the ellipsis:",
    ),
    c("let shown = summary.truncate_to(width, Ellipsis::Tail);"),
];

static WRAPPING_DIFF: &[DiffLine] = &[
    (12, None, "impl CommentSummary {"),
    (13, Some(false), "    pub fn line(&self) -> String {"),
    (
        14,
        Some(true),
        "    pub fn line(&self, width: Pixels) -> String {",
    ),
    (15, None, "        let mut out = String::new();"),
    (16, Some(true), "        let budget = width - self.gutter;"),
    (17, None, "        for part in &self.parts {"),
    (18, Some(false), "            out.push_str(&part.text);"),
    (
        19,
        Some(true),
        "            out.push_str(&part.truncate_to(budget));",
    ),
    (20, None, "        }"),
    (21, None, "        out"),
    (22, None, "    }"),
    (23, None, "}"),
];

static KEYBINDING_BLOCKS: &[Block] = &[
    p(
        "The reply affordance only appears on hover today, so there is no way to reach it from the keyboard at all.",
    ),
    p(
        "I am adding a focusable reply target per comment row and binding `r` to it, which means the row needs a real focus handle rather than a plain div:",
    ),
    c("let focus = cx.focus_handle();"),
    p(
        "That lands in comment_summary.rs, where the row is built. I will keep the hover affordance so nothing regresses for mouse users.",
    ),
];

/// Lands in the same file as the wrapping thread. Neither one knows about the
/// other -- which is the whole point of the v2 demo.
static KEYBINDING_DIFF: &[DiffLine] = &[
    (13, None, "    fn render_row(&self, cx: &mut Context<Self>) -> impl IntoElement {"),
    (14, Some(false), "        div()"),
    (15, Some(true), "        div()"),
    (16, Some(true), "            .track_focus(&self.focus)"),
    (17, Some(true), "            .on_action(cx.listener(Self::reply))"),
    (18, None, "            .flex()"),
    (19, None, "            .gap_2()"),
    (20, None, "            .child(self.summary.line(self.width))"),
    (21, None, "    }"),
];

pub static PROJECTS: &[SeedProject] = &[
    SeedProject {
        name: "daniloleal.co",
        threads: &[],
    },
    SeedProject {
        name: "zed.dev",
        threads: &[],
    },
    SeedProject {
        name: "Cloud",
        threads: &[
            bare(
                "GPT Five Point Six Luna vs Nano",
                Status::Idle,
                false,
                false,
            ),
            bare(
                "Understanding organization creation user flow",
                Status::Idle,
                false,
                false,
            ),
        ],
    },
    SeedProject {
        name: "delta",
        threads: &[
            SeedThread {
                title: "Prevent Unknown Model Labels",
                slug: "labels",
                participants: &[0, 1, 3],
                branched_from: None,
                status: Status::Running,
                unread: false,
                shared: true,
                blocks: LABELS_BLOCKS,
                diff_dir: "crates/delta_common/src/",
                diff_file: "model_catalog.rs",
                diff_lines: LABELS_DIFF,
                added: 64,
                removed: 13,
            },
            SeedThread {
                title: "Prevent Unknown Model Labels (registry retry)",
                slug: "registry-retry",
                participants: &[0, 1],
                branched_from: Some("Prevent Unknown Model Labels"),
                status: Status::Running,
                unread: false,
                shared: false,
                blocks: RETRY_BLOCKS,
                diff_dir: "crates/delta_common/src/",
                diff_file: "model_registry.rs",
                diff_lines: REGISTRY_DIFF,
                added: 3,
                removed: 1,
            },
            SeedThread {
                title: "Fix Compaction Divider Spacing Jitter",
                slug: "jitter",
                participants: &[0, 1, 2],
                branched_from: None,
                status: Status::Conflicting,
                unread: false,
                shared: false,
                blocks: JITTER_BLOCKS,
                diff_dir: "crates/delta_common/src/",
                diff_file: "model_catalog.rs",
                diff_lines: JITTER_DIFF,
                added: 3,
                removed: 1,
            },
            SeedThread {
                title: "Investigate Comment Summary Wrapping",
                slug: "wrapping",
                participants: &[0, 2],
                branched_from: None,
                status: Status::Running,
                unread: true,
                shared: false,
                blocks: WRAPPING_BLOCKS,
                diff_dir: "crates/delta/src/",
                diff_file: "comment_summary.rs",
                diff_lines: WRAPPING_DIFF,
                added: 4,
                removed: 3,
            },
            SeedThread {
                title: "Multiplayer Comment Reply Keybinding",
                slug: "reply-keys",
                participants: &[0, 2],
                branched_from: None,
                status: Status::Running,
                unread: true,
                shared: false,
                blocks: KEYBINDING_BLOCKS,
                diff_dir: "crates/delta/src/",
                diff_file: "comment_summary.rs",
                diff_lines: KEYBINDING_DIFF,
                added: 3,
                removed: 1,
            },
            bare(
                "Add Catchup Placeholder User Message",
                Status::Idle,
                false,
                false,
            ),
            bare(
                "Add Loading Feedback To Cloud Sync Popover",
                Status::Idle,
                false,
                false,
            ),
            bare(
                "Reuse Empty Threads On Readdition",
                Status::Idle,
                false,
                false,
            ),
            bare("Explore Running Terminal Icon", Status::Idle, false, false),
            bare(
                "Design First Time Delta Onboarding",
                Status::Idle,
                false,
                false,
            ),
            bare(
                "Commit SHA Link Discoverability",
                Status::Idle,
                false,
                false,
            ),
            bare(
                "Reproducing external changes in shared threads",
                Status::Idle,
                false,
                false,
            ),
            bare("Change Thread Primary Worktree", Status::Idle, false, false),
        ],
    },
    SeedProject {
        name: "delta-meta",
        threads: &[bare("Weekly Standup Thread", Status::Idle, false, false)],
    },
    SeedProject {
        name: "delta-prototype",
        threads: &[],
    },
    SeedProject {
        name: "zed",
        threads: &[],
    },
];

// -------------------------------------------------------------- comments ---

pub enum SeedAnchor {
    Prose(usize),
    /// Inclusive run of diff-line indices.
    Diff(usize, usize),
}

/// Comments already in the threads when the window opens. Without these every
/// comment would be authored by whoever is driving, and "someone mentioned you"
/// would be unreachable.
pub struct SeedComment {
    pub thread: &'static str,
    pub author: usize,
    pub body: &'static str,
    pub anchor: SeedAnchor,
}

pub static COMMENTS: &[SeedComment] = &[
    SeedComment {
        thread: "Prevent Unknown Model Labels",
        author: 1,
        body: "@danilo-leal these five lines restate what the signature already says. Worth dropping before this ships?",
        anchor: SeedAnchor::Prose(0),
    },
    SeedComment {
        thread: "Investigate Comment Summary Wrapping",
        author: 2,
        body: "@danilo-leal this is the width-aware path we sketched. Does subtracting the gutter here look right to you?",
        anchor: SeedAnchor::Diff(2, 2),
    },
    SeedComment {
        thread: "Fix Compaction Divider Spacing Jitter",
        author: 1,
        body: "heads up @danilo-leal, this overlaps the label thread's hunk.",
        anchor: SeedAnchor::Diff(3, 4),
    },
];

pub fn thread_by_title(title: &str) -> Option<&'static SeedThread> {
    PROJECTS
        .iter()
        .flat_map(|p| p.threads.iter())
        .find(|t| t.title == title)
}

/// The thread whose transcript this one is a copy of. Branches share it; every
/// other thread is its own root.
pub fn transcript_root(title: &'static str) -> &'static str {
    let mut current = title;
    for _ in 0..8 {
        match thread_by_title(current).and_then(|t| t.branched_from) {
            Some(parent) => current = parent,
            None => break,
        }
    }
    current
}

// ------------------------------------------------------------ divergence ---

/// Two threads solving one problem differently.
///
/// This is the class of conflict a diff cannot see. The two threads below touch
/// different files entirely -- git merges them cleanly and the product ends up
/// with two answers to the same question. The only place the disagreement is
/// visible is in what each thread *said it was going to do*, before it wrote a
/// line of code.
///
/// A real implementation would compare the two statements with a model. Here
/// they are declared, because what is being prototyped is the interaction, not
/// the classifier.
pub struct SeedDivergence {
    pub subject: &'static str,
    pub a_thread: &'static str,
    /// Block index where this thread stated its approach.
    pub a_block: usize,
    pub a_approach: &'static str,
    pub b_thread: &'static str,
    pub b_block: usize,
    pub b_approach: &'static str,
    /// Who gets to make the call.
    pub owner: usize,
}

pub static DIVERGENCES: &[SeedDivergence] = &[SeedDivergence {
    subject: "where an unresolvable model key gets its display name",
    a_thread: "Prevent Unknown Model Labels",
    a_block: 1,
    a_approach: "format it at the call site, in ModelSelector::render",
    b_thread: "Prevent Unknown Model Labels (registry retry)",
    b_block: 0,
    b_approach: "resolve it in the registry, with one central fallback",
    owner: ME,
}];

impl SeedDivergence {
    /// This thread's side of it, and the other thread's.
    pub fn sides(&self, thread: &str) -> Option<(&'static str, &'static str, &'static str)> {
        if thread == self.a_thread {
            Some((self.a_approach, self.b_approach, self.b_thread))
        } else if thread == self.b_thread {
            Some((self.b_approach, self.a_approach, self.a_thread))
        } else {
            None
        }
    }

    pub fn block_for(&self, thread: &str) -> Option<usize> {
        if thread == self.a_thread {
            Some(self.a_block)
        } else if thread == self.b_thread {
            Some(self.b_block)
        } else {
            None
        }
    }
}

impl SeedThread {
    pub fn has(&self, actor: usize) -> bool {
        self.participants.contains(&actor)
    }
}

/// Threads `actor` may address. Slug-less threads are not addressable.
pub fn addressable_by(actor: usize) -> impl Iterator<Item = &'static SeedThread> {
    PROJECTS
        .iter()
        .flat_map(|p| p.threads.iter())
        .filter(move |t| !t.slug.is_empty() && t.has(actor))
}
