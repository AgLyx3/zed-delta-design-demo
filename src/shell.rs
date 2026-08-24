//! Interactive three-pane shell.
//!
//! The loop this exists to exercise: anchor a block (in the transcript or in
//! the diff) -> type a comment -> it lands in the pending tray -> it ships to
//! the model as a batch on send, unless withheld.
//!
//! On top of that sit the two routing moves the prototype is actually for:
//!
//!   * **mention a person** — `@handle` in a comment, with the expertise index
//!     suggesting who, so the "smart ask" is one click rather than a guess.
//!   * **send the snippet to another thread** — the comment is copied with its
//!     `PortableAnchor` intact and re-resolved against the recipient's
//!     worktree, landing as Exact / Shifted / Orphaned.
//!
//! Both are the same `OverlapIndex` query in different tenses: a past author is
//! expertise, a live thread is conflict.

use gpui::{App, Context, Entity, FocusHandle, Focusable, Window, div, prelude::*, px, rems};
use theme::ActiveTheme;
use ui::DiffStat;
use ui::prelude::*;

use crate::composer::{Composer, ComposerEvent};
use crate::model::actor::ActorId;
use crate::model::anchor::{BlockKind, PortableAnchor, Resolution, Snapshot};
use crate::model::comment::Delivery;
use crate::model::comment::CommentId;
use crate::model::overlap::{ConflictKind, HistoricalTouch, LiveTouch, OverlapIndex};
use crate::model::thread::ThreadId;
use crate::model::transport::{Envelope, Receipt, SendError, ThreadTransport};
use crate::seed::{self, Status};

/// What the composer is currently pointed at, in the *active* thread.
#[derive(Clone, Copy, PartialEq)]
pub enum Target {
    /// Index into the thread's blocks.
    Prose(usize),
    /// Inclusive run of the thread's diff-line indices.
    Diff(usize, usize),
    /// Replying to a comment. Same box, no new anchor -- it inherits the one
    /// the comment it answers already has.
    Reply(u32),
}

/// Someone answering a comment. Not a different object, just a comment that
/// belongs to another one.
pub struct CommentReply {
    pub handle: String,
    pub author: usize,
    pub body: String,
    pub mentions: Vec<usize>,
}

/// Where a comment came from. Handoff is direct-injection per option (A), so
/// provenance is load-bearing rather than decorative: it is the only thing
/// distinguishing a teammate's deliberate note from another thread's spillover.
#[derive(Clone, Copy)]
pub enum Provenance {
    Local,
    Forwarded {
        from_title: &'static str,
        forwarded_by: usize,
        original_author: usize,
    },
}

pub struct UiComment {
    pub id: u32,
    /// Something a person can say out loud: `labels-3`. The integer id is for
    /// element keys; this is what you use to refer to a comment.
    pub handle: String,
    /// The real portable anchor. Carrying this rather than a line number is
    /// what lets a comment survive being forwarded to another thread.
    pub anchor: PortableAnchor,
    /// Where the anchor lands *in the thread holding this comment*. Recomputed
    /// on arrival; never inherited from the sender.
    pub resolved: Resolution,
    pub body: String,
    pub author: usize,
    pub mentions: Vec<usize>,
    pub origin: Provenance,
    pub delivery: Delivery,
    /// Set when this comment is the agent flagging a design divergence rather
    /// than a person leaving a note. Indexes `seed::DIVERGENCES`.
    pub divergence: Option<usize>,
    /// The file a workspace finding is about. Its identity -- dedup, dismissal
    /// and board status all key on this rather than on the body text, which
    /// names the other threads and therefore changes when they do.
    pub about: Option<String>,
    /// Answers to this comment, in order.
    pub replies: Vec<CommentReply>,
}

/// What an `@` can address.
///
/// Two kinds, doing different jobs. A person is addressed *here* -- it puts
/// their name on a note in this thread. A thread is a reference to work
/// happening elsewhere; it links, it does not deliver. Sending something to
/// another thread is forwarding, and that stays a separate, deliberate act.
#[derive(Clone, Copy, PartialEq)]
pub enum MentionTarget {
    Actor(usize),
    Thread(usize),
}

/// An agent response addressed to one specific comment.
pub struct AgentReply {
    pub in_reply_to: u32,
    pub quote: String,
    pub quote_author: usize,
    pub body: String,
    pub turn: u32,
}

/// What this thread sent somewhere else, and where it went.
///
/// Kept on the *sending* side. Without it a forward vanishes the moment it is
/// sent: the sender is left believing it warned someone, with nothing on screen
/// either way. Status is read live from the recipient rather than snapshotted,
/// so it tracks what they actually did with it.
struct SentRecord {
    to: usize,
    comment: u32,
    excerpt: String,
}

struct ThreadUi {
    seed: &'static seed::SeedThread,
    project: &'static str,
    comments: Vec<UiComment>,
    replies: Vec<AgentReply>,
    sent: Vec<SentRecord>,
    turn: u32,
    unread: bool,
    /// The agent is mid-turn. Anything arriving now waits for the boundary
    /// rather than being spliced into a prompt that is already in flight.
    agent_busy: bool,
}

impl ThreadUi {
    fn diff_path(&self) -> String {
        format!("{}{}", self.seed.diff_dir, self.seed.diff_file)
    }

    /// The diff pane's text, as a snapshot anchors can resolve against.
    fn diff_snapshot(&self) -> Snapshot {
        let text = self
            .seed
            .diff_lines
            .iter()
            .map(|(_, _, t)| *t)
            .collect::<Vec<_>>()
            .join("\n");
        Snapshot::new(self.diff_path(), &text)
    }

    fn prose_path(&self) -> String {
        // A branch inherits its parent's transcript, so the pair shares a
        // coordinate system and prose anchors can cross between them. Unrelated
        // threads get distinct paths and are forced to orphan.
        format!(
            "thread://{}/transcript",
            seed::transcript_root(self.seed.title)
        )
    }

    fn prose_snapshot(&self) -> Snapshot {
        let text = self
            .seed
            .blocks
            .iter()
            .map(|b| b.text)
            .collect::<Vec<_>>()
            .join("\n");
        Snapshot::new(self.prose_path(), &text)
    }

    fn has_diff(&self) -> bool {
        !self.seed.diff_lines.is_empty()
    }

    /// File line numbers this worktree is dirty across.
    fn dirty_lines(&self) -> Option<std::ops::Range<u32>> {
        let first = self.seed.diff_lines.first()?.0;
        let last = self.seed.diff_lines.last()?.0;
        Some(first..last + 1)
    }
}

/// What state a comment should be in when it lands in `to`.
///
/// The *recipient's* situation decides this, never the sender's judgement.
/// Someone noting a bug in passing should not have to know whether the other
/// thread's agent is busy, and should not be blamed for interrupting it.
fn arrival_delivery(by: usize, recipient_busy: bool) -> Delivery {
    if seed::ACTORS[by].agent {
        // Agents will eventually initiate these themselves. An agent's forward
        // always waits for a human to release it, otherwise two agents can
        // drive each other round a loop with nobody in it.
        return Delivery::Queued;
    }
    if recipient_busy {
        Delivery::Queued
    } else {
        Delivery::Pending
    }
}

/// The other half of the agent-initiated case: a forward must not travel back
/// to the thread the comment came from. That is the A -> B -> A loop.
fn forward_allowed(by: usize, origin: Option<&str>, to_title: &str) -> bool {
    !(seed::ACTORS[by].agent && origin == Some(to_title))
}

