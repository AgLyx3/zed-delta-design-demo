# delta-mock

A GPUI prototype for catching collisions between parallel agent threads while
both are still running, instead of at merge time.

> **This is not a working product.** There is no agent, no git, no network and
> no persistence. Every thread, diff and transcript is hardcoded in
> `src/seed.rs`; agent replies are canned strings; a "turn" is a timer; and the
> model that decides things in `auto` mode is a line-overlap heuristic. State
> resets on relaunch. It exists to make the interactions concrete enough to
> argue about, nothing more.
>
> Two things are real, because faking them would have proved nothing: anchoring
> (`PortableAnchor` genuinely re-resolves against a divergent snapshot) and the
> comment box (a real text field, so IME and paste work).

## The problem

Each agent thread gets its own worktree, so nobody learns they collided until
someone merges — by then both implementations are finished and reconciling them
costs a day for the engineering team.

## Roadmap

### V1 — manual *(shipped)*

- Highlight a paragraph or a run of diff lines; a comment box opens inline.
- `@` a person — the list appears only once you type `@`.
- Forward the highlighted span plus a note to another thread; it re-anchors against that thread's worktree.
- Reply to any comment, in the same box.

> *[Not exhaustive.]*

### V2 — agent proactive *(shipped, behind a toggle)*

- **Signals**: a board above all threads, showing which live threads share a file.
- Findings are raised into each thread involved, not left on a dashboard.
- `manual` / `auto` — a person decides whether their agent sees a finding, or a model does.
- Nothing lands mid-turn; a release waits for a turn boundary.

> *[Not exhaustive.]*

### Beyond

The split is the roadmap: **V1 is anything a person does by hand, V2 is the
agent doing it proactively.** Every V1 affordance has a V2 counterpart, and
everything not yet built lands in one column or the other.

## Demos

Both are self-driving — the app drives its own state and types into its own
composer, so nothing depends on synthetic keystrokes.

### V1 — [`media/v1-comment-and-forward.mp4`](media/v1-comment-and-forward.mp4)

| | on screen |
|---|---|
| 1 | **Conversation pane.** A paragraph of the agent's own output is highlighted; the box opens inline under it. `@fran` drops the mention list; it completes to `@franciskafyi`. Enter commits, and agent output continues below — a note on existing output, not a new prompt. |
| 2 | **Diff pane.** Same box, inline against the highlighted lines. The note only claims what the sender can know: which file *they* touch, and roughly what the other thread is doing. |
| 3 | **Forward to thread** — a separate button, not a row in the mention list — then the thread picker. |
| 4 | **Recipient thread.** Re-anchored to its own numbering (`79-91` → `64-76`), `re-anchored 80%` badge, provenance, mention chip intact. |

### V2 — [`media/v2-proactive-signals.mp4`](media/v2-proactive-signals.mp4)

| | on screen |
|---|---|
| 1 | **A thread already flagged.** Nobody went looking; the finding sits there marked `agent hasn't seen this`, with `show agent` / `ignore`. Hollow ring = nobody authored it. |
| 2 | **Signals.** The same finding raised in *both* threads in that file, both `not notified`. The board reports only whether each agent was told. |
| 3 | **Flipped to auto.** Releases are staggered, so one row turns green while the other stays grey — a decision landing per thread, not a page refresh. |
| 4 | **The green thread.** Its agent has the finding; it is now an ordinary comment. |

Re-record: `./demo.sh --record out.mov 30` (v1), `./demo.sh v2 --record out.mov 30` (v2).
Keep the window frontmost for the whole take; seeded threads stay live ~4 minutes.

## Setup

Depends on Zed by path, pinned to one commit, cloned as a sibling:

```sh
git clone --depth 1 --filter=blob:none https://github.com/zed-industries/zed.git
git -C zed checkout 91bf967e279fba3b326c096aeb66053cb2373547
```

```
Zed-redesign/
  zed/          <- the clone
  delta-mock/   <- this repo
```

Rust 1.97.1, pinned in `rust-toolchain.toml`. Two settings are deliberate:
`runtime_shaders` (building shaders needs Xcode's Metal Toolchain, not assumed
here) and the hand-rolled `ThemeSettingsProvider` in `src/theme_setup.rs` (so
Zed's `ui` crate works without the `settings` crate).

## Running

```sh
./bundle.sh    # wrap in a .app so it gets focus, then open
./demo.sh      # v1 demo          ./demo.sh v2   # v2 demo
cargo test     # 30 tests, no window needed
```

Under `open` there is no readable stderr, so panics append to
`~/Library/Logs/delta-mock-panic.log` with a backtrace.

## Layout

| | |
|---|---|
| `src/shell.rs` | the three panes and every interaction |
| `src/composer.rs` | the comment box — a real text field on `EntityInputHandler`, so IME, paste and selection work |
| `src/model/anchor.rs` | `PortableAnchor` — resolves against a *divergent* snapshot, degrading Exact / Shifted / Orphaned |
| `src/model/transport.rs` | the seam a backend plugs into: `Envelope`, `Receipt`, `ThreadTransport` |
| `src/model/overlap.rs` | one index, two questions: who is editing this now, who edited it before |
| `src/seed.rs` | all content. Two threads share a file with divergent worktrees, or the anchors would have nothing to prove |