/// A thread's short name. Hand-written where it matters, derived elsewhere so
/// every thread has one.
fn thread_slug(thread: &'static seed::SeedThread) -> String {
    if !thread.slug.is_empty() {
        return thread.slug.to_string();
    }
    thread
        .title
        .to_lowercase()
        .split_whitespace()
        .take(2)
        .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// `labels-3` -- the thread it lives in, and which comment it is there.
fn comment_handle(thread: &ThreadUi) -> String {
    format!("{}-{}", thread_slug(thread.seed), thread.comments.len() + 1)
}

/// Which live threads have uncommitted changes in the same file.
///
/// Set maths over metadata: no model call, no diff read. Idle threads are
/// skipped because a thread that is not running cannot surprise anyone, and a
/// file only reports if more than one thread is in it.
fn observe(threads: &[ThreadUi]) -> Vec<Observation> {
    let mut out: Vec<Observation> = Vec::new();
    for (i, thread) in threads.iter().enumerate() {
        if !thread.agent_busy || !thread.has_diff() {
            continue;
        }
        let file = thread.diff_path();
        match out.iter_mut().find(|o| o.file == file) {
            Some(existing) => existing.threads.push(i),
            None => out.push(Observation { file, threads: vec![i] }),
        }
    }
    out.retain(|o| o.threads.len() > 1);
    out
}

fn resolve_in(thread: &ThreadUi, anchor: &PortableAnchor) -> Resolution {
    let in_diff = anchor.resolve(&thread.diff_snapshot());
    if !in_diff.is_orphaned() {
        return in_diff;
    }
    anchor.resolve(&thread.prose_snapshot())
}

/// One beat of the scripted demo.
///
/// The app drives itself rather than being driven by synthetic keystrokes: a
/// composing input source turns every fake keypress into whatever it feels
/// like, and stealing focus mid-recording is rude besides. This types into the
/// composer directly, so what is recorded is the app, not the robot.
#[derive(Clone, Copy)]
enum Beat {
    Thread(&'static str),
    Mode(Proactive),
    Channel,
    /// Approve whatever the orchestrator raised in the current thread.
    ApproveProposal,
    Select(Target),
    Key(char),
    Commit,
    OpenForward,
    ForwardTo(&'static str),
    Pause,
}

pub struct Shell {
    focus: FocusHandle,
    threads: Vec<ThreadUi>,
    active: usize,
    target: Option<Target>,
    composer: Entity<Composer>,
    /// Highlighted row in the `@` picker.
    mention_sel: usize,
    /// Escape closes the picker without cancelling the comment.
    picker_dismissed: bool,
    /// Which "send to thread" list is open. At most one, which is what keeps
    /// the row element ids unique per frame.
    forwarding: Forwarding,
    /// Whether the middle pane shows a thread or the mention inbox.
    view: View,
    /// Proposals that were turned down, so the sweep never re-raises them.
    dismissed: Vec<(usize, String)>,
    /// Auto releases already scheduled, so repeated sweeps do not stack timers.
    auto_scheduled: Vec<(usize, String)>,
    /// Scripted demo, if `DELTA_MOCK_DEMO` is set.
    demo: Vec<(u64, Beat)>,
    /// V1 is people talking to each other. V2 is the workspace noticing things
    /// on its own. Off by default so the human loop can be shown on its own,
    /// and flipped on to show where it is heading.
    proactive: Proactive,
    next_id: u32,
}

/// What a pending forward is carrying.
#[derive(Clone, Copy, PartialEq)]
enum Forwarding {
    Idle,
    /// A committed comment, note and all.
    Comment(u32),
    /// The current selection on its own -- the code, with nothing said about it.
    Selection,
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Thread,
    /// Every comment across every thread that names you.
    Mentions,
    /// Workspace-wide observations. Above the threads, visible to everyone,
    /// read-mostly -- an announcement board rather than somebody's inbox.
    Channel,
}

/// Something the workspace noticed across threads.
///
/// Computed rather than stored: intersecting the file sets of live threads is
/// set maths on metadata that already exists, so it can run at every turn
/// boundary without costing anything. It is deliberately shallow -- it knows
/// *that* two threads have changes in one file, never what those changes are.
/// How much the workspace is allowed to do on its own.
///
/// The gate is not inherently a human one -- it is a mode, the way a permission
/// setting is. `Manual` means a person decides whether their agent sees a
/// finding. `Auto` means a model makes that call and only holds the ones it is
/// unsure about.
#[derive(Clone, Copy, PartialEq)]
enum Proactive {
    Off,
    Manual,
    Auto,
}

impl Proactive {
    fn on(self) -> bool {
        self != Proactive::Off
    }

    fn next(self) -> Self {
        match self {
            Proactive::Off => Proactive::Manual,
            Proactive::Manual => Proactive::Auto,
            Proactive::Auto => Proactive::Off,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Proactive::Off => "proactive: off",
            Proactive::Manual => "proactive: manual",
            Proactive::Auto => "proactive: auto",
        }
    }
}

pub struct Observation {
    pub file: String,
    pub threads: Vec<usize>,
}

impl Shell {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut threads = Vec::new();
        for project in seed::PROJECTS {
            for thread in project.threads {
                threads.push(ThreadUi {
                    seed: thread,
                    project: project.name,
                    comments: Vec::new(),
                    replies: Vec::new(),
                    sent: Vec::new(),
                    turn: 1,
                    unread: thread.unread,
                    agent_busy: thread.status == Status::Running,
                });
            }
        }
        let active = threads
            .iter()
            .position(|t| t.seed.title == "Prevent Unknown Model Labels")
            .unwrap_or(0);
        // Comments that were already there. They arrive by exactly the same
        // rule as a forward, so there is no special case to explain.
        let mut next_id = 0u32;
        for seeded in seed::COMMENTS {
            let Some(index) = threads.iter().position(|t| t.seed.title == seeded.thread) else {
                continue;
            };
            let anchor = match seeded.anchor {
                seed::SeedAnchor::Prose(i) => PortableAnchor::capture(
                    &threads[index].prose_snapshot(),
                    BlockKind::Paragraph,
                    i as u32..(i as u32 + 1),
                    false,
                ),
                seed::SeedAnchor::Diff(a, b) => PortableAnchor::capture(
                    &threads[index].diff_snapshot(),
                    BlockKind::DiffHunk,
                    a as u32..(b as u32 + 1),
                    false,
                ),
            };
            let resolved = resolve_in(&threads[index], &anchor);
            let handle = comment_handle(&threads[index]);
            next_id += 1;
            threads[index].comments.push(UiComment {
                id: next_id,
                handle,
                anchor,
                resolved,
                body: seeded.body.to_string(),
                author: seeded.author,
                mentions: parse_mentions(seeded.body),
                origin: Provenance::Local,
                delivery: Delivery::Pending,
                divergence: None,
            about: None,
            replies: Vec::new(),
            });
        }

        // Other people's agents are mid-flight. They finish on their own, on a
        // stagger, so a forward sent into one can be watched queueing and then
        // releasing without anybody pressing anything.
        for (index, thread) in threads.iter().enumerate() {
            if thread.agent_busy {
                // Long enough to actually show someone. These stand in for
                // other people's agents working; at 30s they were finishing
                // before you could open the workspace channel, and the sweep
                // would correctly report nothing.
                let millis = 240_000 + (index as u64 % 3) * 20_000;
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(millis))
                        .await;
                    this.update(cx, |this, cx| this.finish_turn(index, Vec::new(), cx))
                        .ok();
                })
                .detach();
            }
        }

        // The comparison the workspace can do that no single agent can: two
        // live threads declared different approaches to one problem. The agent
        // says so in its own thread, using the same card a teammate would.
        let agent = seed::ACTORS.iter().position(|a| a.agent).unwrap_or(0);
        for (d, divergence) in seed::DIVERGENCES.iter().enumerate() {
            for title in [divergence.a_thread, divergence.b_thread] {
                let Some(index) = threads.iter().position(|t| t.seed.title == title) else {
                    continue;
                };
                let Some((mine, theirs, other)) = divergence.sides(title) else {
                    continue;
                };
                let Some(block) = divergence.block_for(title) else {
                    continue;
                };
                let anchor = PortableAnchor::capture(
                    &threads[index].prose_snapshot(),
                    BlockKind::Paragraph,
                    block as u32..(block as u32 + 1),
                    false,
                );
                let resolved = resolve_in(&threads[index], &anchor);
                let handle = comment_handle(&threads[index]);
                let body = format!(
                    "@{} both threads are deciding {}. Here we {}; \"{}\" will {}.                      Nothing overlaps, so this merges cleanly and ships twice.",
                    seed::ACTORS[divergence.owner].handle,
                    divergence.subject,
                    mine,
                    other,
                    theirs
                );
                next_id += 1;
                threads[index].comments.push(UiComment {
                    id: next_id,
                    handle,
                    anchor,
                    resolved,
                    mentions: parse_mentions(&body),
                    body,
                    // Nominally the thread's agent, but never rendered as a
                    // name -- the `design divergence` badge is the attribution.
                    author: agent,
                    origin: Provenance::Local,
                    // A flag is not a note to the model. The *verdict* is.
                    delivery: Delivery::Withheld,
                    divergence: Some(d),
                    about: None,
                    replies: Vec::new(),
                });
            }
        }

        let composer = cx.new(|cx| {
            Composer::new("Type to comment, @ to mention, Enter to send, Esc to cancel", cx)
        });
        cx.subscribe(&composer, Self::on_composer_event).detach();


        Self {
            focus: cx.focus_handle(),
            threads,
            active,
            target: None,
            composer,
            mention_sel: 0,
            picker_dismissed: false,
            forwarding: Forwarding::Idle,
            view: View::Thread,
            dismissed: Vec::new(),
            auto_scheduled: Vec::new(),
            demo: Vec::new(),
            proactive: Proactive::Off,
            next_id,
        }
    }

    fn active(&self) -> &ThreadUi {
        &self.threads[self.active]
    }

    // ------------------------------------------------------------ overlap --

    /// One index serves both questions. Live threads answer "will this
    /// conflict?"; historical authorship answers "who should I ask?".
    fn overlap(&self) -> OverlapIndex {
        let mut index = OverlapIndex::new(6);
        for (i, thread) in self.threads.iter().enumerate() {
            if let Some(lines) = thread.dirty_lines() {
                index.live.push(LiveTouch {
                    thread: crate::model::thread::ThreadId(i as u32),
                    file: thread.diff_path(),
                    lines,
                    turn: thread.turn,
                });
            }
        }
        for (actor, file, start, end, commits) in seed::BLAME {
            index.history.push(HistoricalTouch {
                author: ActorId(*actor as u32),
                file: file.to_string(),
                lines: *start..*end,
                commits: *commits,
            });
        }
        index
    }

    /// File line numbers the current target covers, if it is in the diff.
    fn target_file_lines(&self) -> Option<(String, std::ops::Range<u32>)> {
        let Some(Target::Diff(a, b)) = self.target else {
            return None;
        };
        let thread = self.active();
        let lines = thread.seed.diff_lines;
        Some((thread.diff_path(), lines.get(a)?.0..lines.get(b)?.0 + 1))
    }

    // ------------------------------------------------------------ editing --

    fn composer_text(&self, cx: &App) -> String {
        self.composer.read(cx).text().to_string()
    }

    fn set_composer(&mut self, text: String, cx: &mut Context<Self>) {
        self.composer.update(cx, |c, cx| c.set_text(text, cx));
    }

    fn begin(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        self.target = Some(target);
        self.composer.update(cx, |c, cx| c.clear(cx));
        self.mention_sel = 0;
        self.picker_dismissed = false;
        self.forwarding = Forwarding::Idle;
        // Focus the field itself, so the platform's text system -- and with it
        // IME, paste and selection -- is pointed at what you type into.
        let handle = self.composer.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        let text = self.composer_text(cx);
        let (Some(target), false) = (self.target, text.trim().is_empty()) else {
            return;
        };
        let body = text.trim().to_string();
        let mentions = parse_mentions(&body);

        // Answering a comment needs no anchor of its own.
        if let Target::Reply(parent) = target {
            let thread = &mut self.threads[self.active];
            if let Some(comment) = thread.comments.iter_mut().find(|c| c.id == parent) {
                let handle = format!("{}.{}", comment.handle, comment.replies.len() + 1);
                comment.replies.push(CommentReply {
                    handle,
                    author: seed::ME,
                    body,
                    mentions,
                });
            }
            self.composer.update(cx, |c, cx| c.clear(cx));
            self.target = None;
            cx.notify();
            return;
        }

        let thread = self.active();
        // Capture a genuine anchor so the comment is portable from birth.
        let anchor = match target {
            Target::Prose(i) => PortableAnchor::capture(
                &thread.prose_snapshot(),
                BlockKind::Paragraph,
                i as u32..(i as u32 + 1),
                true,
            ),
            Target::Diff(a, b) => PortableAnchor::capture(
                &thread.diff_snapshot(),
                BlockKind::DiffHunk,
                a as u32..(b as u32 + 1),
                true,
            ),
            Target::Reply(_) => unreachable!("handled above"),
        };
        let resolved = resolve_in(thread, &anchor);
        let handle = comment_handle(thread);
        self.next_id += 1;
        let comment = UiComment {
            id: self.next_id,
            handle,
            anchor,
            resolved,
            body,
            author: seed::ME,
            mentions,
            origin: Provenance::Local,
            delivery: Delivery::Pending,
            divergence: None,
            about: None,
            replies: Vec::new(),
        };
        self.threads[self.active].comments.push(comment);
        self.composer.update(cx, |c, cx| c.clear(cx));
        self.target = None;
        self.picker_dismissed = false;
        // Enter sends. There is no second step -- the comment is in the thread
        // and the agent picks it up, exactly as the placeholder says.
        if !self.active().agent_busy {
            self.send_batch(cx);
        }
        cx.notify();
    }

    /// Copy a comment into another thread, per option (A): it lands directly,
    /// already pending, with no accept gate. The anchor travels verbatim and is
    /// re-resolved here rather than translated by the sender, who has no view
    /// of the recipient's worktree.
    /// Build the envelope for whatever is being handed over, and post it. The
    /// two callers below differ only in what they put in the body.
    fn post(&mut self, envelope: Envelope, cx: &mut Context<Self>) {
        match self.send(envelope) {
            Ok(_receipt) => {
                self.forwarding = Forwarding::Idle;
                cx.notify();
            }
            Err(_refused) => {
                // Nothing partial happened; the transport either took it or did
                // not. Close the picker so the refusal is at least visible.
                self.forwarding = Forwarding::Idle;
                cx.notify();
            }
        }
    }

    fn forward_to(&mut self, what: Forwarding, to: usize, cx: &mut Context<Self>) {
        match what {
            Forwarding::Comment(id) => self.forward_comment(id, to, cx),
            Forwarding::Selection => self.forward_selection(to, cx),
            Forwarding::Idle => {}
        }
    }

    /// Hand the highlighted span to another thread, carrying whatever has been
    /// typed about it. An empty note is fine -- then the snippet is the message.
    fn forward_selection(&mut self, to: usize, cx: &mut Context<Self>) {
        let (Some(target), true) = (self.target, to != self.active) else {
            return;
        };
        let thread = self.active();
        let anchor = match target {
            Target::Prose(i) => PortableAnchor::capture(
                &thread.prose_snapshot(),
                BlockKind::Paragraph,
                i as u32..(i as u32 + 1),
                true,
            ),
            Target::Diff(a, b) => PortableAnchor::capture(
                &thread.diff_snapshot(),
                BlockKind::DiffHunk,
                a as u32..(b as u32 + 1),
                true,
            ),
            Target::Reply(_) => return,
        };
        let from_title = thread.seed.title.to_string();
        let note = self.composer_text(cx).trim().to_string();
        let mentions = parse_mentions(&note);
        let envelope = Envelope {
            from: ThreadId(self.active as u32),
            to: ThreadId(to as u32),
            sender: ActorId(seed::ME as u32),
            author: ActorId(seed::ME as u32),
            anchor,
            body: note,
            mentions: mentions.iter().map(|m| ActorId(*m as u32)).collect(),
            from_title,
        };
        self.target = None;
        self.composer.update(cx, |c, cx| c.clear(cx));
        self.post(envelope, cx);
    }

    fn forward_comment(&mut self, comment_id: u32, to: usize, cx: &mut Context<Self>) {
        if to == self.active {
            return;
        }
        let Some(source) = self.threads.get(self.active) else {
            return;
        };
        let Some(comment) = source.comments.iter().find(|c| c.id == comment_id) else {
            return;
        };
        let anchor = comment.anchor.clone();
        let body = comment.body.clone();
        let mentions = comment.mentions.clone();
        let original_author = comment.author;
        let from_title = source.seed.title;

        let envelope = Envelope {
            from: ThreadId(self.active as u32),
            to: ThreadId(to as u32),
            sender: ActorId(seed::ME as u32),
            author: ActorId(original_author as u32),
            anchor,
            body,
            mentions: mentions.iter().map(|m| ActorId(*m as u32)).collect(),
            from_title: from_title.to_string(),
        };
        self.post(envelope, cx);
    }

    fn send_batch(&mut self, cx: &mut Context<Self>) {
        let turn = self.threads[self.active].turn;
        let file = self.active().seed.diff_file;
        let diff_path = self.active().diff_path();
        let mut produced = Vec::new();
        let thread = &mut self.threads[self.active];
        for comment in &mut thread.comments {
            if comment.delivery == Delivery::Pending {
                comment.delivery = Delivery::Delivered { turn };
                let where_ = describe_where(comment, &diff_path, file, thread.seed.diff_lines);
                produced.push(AgentReply {
                    in_reply_to: comment.id,
                    quote: comment.body.clone(),
                    quote_author: comment.author,
                    body: synth_reply(&comment.body, &where_),
                    turn,
                });
            }
        }
        thread.turn += 1;
        // Hand it to the agent rather than answering on its behalf; the replies
        // appear when the turn ends, which is also when the queue releases.
        let active = self.active;
        self.start_turn(active, produced, 2_600, cx);
    }

    /// Put a thread's agent to work, and have it finish on its own.
    ///
    /// The replies are canned, but the *timing* is not: the turn occupies real
    /// wall-clock, so queueing is something you watch happen rather than
    /// something the UI asserts.
    fn start_turn(
        &mut self,
        thread_index: usize,
        replies: Vec<AgentReply>,
        millis: u64,
        cx: &mut Context<Self>,
    ) {
        self.threads[thread_index].agent_busy = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(millis))
                .await;
            this.update(cx, |this, cx| this.finish_turn(thread_index, replies, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    /// The turn boundary. Replies land, and everything that arrived mid-turn
    /// arms itself -- nobody had to decide that.
    fn finish_turn(
        &mut self,
        thread_index: usize,
        replies: Vec<AgentReply>,
        cx: &mut Context<Self>,
    ) {
        let active = self.active;
        let Some(thread) = self.threads.get_mut(thread_index) else {
            return;
        };
        thread.agent_busy = false;
        let had_replies = !replies.is_empty();
        thread.replies.extend(replies);
        for comment in &mut thread.comments {
            // `Proposed` is deliberately untouched: a turn ending is not
            // somebody agreeing to it.
            if comment.delivery == Delivery::Queued {
                comment.delivery = Delivery::Pending;
            }
        }
        if had_replies && thread_index != active {
            thread.unread = true;
        }
        // A turn just ended, so there is new work to compare. This is the cheap
        // tier: set maths over live threads, no model call.
        self.sweep(cx);
        cx.notify();
    }

    fn toggle_delivery(&mut self, id: u32, cx: &mut Context<Self>) {
        let busy = self.threads[self.active].agent_busy;
        if let Some(c) = self.threads[self.active]
            .comments
            .iter_mut()
            .find(|c| c.id == id)
        {
            c.delivery = match c.delivery {
                Delivery::Pending | Delivery::Queued => Delivery::Withheld,
                // Re-arming puts it back on the queue if the agent is still
                // working, rather than jumping the boundary.
                Delivery::Withheld if busy => Delivery::Queued,
                Delivery::Withheld => Delivery::Pending,
                other => other,
            };
            cx.notify();
        }
    }

    // ------------------------------------------------------------ mentions --

    /// The `@token` being typed, if the caret is inside one.
    /// People, and threads you are in. Agents are never listed -- to reach the
    /// agent in another thread you address the thread.
    fn mention_candidates(&self, cx: &App) -> Vec<MentionTarget> {
        let text = self.composer_text(cx);
        let Some(query) = Self::mention_query(&text) else {
            return Vec::new();
        };
        let query = query.to_lowercase();
        let people = seed::ACTORS
            .iter()
            .enumerate()
            .filter(|(_, a)| !a.agent)
            .filter(|(_, a)| a.handle.to_lowercase().starts_with(&query))
            .map(|(i, _)| MentionTarget::Actor(i));
        let threads = self
            .threads
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.seed.slug.is_empty() && t.seed.has(seed::ME))
            .filter(|(_, t)| t.seed.slug.to_lowercase().starts_with(&query))
            .map(|(i, _)| MentionTarget::Thread(i));
        people.chain(threads).collect()
    }

    fn picker_open(&self, cx: &App) -> bool {
        !self.picker_dismissed && !self.mention_candidates(cx).is_empty()
    }

    fn complete_mention(&mut self, target: MentionTarget, cx: &mut Context<Self>) {
        let handle = match target {
            MentionTarget::Actor(i) => seed::ACTORS[i].handle,
            MentionTarget::Thread(i) => self.threads[i].seed.slug,
        };
        let mut text = self.composer_text(cx);
        if let Some(at) = text.rfind('@') {
            text.truncate(at);
        }
        text.push('@');
        text.push_str(handle);
        text.push(' ');
        self.set_composer(text, cx);
        self.mention_sel = 0;
        self.picker_dismissed = false;
    }

    /// The field reports what happened; the shell decides what it means.
    fn on_composer_event(
        &mut self,
        _composer: Entity<Composer>,
        event: &ComposerEvent,
        cx: &mut Context<Self>,
    ) {
        let candidates = self.mention_candidates(cx);
        let picker = !self.picker_dismissed && !candidates.is_empty();
        match event {
            ComposerEvent::Changed => {
                self.picker_dismissed = false;
                self.mention_sel = 0;
                cx.notify();
            }
            ComposerEvent::Submit | ComposerEvent::Complete if picker => {
                let pick = candidates[self.mention_sel.min(candidates.len() - 1)];
                self.complete_mention(pick, cx);
                cx.notify();
            }
            ComposerEvent::Submit => self.commit(cx),
            ComposerEvent::Complete => {}
            ComposerEvent::Cancel if picker => {
                self.picker_dismissed = true;
                cx.notify();
            }
            ComposerEvent::Cancel => {
                self.target = None;
                self.composer.update(cx, |c, cx| c.clear(cx));
                cx.notify();
            }
            ComposerEvent::MoveUp if picker => {
                self.mention_sel = self.mention_sel.saturating_sub(1);
                cx.notify();
            }
            ComposerEvent::MoveDown if picker => {
                self.mention_sel = (self.mention_sel + 1).min(candidates.len() - 1);
                cx.notify();
            }
            ComposerEvent::MoveUp | ComposerEvent::MoveDown => {}
        }
    }

/// The `@token` under the caret, if there is one.
fn mention_query(text: &str) -> Option<&str> {
    let at = text.rfind('@')?;
    let rest = &text[at + 1..];
    rest.chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        .then_some(rest)
}


    /// Comment body with mentions rendered as chips: a person in their cursor
    /// colour, a thread with its live status dot. Thread chips navigate --
    /// a thread mention is a reference, so the only thing it does is take you
    /// there.
    fn body_el(&self, body: &str, key: &str, cx: &mut Context<Self>) -> gpui::Div {
        let mut row = div().flex().flex_wrap().items_center().gap_1();
        for (n, token) in body.split_whitespace().enumerate() {
            let word = mention_word(token);
            let actor = word.and_then(|w| seed::ACTORS.iter().position(|a| a.handle == w));
            let thread = word.and_then(|w| {
                self.threads
                    .iter()
                    .position(|t| !t.seed.slug.is_empty() && t.seed.slug == w)
            });
            row = match (actor, thread) {
                (Some(i), _) => row.child(
                    div()
                        .px_1()
                        .rounded_sm()
                        .bg(gpui::rgba((seed::ACTORS[i].color << 8) | 0x33))
                        .text_size(px(12.))
                        .text_color(gpui::rgb(seed::ACTORS[i].color))
                        .child(token.to_string()),
                ),
                (_, Some(i)) => {
                    let busy = self.threads[i].agent_busy;
                    row.child(
                        div()
                            .id(gpui::SharedString::from(format!("tm-{key}-{n}")))
                            .flex()
                            .items_center()
                            .gap_1()
                            .px_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .bg(gpui::rgba(0x8B7FD433))
                            .hover(|el| el.bg(gpui::rgba(0x8B7FD455)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.active = i;
                                this.view = View::Thread;
                                this.target = None;
                                this.forwarding = Forwarding::Idle;
                                this.threads[i].unread = false;
                                cx.notify();
                            }))
                            .child(dot_el(if busy { 0x4A9E5C } else { 0x555555 }))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(gpui::rgb(0xA79BE0))
                                    .child(token.to_string()),
                            ),
                    )
                }
                _ => row.child(div().text_size(px(12.)).child(token.to_string())),
            };
        }
        row
    }

    /// Every comment anywhere that names you. A mention is only routing if it
    /// is reachable from outside the thread it was written in.
    /// What this thread sent elsewhere, and whether it landed in front of an
    /// agent. Read live, so it reflects what the recipient did rather than what
    /// was true at the moment of sending.
    fn receipts(&self, cx: &mut Context<Self>) -> gpui::Div {
        let colors = cx.theme().colors().clone();
        let thread = self.active();
        let mut list = div()
            .flex()
            .flex_col()
            .gap_1()
            .pt_2()
            .border_t_1()
            .border_color(colors.border_variant)
            .child(
                Label::new("Sent from this thread")
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            );

        for (n, record) in thread.sent.iter().enumerate() {
            let recipient = &self.threads[record.to];
            let landed = recipient.comments.iter().find(|c| c.id == record.comment);
            let (status, colour) = match landed {
                None => ("withdrawn there", 0x777777),
                Some(c) if matches!(c.delivery, Delivery::Proposed | Delivery::Queued) => {
                    ("agent not told yet", 0xE0A030)
                }
                Some(_) => ("agent has it", 0x4A9E5C),
            };
            let target = record.to;
            list = list.child(
                div()
                    .id(("receipt", n))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|el| el.bg(colors.element_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active = target;
                        this.view = View::Thread;
                        this.threads[target].unread = false;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(gpui::rgb(0xA79BE0))
                            .child(format!("@{}", thread_slug(recipient.seed))),
                    )
                    .child(
                        div().flex_1().min_w(px(0.)).child(
                            Label::new(record.excerpt.clone())
                                .size(LabelSize::XSmall)
                                .color(Color::Muted)
                                .truncate(),
                        ),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(gpui::rgb(colour))
                            .child(status.to_string()),
                    ),
            );
        }
        list
    }

    /// The cheap sweep: which live threads have changes in the same file.
    ///
    /// No model call and no diff reading -- idle threads are skipped, because
    /// a thread that is not running cannot surprise anyone.
    fn observations(&self) -> Vec<Observation> {
        observe(&self.threads)
    }

    fn proposal_body(&self, file: &str, to: usize, others: &[usize]) -> String {
        let named: Vec<&str> = others
            .iter()
            .filter(|i| **i != to)
            .map(|i| self.threads[*i].seed.slug)
            .filter(|s| !s.is_empty())
            .collect();
        let short = file.rsplit('/').next().unwrap_or(file);
        format!(
            "another thread has uncommitted changes in {short}: @{}. worth a look before either lands.",
            named.join(", @")
        )
    }

    /// The orchestrator's pass. Detection cannot live inside a thread -- no
    /// thread can see another's worktree -- so this runs above them and pushes
    /// into every thread involved. Nobody has to go looking for it.
    fn sweep(&mut self, cx: &mut Context<Self>) {
        if !self.proactive.on() {
            return;
        }
        for observation in self.observations() {
            for target in observation.threads.clone() {
                self.propose_into(observation.file.clone(), target, observation.threads.clone(), cx);
            }
        }
        // Switching to auto hands the judgement over, including on things
        // already being held. It is not instant: something has to weigh each
        // one, so the release lands a beat later rather than the moment the
        // mode changes.
        if self.proactive == Proactive::Auto {
            // Staggered, not all at once. Each one lands when that thread's
            // agent next comes up for air, which is not the same moment for
            // everybody.
            let mut n = 0u64;
            for observation in self.observations() {
                for target in observation.threads.clone() {
                    if !self.confident_about(target, &observation.threads) {
                        continue;
                    }
                    let file = observation.file.clone();
                    if self
                        .auto_scheduled
                        .iter()
                        .any(|(t, f)| *t == target && *f == file)
                    {
                        continue;
                    }
                    self.auto_scheduled.push((target, file.clone()));
                    let millis = 2_000 + n * 8_000;
                    n += 1;
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(millis))
                            .await;
                        this.update(cx, |this, cx| this.auto_release(target, &file, cx))
                            .ok();
                    })
                    .detach();
                }
            }
        }
    }

    /// One thread's turn comes up and what auto decided for it lands.
    ///
    /// The delay stands in for waiting on that agent: nothing is spliced into a
    /// prompt already in flight, so the notification arrives at a boundary
    /// rather than the instant the mode changed.
    fn auto_release(&mut self, target: usize, file: &str, cx: &mut Context<Self>) {
        if self.proactive != Proactive::Auto {
            return;
        }
        if let Some(c) = self
            .threads
            .get_mut(target)
            .and_then(|t| {
                t.comments.iter_mut().find(|c| {
                    c.about.as_deref() == Some(file) && c.delivery == Delivery::Proposed
                })
            })
        {
            c.delivery = Delivery::Pending;
            cx.notify();
        }
    }

    /// Stands in for the model's call in auto mode: dirty in overlapping lines
    /// is worth interrupting for; merely sharing a file is not.
    fn confident_about(&self, to: usize, others: &[usize]) -> bool {
        others.iter().filter(|i| **i != to).any(|other| {
            match (
                self.threads[to].dirty_lines(),
                self.threads[*other].dirty_lines(),
            ) {
                (Some(a), Some(b)) => a.start < b.end && b.start < a.end,
                _ => false,
            }
        })
    }

    /// Put an observation into a thread as something to approve, not as fact.
    fn propose_into(&mut self, file: String, to: usize, others: Vec<usize>, cx: &mut Context<Self>) {
        let body = self.proposal_body(&file, to, &others);
        if self.dismissed.iter().any(|(t, f)| *t == to && *f == file) {
            return;
        }
        if self.threads[to]
            .comments
            .iter()
            .any(|c| c.about.as_deref() == Some(file.as_str()))
        {
            return;
        }
        // Auto mode: something decides whether this is worth interrupting for.
        // Standing in for that judgement: if the two threads are dirty in
        // overlapping *lines* it goes straight through; merely sharing a file
        // is not enough, and waits.
        let thread = &self.threads[to];
        let span = (thread.seed.diff_lines.len() as u32).min(3);
        let anchor = PortableAnchor::capture(
            &thread.diff_snapshot(),
            BlockKind::DiffHunk,
            0..span,
            false,
        );
        let resolved = resolve_in(thread, &anchor);
        let handle = comment_handle(thread);
        self.next_id += 1;
        self.threads[to].comments.push(UiComment {
            id: self.next_id,
            handle,
            anchor,
            resolved,
            mentions: parse_mentions(&body),
            body,
            author: seed::ME,
            origin: Provenance::Local,
            // Always raised held. Auto releases it a beat later, per thread,
            // rather than everything going through the instant it is noticed.
            delivery: Delivery::Proposed,
            divergence: None,
            about: Some(file.clone()),
            replies: Vec::new(),
        });
        self.threads[to].unread = true;
        cx.notify();
    }

    /// Let it through to the agent, or keep it out.
    ///
    /// Deliberately *not* a verdict. Showing the agent means the agent now has
    /// the information; it says nothing about what anyone decided to do, and
    /// the two threads can still disagree afterwards. Whether the underlying
    /// question was settled is a separate thing, and it does not live on a
    /// comment.
    fn resolve_proposal(&mut self, id: u32, show_agent: bool, cx: &mut Context<Self>) {
        let active = self.active;
        let thread = &mut self.threads[active];
        if show_agent {
            if let Some(c) = thread.comments.iter_mut().find(|c| c.id == id) {
                c.delivery = Delivery::Pending;
            }
        } else if let Some(pos) = thread.comments.iter().position(|c| c.id == id) {
            let gone = thread.comments.remove(pos);
            if let Some(file) = gone.about {
                self.dismissed.push((active, file));
            }
        }
        cx.notify();
    }

    /// Proactive flags are hidden in V1.
    fn visible<'a>(&self, comment: &'a UiComment) -> bool {
        self.proactive.on() || comment.divergence.is_none()
    }

    fn my_mentions(&self) -> Vec<(usize, &UiComment)> {
        self.threads
            .iter()
            .enumerate()
            .flat_map(|(i, t)| {
                t.comments
                    .iter()
                    .filter(|c| self.visible(c))
                    .filter(|c| c.mentions.contains(&seed::ME))
                    .map(move |c| (i, c))
            })
            .collect()
    }

}

impl Shell {
    /// Called once the entity exists, so the script can drive it.
    pub fn maybe_start_demo(&mut self, cx: &mut Context<Self>) {
        match std::env::var("DELTA_MOCK_DEMO").as_deref() {
            Ok("v2") => {
                self.demo = demo_script_v2();
                self.schedule_beat(0, cx);
            }
            Ok(_) => self.start_demo(cx),
            Err(_) => {}
        }
    }
}

impl Focusable for Shell {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();
        div()
            .id("shell")
            .track_focus(&self.focus)
            .flex()
            .size_full()
            .bg(colors.background)
            .text_color(colors.text)
            .child(self.sidebar(cx))
            .child(self.conversation(cx))
            .child(self.review(cx))
    }
}

impl Shell {
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();
        let mut rail = div()
            .flex()
            .flex_col()
            .w(px(250.))
            .flex_shrink_0()
            .h_full()
            .bg(colors.panel_background)
            .border_r_1()
            .border_color(colors.border)
            .child(self.inbox_row(cx))
            .child(self.channel_row(cx))
            .child(section_label("Projects"));

        // Projects are listed even when empty, as they are in Delta's sidebar.
        for project in seed::PROJECTS {
            rail = rail.child(project_row(project.name));
            for (i, thread) in self
                .threads
                .iter()
                .enumerate()
                .filter(|(_, t)| t.project == project.name)
            {
                let active = i == self.active;
                // The dot reports what the agent is doing now, not what the
                // seed said at launch.
                let dot = if thread.agent_busy {
                    0x4A9E5C
                } else if thread.seed.status == Status::Conflicting {
                    0xE0A030
                } else {
                    0x555555
                };
                let count = thread.comments.iter().filter(|c| self.visible(c)).count();
            let mentions_me = thread
                .comments
                .iter()
                .filter(|c| self.visible(c))
                .any(|c| c.mentions.contains(&seed::ME));
                rail = rail.child(
                    div()
                        .id(thread.seed.title)
                        .flex()
                        .items_center()
                        .gap_2()
                        .pl_6()
                        .pr_3()
                        .py_1()
                        .cursor_pointer()
                        .when(active, |el| el.bg(colors.element_selected))
                        .hover(|el| el.bg(colors.element_hover))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.active = i;
                            this.target = None;
                            this.composer.update(cx, |c, cx| c.clear(cx));
                            this.forwarding = Forwarding::Idle;
                            this.view = View::Thread;
                            this.threads[i].unread = false;
                            cx.notify();
                        }))
                        .child(dot_el(dot))
                        .child(
                            div().flex_1().min_w(px(0.)).child(
                                Label::new(thread.seed.title)
                                    .size(LabelSize::Small)
                                    .truncate()
                                    .color(if active { Color::Default } else { Color::Muted }),
                            ),
                        )
                        .when(count > 0, |el| {
                            el.child(
                                Label::new(format!("{count}"))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Accent),
                            )
                        })
                        .when(mentions_me, |el| {
                        el.child(
                            div()
                                .text_size(px(10.))
                                .text_color(colors.text_accent)
                                .child("@"),
                        )
                    })
                    .when(thread.unread, |el| el.child(dot_el(0x2E6FBF))),
                );
            }
        }

        rail.child(div().flex_1()).child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .border_t_1()
                .border_color(colors.border)
                .child(dot_el(seed::ACTORS[seed::ME].color))
                .child(Label::new(seed::ACTORS[seed::ME].handle).size(LabelSize::Small)),
        )
    }

    /// The board. Above the threads, not inside any of them, visible to
    /// everyone -- so a cross-thread finding has somewhere to live that is not
    /// somebody's inbox.
    ///
    /// Called Signals rather than Conflicts: most of what lands here turns out
    /// to be nothing, and a name that calls every entry a conflict guarantees
    /// people stop reading it. Not Overlaps either, since a divergence in
    /// approach shares no lines at all and still belongs here.
    fn channel_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();
        let count = if self.proactive.on() {
            self.observations().len()
        } else {
            0
        };
        let active = self.view == View::Channel;
        div()
            .id("channel")
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .cursor_pointer()
            .when(active, |el| el.bg(colors.element_selected))
            .hover(|el| el.bg(colors.element_hover))
            .on_click(cx.listener(|this, _, _, cx| {
                this.view = View::Channel;
                this.forwarding = Forwarding::Idle;
                cx.notify();
            }))
            .child(dot_el(0xE0A030))
            .child(div().flex_1().child(
                Label::new("Signals").size(LabelSize::Small).color(
                    if active { Color::Default } else { Color::Muted },
                ),
            ))
            .when(count > 0, |el| {
                el.child(
                    Label::new(format!("{count}"))
                        .size(LabelSize::XSmall)
                        .color(Color::Accent),
                )
            })
    }

    fn channel_pane(&self, cx: &mut Context<Self>) -> gpui::Div {
        let colors = cx.theme().colors().clone();
        let mut body = div()
            .flex()
            .flex_col()
            .gap_3()
            .p_5()
            .flex_1()
            .min_w(px(0.))
            .overflow_hidden();

        if !self.proactive.on() {
            body = body.child(
                Label::new("Set `proactive` in the status bar to see what the workspace notices.")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            );
            return div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .child(pane_header("Signals", cx))
                .child(body)
                .child(self.status_bar(cx));
        }

        let observations = self.observations();
        if observations.is_empty() {
            body = body.child(
                Label::new("Nothing to report. No two live threads are in the same file.")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            );
        }
        for (row, observation) in observations.iter().enumerate() {
            let short = observation
                .file
                .rsplit('/')
                .next()
                .unwrap_or(&observation.file);
            let mut card = div()
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(colors.border)
                .bg(colors.surface_background)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_1()
                        .border_b_1()
                        .border_color(colors.border_variant)
                        .child(
                            div()
                                .px_1()
                                .rounded_sm()
                                .bg(gpui::rgba(0xE0A03030))
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(gpui::rgb(0xE0A030))
                                        .child("same file".to_string()),
                                ),
                        )
                        .child(Label::new(short.to_string()).size(LabelSize::XSmall)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_3()
                        .py_2()
                        .child(
                            Label::new(format!(
                                "{} live threads have uncommitted changes here.",
                                observation.threads.len()
                            ))
                            .size(LabelSize::Small),
                        )
                        .child(
                            Label::new(
                                "Whether each agent has been told. Nothing here decides anything.",
                            )
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                        ),
                );
            for (n, index) in observation.threads.iter().enumerate() {
                let target = *index;
                // Only whether that agent has it. *Why* it does not -- a person
                // has not looked, or the model chose to hold it -- is not the
                // board's business, and saying so would imply a decision.
                let notified = self.threads[target].comments.iter().any(|c| {
                    c.about.as_deref() == Some(observation.file.as_str())
                        && !matches!(c.delivery, Delivery::Proposed | Delivery::Queued)
                });
                let status = if notified {
                    ("agent notified", 0x4A9E5C)
                } else {
                    ("not notified", 0x777777)
                };
                card = card.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_1()
                        .border_t_1()
                        .border_color(colors.border_variant)
                        .child(dot_el(0x4A9E5C))
                        .child(
                            div().flex_1().min_w(px(0.)).child(
                                Label::new(self.threads[target].seed.title)
                                    .size(LabelSize::XSmall)
                                    .truncate(),
                            ),
                        )
                        .child(
                            div()
                                .id(("status", row * 16 + n))
                                .cursor_pointer()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .hover(|el| el.bg(colors.element_hover))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.active = target;
                                    this.view = View::Thread;
                                    this.threads[target].unread = false;
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(gpui::rgb(status.1))
                                        .child(status.0.to_string()),
                                ),
                        ),
                );
            }
            // The outcome reads as a comment, because that is what it is --
            // somebody writing down what was agreed. Same chrome as any other,
            // and unattributed until there is something to attribute.
            card = card.child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(colors.border_variant)
                    .child(
                        div()
                            .w(px(7.))
                            .h(px(7.))
                            .rounded_full()
                            .flex_shrink_0()
                            .border_1()
                            .border_color(colors.border),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_size(px(12.))
                            .text_color(colors.text_muted)
                            .child("No outcome written down yet.".to_string()),
                    ),
            );
            body = body.child(card);
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .child(pane_header("Signals", cx))
            .child(body)
            .child(self.status_bar(cx))
    }

    fn inbox_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();
        let count = self.my_mentions().len();
        let active = self.view == View::Mentions;
        div()
            .id("inbox")
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .mt_2()
            .cursor_pointer()
            .when(active, |el| el.bg(colors.element_selected))
            .hover(|el| el.bg(colors.element_hover))
            .on_click(cx.listener(|this, _, _, cx| {
                this.view = View::Mentions;
                this.forwarding = Forwarding::Idle;
                cx.notify();
            }))
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(colors.text_accent)
                    .child("@"),
            )
            .child(div().flex_1().child(
                Label::new("Mentions").size(LabelSize::Small).color(
                    if active { Color::Default } else { Color::Muted },
                ),
            ))
            .when(count > 0, |el| {
                el.child(
                    Label::new(format!("{count}"))
                        .size(LabelSize::XSmall)
                        .color(Color::Accent),
                )
            })
    }

    /// Mentions of you, gathered from every thread. Clicking one opens the
    /// thread it lives in, which is the whole point of routing to a person:
    /// you find the note without knowing which thread it landed in.
    fn inbox_pane(&self, cx: &mut Context<Self>) -> gpui::Div {
        let colors = cx.theme().colors().clone();
        let mut body = div()
            .flex()
            .flex_col()
            .gap_3()
            .p_5()
            .flex_1()
            .min_w(px(0.))
            .overflow_hidden();

        let mentions = self.my_mentions();
        if mentions.is_empty() {
            body = body.child(
                Label::new("No one has mentioned you yet.")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            );
        }
        for (row, (thread_index, comment)) in mentions.into_iter().enumerate() {
            let thread = &self.threads[thread_index];
            let where_ = describe_where(
                comment,
                &thread.diff_path(),
                thread.seed.diff_file,
                thread.seed.diff_lines,
            );
            body = body.child(
                div()
                    .id(("inbox-row", row))
                    .flex()
                    .flex_col()
                    .cursor_pointer()
                    .rounded_md()
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.surface_background)
                    .hover(|el| el.bg(colors.element_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active = thread_index;
                        this.view = View::Thread;
                        this.threads[thread_index].unread = false;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_3()
                            .py_1()
                            .border_b_1()
                            .border_color(colors.border_variant)
                            .child(Label::new(thread.seed.title).size(LabelSize::XSmall))
                            .child(div().flex_1())
                            .child(
                                Label::new(where_)
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap_2()
                            .px_3()
                            .py_2()
                            .child(dot_el(seed::ACTORS[comment.author].color))
                            .child(self.body_el(&comment.body, &format!("i{row}"), cx)),
                    ),
            );
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .child(pane_header("Mentions", cx))
            .child(body)
            .child(self.status_bar(cx))
    }

    fn conversation(&self, cx: &mut Context<Self>) -> gpui::Div {
        if self.view == View::Mentions {
            return self.inbox_pane(cx);
        }
        if self.view == View::Channel {
            return self.channel_pane(cx);
        }
        let colors = cx.theme().colors().clone();
        let thread = self.active();
        let prose_path = thread.prose_path();
        let diff_path = thread.diff_path();

        let mut body = div()
            .flex()
            .flex_col()
            .gap_3()
            .p_5()
            .flex_1()
            .min_w(px(0.))
            .overflow_hidden();

        if let Some(parent) = thread.seed.branched_from {
            body = body.child(
                Label::new(format!(
                    "branched from \"{parent}\" - shares its transcript, so anchors carry across"
                ))
                .size(LabelSize::XSmall)
                .color(Color::Muted),
            );
        }

        if thread.seed.blocks.is_empty() {
            body = body.child(
                Label::new("No transcript in this thread yet.")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            );
        }

        for (i, block) in thread.seed.blocks.iter().enumerate() {
            let composing = self.target == Some(Target::Prose(i));
            // The wash marks "this span carries a comment", not "this span is
            // selected" -- so it has to outlive composition.
            let commented = thread
                .comments
                .iter()
                .filter(|c| self.visible(c))
                .any(|c| c.anchor.file == prose_path && covers(&c.resolved, i));
            body = body.child(
                div()
                    .id(("prose", i))
                    .cursor_pointer()
                    .rounded_sm()
                    .px_1()
                    .when(block.code, |el| {
                        el.font_family("SF Mono")
                            .text_size(px(11.5))
                            .p_2()
                            .bg(colors.surface_background)
                    })
                    // Delta washes an anchored span in lavender; we reuse that.
                    .when(commented && !composing, |el| el.bg(gpui::rgba(0x8B7FD42E)))
                    .when(composing, |el| el.bg(gpui::rgba(0x8B7FD459)))
                    .hover(|el| el.bg(gpui::rgba(0x8B7FD424)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.begin(Target::Prose(i), window, cx)
                    }))
                    .text_size(px(13.))
                    .line_height(rems(1.55))
                    .child(block.text),
            );
            if composing {
                body = body.child(self.composer_el(cx));
            }
            for comment in thread
                .comments
                .iter()
                .filter(|c| self.visible(c))
                .filter(|c| c.anchor.file == prose_path && covers(&c.resolved, i))
            {
                body = body.child(self.comment_card(comment, cx));
            }
        }

        for comment in thread
            .comments
            .iter()
            .filter(|c| self.visible(c))
            .filter(|c| c.anchor.file == diff_path && !c.resolved.is_orphaned())
        {
            body = body.child(self.comment_card(comment, cx));
        }

        // Anchors that found nothing here. Never guessed at, never dropped:
        // quoted with a backlink so the content survives the hop.
        for comment in thread
            .comments
            .iter()
            .filter(|c| self.visible(c))
            .filter(|c| c.resolved.is_orphaned())
        {
            body = body.child(self.comment_card(comment, cx));
        }

        for reply in &thread.replies {
            body = body.child(self.reply_el(reply, cx));
        }

        if !thread.sent.is_empty() {
            body = body.child(self.receipts(cx));
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .child(pane_header(thread.seed.title, cx))
            .child(body)
            .child(self.turn_bar(cx))
            .child(self.status_bar(cx))
    }

    // ------------------------------------------------------------ composer --

    fn composer_el(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();
        let mut stack = div().flex().flex_col().gap_1().ml_4();

        stack = stack.child(
            div()
                .flex()
                .items_start()
                .gap_2()
                .px_3()
                .py_2()
                .rounded_md()
                .border_1()
                .border_color(colors.text_accent)
                .bg(colors.surface_background)
                .child(dot_el(seed::ACTORS[seed::ME].color))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .text_size(px(12.5))
                        .child(self.composer.clone()),
                ),
        );

        // The only thing that drops out of the box is the @ list, and only
        // once you have typed an @. Everything else that used to sit here was
        // the workspace volunteering opinions, which is a v2 idea in a v1
        // surface -- it is behind the proactive toggle now.
        if let Some(picker) = self.mention_picker(cx) {
            stack = stack.child(picker);
        }
        if self.proactive.on()
            && let Some(hint) = self.routing_hints(cx)
        {
            stack = stack.child(hint);
        }

        // Forwarding is a button, not another row in the list. It is a second
        // destination for what you just wrote, not a suggestion. Replies have
        // no span of their own, so there is nothing to forward.
        if matches!(self.target, Some(Target::Reply(_))) {
            return stack;
        }
        stack = stack.child(
            div().flex().items_center().gap_2().child(div().flex_1()).child(
                div()
                    .id("fwd-selection")
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .cursor_pointer()
                    .border_color(if self.forwarding == Forwarding::Selection {
                        colors.text_accent
                    } else {
                        colors.border
                    })
                    .bg(colors.element_background)
                    .hover(|el| el.bg(colors.element_hover))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.forwarding = if this.forwarding == Forwarding::Selection {
                            Forwarding::Idle
                        } else {
                            Forwarding::Selection
                        };
                        cx.notify();
                    }))
                    .child(
                        Label::new("Forward to thread")
                            .size(LabelSize::XSmall)
                            .color(Color::Default),
                    ),
            ),
        );
        if self.forwarding == Forwarding::Selection {
            stack = stack.child(self.forward_picker(Forwarding::Selection, cx));
        }
        stack
    }

    /// The `@` autocomplete. Up/Down to move, Tab or Enter to pick.
    fn mention_picker(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.picker_open(cx) {
            return None;
        }
        let colors = cx.theme().colors().clone();
        let candidates = self.mention_candidates(cx);
        let selected = self.mention_sel.min(candidates.len() - 1);
        let mut list = div()
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(colors.border)
            .bg(colors.elevated_surface_background)
            .overflow_hidden();
        for (row, target) in candidates.into_iter().enumerate() {
            let (dot, name, note) = match target {
                MentionTarget::Actor(i) => (
                    seed::ACTORS[i].color,
                    format!("@{}", seed::ACTORS[i].handle),
                    seed::ACTORS[i].role.to_string(),
                ),
                MentionTarget::Thread(i) => {
                    let t = &self.threads[i];
                    (
                        if t.agent_busy { 0x4A9E5C } else { 0x555555 },
                        format!("@{}", t.seed.slug),
                        t.seed.title.to_string(),
                    )
                }
            };
            list = list.child(
                div()
                    .id(("mention", row))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .when(row == selected, |el| el.bg(colors.element_selected))
                    .hover(|el| el.bg(colors.element_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.complete_mention(target, cx);
                        cx.notify();
                    }))
                    .child(dot_el(dot))
                    .child(Label::new(name).size(LabelSize::Small))
                    .child(div().flex_1())
                    .child(
                        Label::new(note)
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    ),
            );
        }
        Some(list)
    }

    /// Both halves of the overlap query, surfaced where the decision is made:
    /// who already knows this range, and who else is editing it right now.
    fn routing_hints(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let (file, lines) = self.target_file_lines()?;
        let index = self.overlap();
        let experts = index.experts(&file, &lines);
        let mine = seed::transcript_root(self.active().seed.title);
        let conflicts: Vec<_> = index
            .conflicts(ThreadId(self.active as u32), &file, &lines)
            .into_iter()
            // A branch of this thread is the same work by another route. It
            // overlaps by construction, and saying so every time is noise.
            .filter(|c| {
                let other = self.threads[c.other_thread.0 as usize].seed.title;
                seed::transcript_root(other) != mine
            })
            .collect();
        if experts.is_empty() && conflicts.is_empty() {
            return None;
        }
        let colors = cx.theme().colors().clone();
        let mut row = div().flex().flex_col().gap_1();

        if let Some(expert) = experts.first() {
            let actor = expert.author.0 as usize;
            let handle = seed::ACTORS[actor].handle;
            let reason = expert.reason.clone();
            row = row.child(
                div()
                    .id("expert-chip")
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .bg(colors.element_background)
                    .hover(|el| el.bg(colors.element_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let mut text = this.composer_text(cx);
                        if !text.is_empty() && !text.ends_with(' ') {
                            text.push(' ');
                        }
                        text.push('@');
                        text.push_str(handle);
                        text.push(' ');
                        this.set_composer(text, cx);
                        this.picker_dismissed = false;
                        cx.notify();
                    }))
                    .child(dot_el(seed::ACTORS[actor].color))
                    .child(Label::new(format!("ask @{handle}")).size(LabelSize::XSmall))
                    .child(
                        Label::new(reason)
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    ),
            );
        }

        for conflict in conflicts.iter().take(2) {
            let other = &self.threads[conflict.other_thread.0 as usize];
            let word = match conflict.kind {
                ConflictKind::Overlapping => "also editing",
                ConflictKind::Adjacent => "editing nearby",
            };
            row = row.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(gpui::rgba(0xE0A03024))
                    .child(dot_el(0xE0A030))
                    .child(
                        Label::new(format!(
                            "{word}: {} (lines {}-{}, turn {})",
                            other.seed.title,
                            conflict.their_lines.start,
                            conflict.their_lines.end - 1,
                            conflict.their_turn
                        ))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                    ),
            );
        }
        Some(row)
    }

    // -------------------------------------------------------------- cards --

    fn comment_card(&self, comment: &UiComment, cx: &mut Context<Self>) -> gpui::Div {
        let colors = cx.theme().colors().clone();
        let thread = self.active();
        let id = comment.id;
        let orphaned = comment.resolved.is_orphaned();

        let anchor_line = describe_where(
            comment,
            &thread.diff_path(),
            thread.seed.diff_file,
            thread.seed.diff_lines,
        );
        let badge = match &comment.resolved {
            Resolution::Exact { .. } => None,
            Resolution::Shifted { similarity, .. } => {
                Some((format!("re-anchored {:.0}%", similarity * 100.), 0xE0A030))
            }
            Resolution::Orphaned => Some(("orphaned".to_string(), 0xB05050)),
        };

        let mut header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(colors.border_variant)
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(colors.text_accent)
                    .child(comment.handle.clone()),
            )
            .child(
                Label::new(anchor_line)
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            );
        if let Some((text, color)) = badge {
            header = header.child(
                div()
                    .px_1()
                    .rounded_sm()
                    .bg(gpui::rgba((color << 8) | 0x30))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(gpui::rgb(color))
                            .child(text),
                    ),
            );
        }
        header = header.child(div().flex_1());
        // Delivery controls live on their own row. Six things in one header ran
        // past the card edge, which put `ignore` out of reach entirely.
        let mut controls = div().flex().items_center().gap_2().px_3().pb_1();

        // One verb per concept. "send" belongs to the batch button and nothing
        // else; crossing threads is a forward; being in or out of the batch is
        // a state, not a second send.
        //
        // Forwarding stays available after delivery -- a comment your agent has
        // already seen is often exactly the one worth handing on. Only the
        // withhold toggle retires, since there is nothing left to withhold.
        header = header.child(
            div()
                .id(("reply-open", id))
                .cursor_pointer()
                .px_2()
                .rounded_sm()
                .hover(|el| el.bg(colors.element_hover))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.begin(Target::Reply(id), window, cx)
                }))
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(colors.text_muted)
                        .child("reply".to_string()),
                ),
        );
        header = header.child(
            div()
                .id(("fwd-open", id))
                .cursor_pointer()
                .px_2()
                .rounded_sm()
                .hover(|el| el.bg(colors.element_hover))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.forwarding = if this.forwarding == Forwarding::Comment(id) {
                        Forwarding::Idle
                    } else {
                        Forwarding::Comment(id)
                    };
                    cx.notify();
                }))
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(colors.text_muted)
                        .child("forward...".to_string()),
                ),
        );
        let has_controls = comment.delivery == Delivery::Proposed;
        if has_controls {
            controls = controls
                .child(
                    div()
                        .px_1()
                        .rounded_sm()
                        .bg(gpui::rgba(0xE0A03030))
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(gpui::rgb(0xE0A030))
                                .child("agent hasn't seen this".to_string()),
                        ),
                )
                .child(
                    div()
                        .id(("approve", id))
                        .cursor_pointer()
                        .px_2()
                        .rounded_sm()
                        .hover(|el| el.bg(colors.element_hover))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.resolve_proposal(id, true, cx)
                        }))
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(colors.text_accent)
                                .child("show agent".to_string()),
                        ),
                )
                .child(
                    div()
                        .id(("dismiss", id))
                        .cursor_pointer()
                        .px_2()
                        .rounded_sm()
                        .hover(|el| el.bg(colors.element_hover))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.resolve_proposal(id, false, cx)
                        }))
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(colors.text_muted)
                                .child("ignore".to_string()),
                        ),
                );
        } else if comment.divergence.is_some() {
            header = header.child(
                div()
                    .px_1()
                    .rounded_sm()
                    .bg(gpui::rgba(0xE0A03030))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(gpui::rgb(0xE0A030))
                            .child("proactive - v2".to_string()),
                    ),
            );
        }
        // No delivery chip. A comment is a comment living in the thread -- it
        // does not need a per-comment decision attached to it. The one case
        // where timing is worth saying out loud is a forward that landed while
        // the receiving agent was mid-turn, and that is said on the provenance
        // line, where it is actually about something.

        let mut card = div()
            .flex()
            .flex_col()
            .ml_4()
            .rounded_md()
            .border_1()
            .border_color(if orphaned {
                colors.border_variant
            } else {
                colors.border
            })
            .bg(colors.surface_background)
            .child(header);
        if has_controls {
            card = card.child(controls);
        }

        // Provenance line. Under option (A) an injected comment lands with no
        // accept step, so saying where it came from is the whole safeguard.
        if let Provenance::Forwarded {
            from_title,
            forwarded_by,
            original_author,
        } = comment.origin
        {
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_3()
                    .py_1()
                    .bg(gpui::rgba(0x8B7FD41A))
                    .child(
                        Label::new(format!(
                            "{}forwarded by @{} - originally @{} in @{}",
                            if comment.delivery == Delivery::Queued {
                                "queued behind the current turn - "
                            } else {
                                ""
                            },
                            seed::ACTORS[forwarded_by].handle,
                            seed::ACTORS[original_author].handle,
                            seed::thread_by_title(from_title)
                                .map(thread_slug)
                                .unwrap_or_else(|| from_title.to_string())
                        ))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                    ),
            );
        }

        // An orphan still carries its content, so quote what it pointed at. A
        // note-less snippet quotes it too -- there the code *is* the message.
        let snippet_only = comment.body.is_empty();
        if orphaned || snippet_only {
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .px_3()
                    .py_1()
                    .child(
                        Label::new(if orphaned {
                            "nothing here matches; quoting the original"
                        } else {
                            "sent as a snippet, no note"
                        })
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                    )
                    .child(
                        div()
                            .border_l_2()
                            .border_color(colors.border)
                            .pl_2()
                            .font_family("SF Mono")
                            .text_size(px(10.5))
                            .text_color(colors.text_muted)
                            .child(comment.anchor.content.clone()),
                    ),
            );
        }

        if !snippet_only {
            card = card.child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .px_3()
                    .py_2()
                    // A flag has no author. Agents are not named; the badge
                    // says what this is.
                    .child(
                        div()
                            .w(px(7.))
                            .h(px(7.))
                            .rounded_full()
                            .flex_shrink_0()
                            // A workspace finding has no author -- a hollow
                            // ring, not somebody's cursor colour.
                            .when(
                                comment.divergence.is_some() || comment.about.is_some(),
                                |el| el.border_1().border_color(gpui::rgb(0xE0A030)),
                            )
                            .when(
                                comment.divergence.is_none() && comment.about.is_none(),
                                |el| el.bg(gpui::rgb(seed::ACTORS[comment.author].color)),
                            ),
                    )
                    .child(self.body_el(&comment.body, &format!("c{id}"), cx)),
            );
        }

        for (n, reply) in comment.replies.iter().enumerate() {
            card = card.child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .pl_6()
                    .pr_3()
                    .py_1()
                    .border_t_1()
                    .border_color(colors.border_variant)
                    .child(dot_el(seed::ACTORS[reply.author].color))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .flex_1()
                            .min_w(px(0.))
                            .child(self.body_el(&reply.body, &format!("r{id}-{n}"), cx)),
                    ),
            );
        }

        if self.target == Some(Target::Reply(id)) {
            card = card.child(div().pl_4().pr_2().py_1().child(self.composer_el(cx)));
        }

        if self.forwarding == Forwarding::Comment(id) {
            card = card.child(self.forward_picker(Forwarding::Comment(id), cx));
        }
        card
    }


    /// Where a snippet can go. Every other thread is a candidate; the anchor
    /// decides on arrival whether it lands or orphans.
    fn forward_picker(&self, what: Forwarding, cx: &mut Context<Self>) -> gpui::Div {
        let colors = cx.theme().colors().clone();
        let mut list = div()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(colors.border_variant)
            .child(
                div().px_3().py_1().child(
                    Label::new("send this snippet to")
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                ),
            );
        for (i, thread) in self.threads.iter().enumerate() {
            if i == self.active {
                continue;
            }
            // The dot reports what the agent is doing now, not what the
            // seed said at launch.
            let dot = if thread.agent_busy {
                0x4A9E5C
            } else if thread.seed.status == Status::Conflicting {
                0xE0A030
            } else {
                0x555555
            };
            // Same-file threads are where the anchor can actually re-resolve.
            let same_file =
                thread.has_diff() && thread.seed.diff_file == self.active().seed.diff_file;
            list = list.child(
                div()
                    .id(("fwd", i))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(|el| el.bg(colors.element_hover))
                    .on_click(cx.listener(move |this, _, _, cx| this.forward_to(what, i, cx)))
                    .child(dot_el(dot))
                    .child(
                        div().flex_1().min_w(px(0.)).child(
                            Label::new(thread.seed.title)
                                .size(LabelSize::XSmall)
                                .truncate(),
                        ),
                    )
                    .when(same_file, |el| {
                        el.child(
                            Label::new(thread.seed.diff_file)
                                .size(LabelSize::XSmall)
                                .color(Color::Accent),
                        )
                    }),
            );
        }
        list
    }

    /// What is waiting on the agent. Visible so that "queued" is something the
    /// system tells you, not something you had to decide.
    fn turn_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();
        let thread = self.active();
        let queued = thread
            .comments
            .iter()
            .filter(|c| c.delivery == Delivery::Queued)
            .filter(|c| matches!(c.origin, Provenance::Forwarded { .. }))
            .count();
        let show = thread.agent_busy || queued > 0;
        div()
            .when(!show, |el| el.invisible().h(px(0.)))
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(colors.border)
            .bg(gpui::rgba(0xE0A03014))
            .child(dot_el(0x4A9E5C))
            .child(
                Label::new(if thread.agent_busy {
                    "agent is mid-turn"
                } else {
                    "waiting for the turn boundary"
                })
                .size(LabelSize::Small),
            )
            .when(queued > 0, |el| {
                el.child(
                    Label::new(format!(
                        "{queued} forwarded comment{} waiting",
                        if queued == 1 { "" } else { "s" }
                    ))
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
                )
            })
            .child(div().flex_1())
            .child(
                Label::new("replies land when it finishes")
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
    }

    fn review(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();
        let thread = self.active();
        let diff_path = thread.diff_path();
        let mut hunk = div()
            .flex()
            .flex_col()
            .font_family("SF Mono")
            .text_size(px(11.))
            .overflow_hidden();

        for (i, (line_no, kind, text)) in thread.seed.diff_lines.iter().enumerate() {
            let composing = matches!(self.target, Some(Target::Diff(a, b)) if i >= a && i <= b);
            let commented = thread
                .comments
                .iter()
                .filter(|c| self.visible(c))
                .any(|c| c.anchor.file == diff_path && covers(&c.resolved, i));
            let bg = if composing {
                gpui::rgba(0x8B7FD459).into()
            } else if commented {
                gpui::rgba(0x8B7FD42E).into()
            } else {
                match kind {
                    Some(true) => colors.version_control_added.opacity(0.15),
                    Some(false) => colors.version_control_deleted.opacity(0.15),
                    None => gpui::transparent_black(),
                }
            };
            let lines = thread.seed.diff_lines;
            hunk = hunk.child(
                div()
                    .id(("diff", i))
                    .flex()
                    .gap_3()
                    .px_3()
                    .cursor_pointer()
                    .bg(bg)
                    .hover(|el| el.bg(gpui::rgba(0x8B7FD424)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        // Click one line, get its contiguous change block --
                        // Delta auto-expands a caret to the whole block rather
                        // than making you drag-select.
                        let (mut a, mut b) = (i, i);
                        let kind = lines[i].1;
                        while a > 0 && lines[a - 1].1 == kind {
                            a -= 1;
                        }
                        while b + 1 < lines.len() && lines[b + 1].1 == kind {
                            b += 1;
                        }
                        this.begin(Target::Diff(a, b), window, cx)
                    }))
                    .child(
                        div()
                            .w(px(30.))
                            .flex_shrink_0()
                            .text_color(colors.text_muted)
                            .child(format!("{line_no}")),
                    )
                    .child(div().min_w(px(0.)).overflow_hidden().child(*text)),
            );
            // Delta puts the box inline where you highlighted rather than in a
            // separate composer elsewhere on screen, so this follows the caret.
            if matches!(self.target, Some(Target::Diff(_, b)) if i == b) {
                hunk = hunk.child(div().py_1().pr_2().child(self.composer_el(cx)));
            }
        }

        let mut pane = div()
            .flex()
            .flex_col()
            .w(px(560.))
            .flex_shrink_0()
            .h_full()
            .border_l_1()
            .border_color(colors.border)
            .child(pane_header("Review Changes", cx));

        if thread.has_diff() {
            pane = pane
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .py_2()
                        .child(Label::new("Branch").size(LabelSize::Small))
                        .child(
                            Label::new("since origin/main")
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                        .child(DiffStat::new("rd", thread.seed.added, thread.seed.removed)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .py_2()
                        .border_t_1()
                        .border_color(colors.border_variant)
                        .child(Label::new(thread.seed.diff_file).size(LabelSize::Small))
                        .child(
                            Label::new(thread.seed.diff_dir)
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                )
                .child(div().flex_1().overflow_hidden().child(hunk));
        } else {
            pane = pane.child(
                div().p_4().child(
                    Label::new("No changes in this thread's worktree.")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
            );
        }
        pane
    }

    fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();
        let thread = self.active();
        div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(30.))
            .px_4()
            .flex_shrink_0()
            .border_t_1()
            .border_color(colors.border)
            .child(
                Label::new("delta")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(
                Label::new("main")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(DiffStat::new("sd", thread.seed.added, thread.seed.removed))
            .child(div().flex_1())
            .child(
                div()
                    .id("proactive-toggle")
                    .cursor_pointer()
                    .px_2()
                    .rounded_sm()
                    .hover(|el| el.bg(colors.element_hover))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.proactive = this.proactive.next();
                        if this.proactive != Proactive::Auto {
                            this.auto_scheduled.clear();
                        }
                        this.sweep(cx);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(if self.proactive.on() {
                                gpui::rgb(0xE0A030).into()
                            } else {
                                colors.text_muted
                            })
                            .child(self.proactive.label().to_string()),
                    ),
            )
            .child(Label::new("Low").size(LabelSize::Small).color(Color::Muted))
            .child(
                Label::new("Claude Fable 5")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(
                Label::new("Local")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
    }

    /// An agent response, rendered as Delta does: a `-> Re:` header, the quoted
    /// comment, then the answer. One block per comment rather than one blob per
    /// batch -- that separation is the whole reason for batching.
    fn reply_el(&self, reply: &AgentReply, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors().clone();
        div()
            .flex()
            .flex_col()
            .gap_2()
            .pt_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(Label::new("↪").size(LabelSize::XSmall).color(Color::Muted))
                    .child(
                        Label::new(format!("Re: {}", seed::ACTORS[reply.quote_author].handle))
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    ),
            )
            .child(
                div()
                    .border_l_2()
                    .border_color(colors.text_accent.opacity(0.5))
                    .pl_3()
                    .py_1()
                    .child(
                        Label::new(reply.quote.clone())
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .line_height(rems(1.55))
                    .child(reply.body.clone()),
            )
    }
}

// ------------------------------------------------------------------ utils --

fn covers(resolution: &Resolution, index: usize) -> bool {
    resolution
        .lines()
        .is_some_and(|r| (index as u32) >= r.start && (index as u32) < r.end)
}

/// Human-readable location, driven by where the anchor *resolved* rather than
/// by whatever line numbers the sender happened to have.
fn describe_where(
    comment: &UiComment,
    diff_path: &str,
    diff_file: &str,
    diff_lines: &[seed::DiffLine],
) -> String {
    if comment.resolved.is_orphaned() {
        return format!("{} (unanchored)", short_path(&comment.anchor.file));
    }
    if comment.anchor.file != diff_path {
        return "transcript".to_string();
    }
    let Some(range) = comment.resolved.lines() else {
        return diff_file.to_string();
    };
    let first = diff_lines.get(range.start as usize).map(|l| l.0);
    let last = diff_lines
        .get(range.end.saturating_sub(1) as usize)
        .map(|l| l.0);
    match (first, last) {
        (Some(a), Some(b)) => format!("{diff_file}  (line: {a}-{b})"),
        _ => diff_file.to_string(),
    }
}

fn short_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("thread://") {
        return format!("\"{}\" transcript", rest.trim_end_matches("/transcript"));
    }
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn parse_mentions(body: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for token in body.split_whitespace() {
        let Some(handle) = token.strip_prefix('@') else {
            continue;
        };
        let handle =
            handle.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
        // Agents are not mention targets. To reach the agent in another thread
        // you forward to the thread; `@` only ever addresses a person.
        if let Some(i) = seed::ACTORS
            .iter()
            .position(|a| !a.agent && a.handle == handle)
            && !out.contains(&i)
        {
            out.push(i);
        }
    }
    out
}

/// Strip trailing punctuation from a mention token.
fn mention_word(token: &str) -> Option<&str> {
    let handle = token.strip_prefix('@')?;
    Some(handle.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_'))
}

fn section_label(text: &'static str) -> impl IntoElement {
    div()
        .px_3()
        .pt_3()
        .pb_1()
        .child(Label::new(text).size(LabelSize::XSmall).color(Color::Muted))
}

fn project_row(name: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_1()
        .child(Label::new(name).size(LabelSize::Small))
}

fn dot_el(color: u32) -> impl IntoElement {
    div()
        .w(px(7.))
        .h(px(7.))
        .rounded_full()
        .bg(gpui::rgb(color))
        .flex_shrink_0()
}

fn pane_header(title: &str, cx: &mut Context<Shell>) -> impl IntoElement {
    let colors = cx.theme().colors().clone();
    div()
        .flex()
        .items_center()
        .h(px(38.))
        .px_4()
        .flex_shrink_0()
        .border_b_1()
        .border_color(colors.border)
        .child(Label::new(title.to_string()).size(LabelSize::Small))
}

/// Stand-in for a model response. Theatre by design -- the point is to exercise
/// the *routing* of a reply to its parent comment, not to be convincing prose.
fn synth_reply(comment: &str, where_: &str) -> String {
    let lower = comment.to_lowercase();
    if lower.contains("remove") || lower.contains("delete") || lower.contains("drop") {
        format!(
            "Done — dropped that from {where_}. The doc comment restated what the \
             signature already says, so removing it costs nothing. `cargo check` \
             and the existing catalog tests still pass."
        )
    } else if lower.contains("why") || lower.ends_with('?') {
        format!(
            "Because the segments are title-cased before the acronym match runs, so \
             `gpt` would never reach the `\"gpt\"` arm otherwise. That ordering is \
             what {where_} is guarding. Want me to add a test that pins it?"
        )
    } else if lower.contains("test") {
        format!(
            "Added a case covering {where_}. It asserts `humanize_model_id\
             (\"claude-sonnet-4-5\")` renders as \"Claude Sonnet 4.5\", which is the \
             behaviour that regressed last time."
        )
    } else if lower.contains("rename") || lower.contains("name") {
        format!(
            "Renamed it across {where_} and the two call sites. Nothing outside this \
             module referenced the old name, so the change is contained."
        )
    } else {
        format!(
            "Understood — applying that to {where_}. I'll leave the surrounding \
             structure alone so the diff stays reviewable."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(title: &str) -> ThreadUi {
        let (project, seed_thread) = seed::PROJECTS
            .iter()
            .flat_map(|p| p.threads.iter().map(move |t| (p.name, t)))
            .find(|(_, t)| t.title == title)
            .expect("seed thread");
        ThreadUi {
            seed: seed_thread,
            project,
            comments: Vec::new(),
            replies: Vec::new(),
            sent: Vec::new(),
            turn: 1,
            unread: false,
            agent_busy: seed_thread.status == Status::Running,
        }
    }

    /// Anchor a run of diff lines in `from`, as `commit` would.
    fn anchor_diff(from: &ThreadUi, a: u32, b: u32) -> PortableAnchor {
        PortableAnchor::capture(&from.diff_snapshot(), BlockKind::DiffHunk, a..b, true)
    }

    #[test]
    fn a_snippet_forwarded_to_a_thread_that_moved_it_still_lands_exactly() {
        // The doc-comment block survives verbatim in the other worktree, just
        // at a different offset. Content-first resolution must follow it.
        let labels = thread("Prevent Unknown Model Labels");
        let jitter = thread("Fix Compaction Divider Spacing Jitter");
        let anchor = anchor_diff(&labels, 2, 7);
        assert_eq!(
            resolve_in(&jitter, &anchor),
            Resolution::Exact { lines: 6..11 },
            "doc block moved from index 2 to 6 between worktrees"
        );
    }

    #[test]
    fn a_snippet_the_other_worktree_edited_re_anchors_by_context() {
        // `previous_numeric` grew a trailing comment there, so the content hash
        // misses and the surrounding lines have to carry it.
        let labels = thread("Prevent Unknown Model Labels");
        let jitter = thread("Fix Compaction Divider Spacing Jitter");
        let anchor = anchor_diff(&labels, 8, 10);
        match resolve_in(&jitter, &anchor) {
            Resolution::Shifted { lines, similarity } => {
                assert_eq!(lines, 12..14);
                assert!(similarity > 0.9, "context was intact, got {similarity}");
            }
            other => panic!("expected Shifted, got {other:?}"),
        }
    }

    #[test]
    fn a_snippet_sent_to_a_thread_touching_another_file_orphans_rather_than_guessing() {
        let labels = thread("Prevent Unknown Model Labels");
        let wrapping = thread("Investigate Comment Summary Wrapping");
        let anchor = anchor_diff(&labels, 2, 7);
        assert!(resolve_in(&wrapping, &anchor).is_orphaned());
        // ...and the content travels anyway, so the recipient can still read it.
        assert!(anchor.content.contains("Renders a persisted model id"));
    }

    #[test]
    fn a_snippet_sent_to_a_thread_with_no_worktree_changes_orphans() {
        let labels = thread("Prevent Unknown Model Labels");
        let empty = thread("Explore Running Terminal Icon");
        let anchor = anchor_diff(&labels, 2, 7);
        assert!(resolve_in(&empty, &anchor).is_orphaned());
    }

    #[test]
    fn transcript_anchors_never_re_resolve_in_someone_elses_transcript() {
        // Prose paths are per thread on purpose: quoting is honest, guessing is
        // not, and two transcripts share no coordinate system.
        let labels = thread("Prevent Unknown Model Labels");
        let jitter = thread("Fix Compaction Divider Spacing Jitter");
        let anchor =
            PortableAnchor::capture(&labels.prose_snapshot(), BlockKind::Paragraph, 0..1, true);
        assert!(resolve_in(&labels, &anchor).is_orphaned() == false);
        assert!(resolve_in(&jitter, &anchor).is_orphaned());
    }

    /// Why `prose_path` is per thread rather than a shared `thread://transcript`.
    ///
    /// Resolution step 2 is a bare content-hash search over the whole document,
    /// so any line that appears in both transcripts cross-anchors as `Exact` --
    /// full confidence, wrong document. Conversation transcripts are full of
    /// exactly the short boilerplate that collides ("Looks good to me.").
    /// Scoping the path per thread makes that unrepresentable instead of rare.
    #[test]
    fn a_shared_transcript_path_would_cross_anchor_on_a_repeated_line() {
        let a = Snapshot::new(
            "thread://transcript",
            "Looks good to me.\nThe ordering matters because segments are title-cased.",
        );
        let b = Snapshot::new(
            "thread://transcript",
            "Rewrote the divider math.\nStill flaky at Comfortable.\nLooks good to me.",
        );
        let anchor = PortableAnchor::capture(&a, BlockKind::Paragraph, 0..1, true);
        assert_eq!(
            anchor.resolve(&b),
            Resolution::Exact { lines: 2..3 },
            "a shared path lets an unrelated transcript claim the anchor"
        );

        // With the real per-thread paths the same pair cannot collide at all.
        let a_scoped = Snapshot::new("thread://Thread A/transcript", "Looks good to me.");
        let b_scoped = Snapshot::new("thread://Thread B/transcript", "Looks good to me.");
        let scoped = PortableAnchor::capture(&a_scoped, BlockKind::Paragraph, 0..1, true);
        assert!(scoped.resolve(&b_scoped).is_orphaned());
    }

    /// The one case where cross-thread prose re-anchoring is legitimate: a
    /// branch is a copy of its parent's transcript, so they share a path and a
    /// lineage, and "where did this paragraph go" is a real question again.
    #[test]
    fn a_branch_shares_its_parents_transcript_so_prose_anchors_carry() {
        let parent = thread("Prevent Unknown Model Labels");
        let branch = thread("Prevent Unknown Model Labels (registry retry)");
        assert_eq!(parent.prose_path(), branch.prose_path());

        // The branch note pushed everything down one; the block is untouched.
        let moved =
            PortableAnchor::capture(&parent.prose_snapshot(), BlockKind::Paragraph, 0..1, true);
        assert_eq!(
            resolve_in(&branch, &moved),
            Resolution::Exact { lines: 1..2 }
        );

        // That block was rewritten on the branch, so context has to carry it.
        // Only the *leading* context matches: the parent kept talking after the
        // branch point, so the blocks that followed it there do not exist here.
        // Half a context is still enough to place it, which is the point of
        // having a threshold rather than demanding an exact surround.
        let rewritten =
            PortableAnchor::capture(&parent.prose_snapshot(), BlockKind::Paragraph, 3..4, true);
        match resolve_in(&branch, &rewritten) {
            Resolution::Shifted { lines, similarity } => {
                assert_eq!(lines, 4..5);
                assert!(
                    similarity >= 0.4,
                    "leading context alone should place it, got {similarity}"
                );
            }
            other => panic!("expected Shifted, got {other:?}"),
        }
    }

    /// A branch is still not a free-for-all: unrelated threads stay walled off.
    #[test]
    fn branching_does_not_open_prose_anchoring_to_unrelated_threads() {
        let branch = thread("Prevent Unknown Model Labels (registry retry)");
        let jitter = thread("Fix Compaction Divider Spacing Jitter");
        assert_ne!(branch.prose_path(), jitter.prose_path());
        let anchor =
            PortableAnchor::capture(&branch.prose_snapshot(), BlockKind::Paragraph, 0..1, true);
        assert!(resolve_in(&jitter, &anchor).is_orphaned());
    }

    #[test]
    fn seeded_comments_route_to_the_local_actor() {
        let mentioning_me = seed::COMMENTS
            .iter()
            .filter(|c| parse_mentions(c.body).contains(&seed::ME))
            .count();
        assert!(
            mentioning_me >= 2,
            "the inbox needs incoming mentions to be reachable at launch"
        );
        // ...and none of them are authored by whoever is driving the window,
        // otherwise "someone mentioned you" would be self-addressed.
        assert!(seed::COMMENTS.iter().all(|c| c.author != seed::ME));
    }

    /// Nobody decides this. A note dropped into a thread whose agent is working
    /// waits for the boundary; the same note into an idle thread arms straight
    /// away. The sender is never asked, and never at fault for the timing.
    #[test]
    fn arrival_state_is_decided_by_the_recipient_not_the_sender() {
        let human = seed::ME;
        assert_eq!(arrival_delivery(human, true), Delivery::Queued);
        assert_eq!(arrival_delivery(human, false), Delivery::Pending);
    }

    /// Agents will start doing this themselves. When they do, their forwards
    /// must not arm on their own and must not bounce back where they came from,
    /// or two agents can steer each other with no human anywhere in the loop.
    #[test]
    fn an_agents_forward_always_waits_for_a_human_and_never_returns_home() {
        let agent = seed::ACTORS.iter().position(|a| a.agent).expect("an agent");
        assert_eq!(arrival_delivery(agent, false), Delivery::Queued);
        assert_eq!(arrival_delivery(agent, true), Delivery::Queued);

        assert!(!forward_allowed(agent, Some("Thread A"), "Thread A"));
        assert!(forward_allowed(agent, Some("Thread A"), "Thread B"));
        // A person may hand something back deliberately; that is not a loop.
        assert!(forward_allowed(seed::ME, Some("Thread A"), "Thread A"));
    }

    /// Queued means "not yet", not "excluded". The boundary releases it.
    #[test]
    fn the_turn_boundary_arms_everything_that_arrived_mid_turn() {
        let mut deliveries = vec![
            Delivery::Queued,
            Delivery::Withheld,
            Delivery::Delivered { turn: 1 },
        ];
        for d in &mut deliveries {
            if *d == Delivery::Queued {
                *d = Delivery::Pending;
            }
        }
        assert_eq!(
            deliveries,
            vec![
                Delivery::Pending,
                Delivery::Withheld,
                Delivery::Delivered { turn: 1 }
            ],
            "withheld and delivered are untouched by the boundary"
        );
    }

    /// The seam is real: something that is not the UI can implement it and be
    /// driven by the same envelope. This is the shape a network backend takes.
    #[test]
    fn the_transport_seam_can_be_implemented_by_something_that_is_not_the_shell() {
        use crate::model::transport::{Envelope, Receipt, SendError, ThreadTransport};

        #[derive(Default)]
        struct Outbox {
            sent: Vec<Envelope>,
        }
        impl ThreadTransport for Outbox {
            fn send(&mut self, envelope: Envelope) -> Result<Receipt, SendError> {
                if envelope.to == envelope.from {
                    return Err(SendError::SameThread);
                }
                let id = CommentId(self.sent.len() as u32 + 1);
                self.sent.push(envelope);
                Ok(Receipt {
                    comment: id,
                    resolution: Resolution::Orphaned,
                    delivery: Delivery::Queued,
                })
            }
        }

        let labels = thread("Prevent Unknown Model Labels");
        let anchor = anchor_diff(&labels, 2, 7);
        let envelope = |to: u32| Envelope {
            from: ThreadId(0),
            to: ThreadId(to),
            sender: ActorId(seed::ME as u32),
            author: ActorId(1),
            anchor: anchor.clone(),
            body: "take a look".into(),
            mentions: vec![ActorId(1)],
            from_title: labels.seed.title.to_string(),
        };

        let mut outbox = Outbox::default();
        assert!(outbox.send(envelope(1)).is_ok());
        assert!(matches!(outbox.send(envelope(0)), Err(SendError::SameThread)));
        assert_eq!(outbox.sent.len(), 1);
        // The snippet travels whole, so a backend needs no second round trip.
        assert!(outbox.sent[0].anchor.content.contains("Renders a persisted"));
        assert!(!outbox.sent[0].is_snippet_only());
    }

    #[test]
    fn an_envelope_with_no_body_is_a_snippet() {
        let labels = thread("Prevent Unknown Model Labels");
        let envelope = Envelope {
            from: ThreadId(0),
            to: ThreadId(1),
            sender: ActorId(seed::ME as u32),
            author: ActorId(seed::ME as u32),
            anchor: anchor_diff(&labels, 2, 7),
            body: String::new(),
            mentions: Vec::new(),
            from_title: labels.seed.title.to_string(),
        };
        assert!(envelope.is_snippet_only());
    }

    /// The case a diff cannot see: the two threads that disagree touch
    /// different files entirely, so nothing in the overlap index fires.
    #[test]
    fn the_diverging_pair_has_no_textual_overlap_at_all() {
        let divergence = &seed::DIVERGENCES[0];
        let a = thread(divergence.a_thread);
        let b = thread(divergence.b_thread);
        assert_ne!(a.diff_path(), b.diff_path(), "different files");

        // No shared content either, beyond closing braces. Git merges both
        // without complaint; there is nothing for a line index to notice.
        let substantive = |line: &&seed::DiffLine| {
            line.2.chars().any(|c| c.is_alphanumeric())
        };
        let a_lines: Vec<_> = a
            .seed
            .diff_lines
            .iter()
            .filter(substantive)
            .map(|l| l.2.trim())
            .collect();
        let shared = b
            .seed
            .diff_lines
            .iter()
            .filter(substantive)
            .filter(|l| a_lines.contains(&l.2.trim()))
            .count();
        assert_eq!(shared, 0, "no overlapping content for a diff to catch");

        // The disagreement is only visible in what each said it would do.
        let (mine, theirs, _) = divergence.sides(divergence.a_thread).unwrap();
        assert_ne!(mine, theirs);
    }

    #[test]
    fn each_side_of_a_divergence_sees_its_own_approach_first() {
        let d = &seed::DIVERGENCES[0];
        let (mine, theirs, other) = d.sides(d.a_thread).unwrap();
        assert_eq!(mine, d.a_approach);
        assert_eq!(theirs, d.b_approach);
        assert_eq!(other, d.b_thread);

        let (mine, theirs, other) = d.sides(d.b_thread).unwrap();
        assert_eq!(mine, d.b_approach);
        assert_eq!(theirs, d.a_approach);
        assert_eq!(other, d.a_thread);

        assert!(d.sides("some unrelated thread").is_none());
        // Each side anchors to the block where it stated its approach.
        assert!(d.block_for(d.a_thread).is_some());
        assert!(d.block_for(d.b_thread).is_some());
    }

    #[test]
    fn mentions_are_parsed_out_of_the_body_and_deduplicated() {
        let body = "@franciskafyi can you look, cc @mikayla and @franciskafyi again @nobody";
        assert_eq!(parse_mentions(body), vec![1, 3]);
    }

    /// You cannot @ an agent. Reaching the agent in another thread is a
    /// forward, which is a deliberate send rather than a name in a sentence.
    /// A comment id you can say out loud, scoped to the thread it lives in.
    #[test]
    fn comment_handles_are_readable_and_thread_scoped() {
        let labels = seed::thread_by_title("Prevent Unknown Model Labels").unwrap();
        assert_eq!(thread_slug(labels), "labels");
        // Threads with no hand-written slug still get one.
        let bare = seed::thread_by_title("Explore Running Terminal Icon").unwrap();
        assert_eq!(thread_slug(bare), "explore-running");

        let mut t = thread("Prevent Unknown Model Labels");
        t.comments.clear();
        assert_eq!(comment_handle(&t), "labels-1");
    }

    /// The cheap sweep, which is the only one that runs every turn.
    #[test]
    fn the_sweep_finds_live_threads_sharing_a_file_and_ignores_the_rest() {
        let all: Vec<ThreadUi> = seed::PROJECTS
            .iter()
            .flat_map(|p| p.threads.iter().map(move |t| (p.name, t)))
            .map(|(project, seed_thread)| ThreadUi {
                seed: seed_thread,
                project,
                comments: Vec::new(),
                replies: Vec::new(),
                sent: Vec::new(),
                turn: 1,
                unread: false,
                agent_busy: seed_thread.status == Status::Running,
            })
            .collect();

        let found = observe(&all);
        let files: Vec<&str> = found.iter().map(|o| o.file.as_str()).collect();
        assert!(
            files.contains(&"crates/delta/src/comment_summary.rs"),
            "the v2 pair both have changes in comment_summary.rs, got {files:?}"
        );

        // Every reported file really does have more than one live thread in it.
        for observation in &found {
            assert!(observation.threads.len() > 1);
            for i in &observation.threads {
                assert!(all[*i].agent_busy, "idle threads cannot surprise anyone");
                assert!(all[*i].has_diff());
            }
        }

        // An idle thread in a shared file is not reported.
        let mut parked = all;
        for t in &mut parked {
            t.agent_busy = false;
        }
        assert!(observe(&parked).is_empty());
    }

    /// Dismissal has to survive the wording changing.
    ///
    /// The sentence names the other threads, so a third thread joining the file
    /// rewrites it. Keying on the file means a finding already turned down
    /// stays down, which is the whole point of it being sticky.
    #[test]
    fn a_finding_is_identified_by_its_subject_not_its_sentence() {
        let file = "crates/delta/src/comment_summary.rs".to_string();

        // Two phrasings of the same finding, as the thread list grows.
        let two = "another thread has uncommitted changes in comment_summary.rs: @reply-keys.";
        let three =
            "another thread has uncommitted changes in comment_summary.rs: @reply-keys, @other.";
        assert_ne!(two, three, "the sentence changes when the thread list does");

        let dismissed: Vec<(usize, String)> = vec![(4, file.clone())];
        let suppressed = |thread: usize, subject: &str| {
            dismissed.iter().any(|(t, f)| *t == thread && f == subject)
        };

        // Same subject, either phrasing -> still suppressed.
        assert!(suppressed(4, &file));
        // A different file is a different finding.
        assert!(!suppressed(4, "crates/delta_common/src/model_catalog.rs"));
        // And it is per thread, not global.
        assert!(!suppressed(1, &file));
    }

    #[test]
    fn agents_are_not_mention_targets() {
        let agent = seed::ACTORS.iter().find(|a| a.agent).expect("an agent");
        assert!(parse_mentions(&format!("hey @{}", agent.handle)).is_empty());
    }

    /// Threads are addressable, but only the ones you are in.
    #[test]
    fn you_can_only_address_threads_you_are_in() {
        let mine: Vec<_> = seed::addressable_by(seed::ME).map(|t| t.slug).collect();
        assert!(mine.contains(&"registry-retry"));
        assert!(mine.contains(&"jitter"));

        // as-cii is not in the registry-retry thread, so cannot address it.
        let theirs: Vec<_> = seed::addressable_by(2).map(|t| t.slug).collect();
        assert!(!theirs.contains(&"registry-retry"));
        assert!(theirs.contains(&"wrapping"));

        // Threads with no slug are not addressable by anyone.
        assert!(seed::addressable_by(seed::ME).all(|t| !t.slug.is_empty()));
    }

    #[test]
    fn mention_query_tracks_the_token_under_the_caret() {
        assert_eq!(Shell::mention_query("hey @fran"), Some("fran"));
        assert_eq!(Shell::mention_query("hey @"), Some(""));
        // a completed mention followed by a space is no longer a live query
        assert_eq!(Shell::mention_query("hey @franciskafyi look"), None);
        assert_eq!(Shell::mention_query("no mention here"), None);
    }

    #[test]
    fn the_expertise_query_ranks_the_author_of_the_anchored_range() {
        let mut index = OverlapIndex::new(6);
        for (actor, file, start, end, commits) in seed::BLAME {
            index.history.push(HistoricalTouch {
                author: ActorId(*actor as u32),
                file: file.to_string(),
                lines: *start..*end,
                commits: *commits,
            });
        }
        // lines 79-83 of model_catalog.rs are franciskafyi's doc comment
        let experts = index.experts(seed::MODEL_CATALOG, &(79..84));
        assert_eq!(experts[0].author, ActorId(1));
        assert!(experts[0].reason.contains("commit"));
    }
}

/// The in-process implementation. Everything a network one would also have to
/// do is here and nowhere else: refuse a loop, re-resolve the anchor against
/// the recipient's own content, and let the recipient's state decide whether
/// the comment is armed.
impl ThreadTransport for Shell {
    fn send(&mut self, envelope: Envelope) -> Result<Receipt, SendError> {
        let to = envelope.to.0 as usize;
        let from = envelope.from.0 as usize;
        if to == from {
            return Err(SendError::SameThread);
        }
        if self.threads.get(to).is_none() {
            return Err(SendError::UnknownThread(envelope.to));
        }
        let sender = envelope.sender.0 as usize;
        if !forward_allowed(
            sender,
            Some(envelope.from_title.as_str()),
            self.threads[to].seed.title,
        ) {
            return Err(SendError::WouldLoop);
        }

        let resolution = resolve_in(&self.threads[to], &envelope.anchor);
        let delivery = arrival_delivery(sender, self.threads[to].agent_busy);
        let handle = comment_handle(&self.threads[to]);
        self.next_id += 1;
        let id = self.next_id;
        let from_title = self.threads[from].seed.title;
        self.threads[to].comments.push(UiComment {
            id,
            handle,
            anchor: envelope.anchor,
            resolved: resolution.clone(),
            body: envelope.body,
            author: envelope.author.0 as usize,
            mentions: envelope.mentions.iter().map(|m| m.0 as usize).collect(),
            origin: Provenance::Forwarded {
                from_title,
                forwarded_by: sender,
                original_author: envelope.author.0 as usize,
            },
            delivery,
            divergence: None,
            about: None,
            replies: Vec::new(),
        });
        self.threads[to].unread = true;
        let excerpt: String = {
            let body = &self.threads[to].comments.last().map(|c| c.body.clone()).unwrap_or_default();
            let trimmed: String = body.chars().take(48).collect();
            if body.chars().count() > 48 {
                format!("{trimmed}...")
            } else if trimmed.is_empty() {
                "a snippet".to_string()
            } else {
                trimmed
            }
        };
        self.threads[from].sent.push(SentRecord {
            to,
            comment: id,
            excerpt,
        });
        Ok(Receipt {
            comment: CommentId(id),
            resolution,
            delivery,
        })
    }
}

// ------------------------------------------------------------------ demo ---

/// The script, in the order a person would perform it.
///
/// Written for what someone actually knows at the time. You cannot see another
/// thread's diff, so nothing here quotes their code. What you do know is what
/// *you* are about to change and roughly what they are working on -- which is
/// exactly enough to send a heads-up and not enough to be sure it matters.
/// The v2 script: the orchestrator has already raised something, and you meet
/// it where it landed rather than on a dashboard.
fn demo_script_v2() -> Vec<(u64, Beat)> {
    vec![
        // 1. It was already raised, in a thread, before anyone went looking.
        (300, Beat::Mode(Proactive::Manual)),
        (900, Beat::Thread("Multiplayer Comment Reply Keybinding")),
        (6000, Beat::Pause),
        // 2. The board: raised in both, neither agent told yet.
        (1000, Beat::Channel),
        (4000, Beat::Pause),
        // 3. Handed to auto. One lands at 2s, the other at 10s, so you watch a
        //    single row turn green rather than everything at once.
        (600, Beat::Mode(Proactive::Auto)),
        (6000, Beat::Pause),
        // 4. And in the thread that went green, the agent has it.
        (900, Beat::Thread("Investigate Comment Summary Wrapping")),
        (4500, Beat::Pause),
    ]
}

fn demo_script() -> Vec<(u64, Beat)> {
    let mut beats: Vec<(u64, Beat)> = Vec::new();
    let mut say = |beats: &mut Vec<(u64, Beat)>, text: &str| {
        for ch in text.chars() {
            beats.push((36, Beat::Key(ch)));
        }
    };

    beats.push((700, Beat::Thread("Prevent Unknown Model Labels")));
    beats.push((500, Beat::Pause));

    // 1. a question about what the agent just did, addressed to a person
    beats.push((700, Beat::Select(Target::Prose(4))));
    beats.push((600, Beat::Pause));
    say(&mut beats, "@fran");
    beats.push((1100, Beat::Pause));
    say(
        &mut beats,
        "ciskafyi do we already have an acronym table somewhere, or is this a new one?",
    );
    beats.push((700, Beat::Commit));
    beats.push((2000, Beat::Pause));

    // 2. a heads-up to a thread that is probably in the same file. You do not
    //    know what they wrote and the note does not pretend to.
    beats.push((700, Beat::Select(Target::Diff(2, 14))));
    beats.push((500, Beat::Pause));
    say(
        &mut beats,
        "heads up - this adds a display-name fallback in model_catalog.rs. \
         you are on the divider spacing work, are you in this file too?",
    );
    beats.push((1000, Beat::OpenForward));
    beats.push((1500, Beat::ForwardTo("Fix Compaction Divider Spacing Jitter")));
    beats.push((1600, Beat::Pause));

    // 3. it is waiting over there, re-anchored to that worktree's line numbers
    beats.push((600, Beat::Thread("Fix Compaction Divider Spacing Jitter")));
    beats.push((3000, Beat::Pause));
    beats
}

impl Shell {
    fn start_demo(&mut self, cx: &mut Context<Self>) {
        self.demo = demo_script();
        self.schedule_beat(0, cx);
    }

    fn schedule_beat(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((delay, _)) = self.demo.get(index).copied() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(delay))
                .await;
            this.update(cx, |this, cx| this.play_beat(index, cx)).ok();
        })
        .detach();
    }

    fn play_beat(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some((_, beat)) = self.demo.get(index).copied() else {
            return;
        };
        match beat {
            Beat::Pause => {}
            Beat::Mode(mode) => {
                self.proactive = mode;
                if mode != Proactive::Auto {
                    self.auto_scheduled.clear();
                }
                self.sweep(cx);
            }
            Beat::Channel => {
                self.view = View::Channel;
                self.forwarding = Forwarding::Idle;
            }
            Beat::ApproveProposal => {
                let id = self.threads[self.active]
                    .comments
                    .iter()
                    .find(|c| c.delivery == Delivery::Proposed)
                    .map(|c| c.id);
                if let Some(id) = id {
                    self.resolve_proposal(id, true, cx);
                }
            }
            Beat::Thread(title) => {
                if let Some(i) = self.threads.iter().position(|t| t.seed.title == title) {
                    self.active = i;
                    self.view = View::Thread;
                    self.target = None;
                    self.forwarding = Forwarding::Idle;
                    self.threads[i].unread = false;
                }
            }
            Beat::Select(target) => {
                self.target = Some(target);
                self.forwarding = Forwarding::Idle;
                self.picker_dismissed = false;
                self.composer.update(cx, |c, cx| c.clear(cx));
            }
            Beat::Key(ch) => {
                let mut text = self.composer_text(cx);
                text.push(ch);
                self.set_composer(text, cx);
            }
            Beat::Commit => self.commit(cx),
            Beat::OpenForward => self.forwarding = Forwarding::Selection,
            Beat::ForwardTo(title) => {
                if let Some(i) = self.threads.iter().position(|t| t.seed.title == title) {
                    self.forward_to(Forwarding::Selection, i, cx);
                }
            }
        }
        cx.notify();
        self.schedule_beat(index + 1, cx);
    }
}
