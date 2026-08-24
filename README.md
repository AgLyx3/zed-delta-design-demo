# delta-mock

A GPUI prototype exploring how parallel agent threads could catch each other's
work **before** it becomes a merge problem.

---

## The problem

When several agent threads work at once, each in its own worktree, nobody finds
out they collided until somebody merges. By then the cheap fix is gone: two
implementations exist, both are finished, and someone spends a day reconciling
them.

The expensive part is not the textual conflict. Git announces those reliably, at
merge time, and they take minutes. What costs days is the pair that merges
**cleanly** and is still wrong — two threads that solved the same problem in
different places, or made the same decision differently. Nothing flags those,
because there is nothing to flag: the diffs do not overlap.

So the question this prototype is built around is:

> How early can two threads find out about each other, and what is the smallest
> interaction that makes that useful rather than noisy?

Everything here follows from that. Findings are raised while both threads are
still running, not at merge; they are anchored to the code they are about, so
they survive a rebase; and every proactive path is gated, because a warning
nobody can dismiss is a warning everybody turns off.

---

## Roadmap

### V1 — people communicating *(shipped, in this build)*

The human loop. Comment on what an agent wrote, address a person, hand a
snippet to another thread.

- Highlight anything — a paragraph of the agent's output, or lines in the diff —
  and a comment box opens inline where you highlighted.
- `@` a person. The list appears only once you type `@`.
- **Forward** the highlighted span plus your note to another thread. It arrives
  re-anchored against *that* thread's worktree.
- Reply to anyone's comment in the same box.

> **[Not exhaustive.]** Also in scope for V1 and not built: resolving a comment
> thread, editing or deleting your own comment, per-thread notification
> preferences, and any real multiplayer presence.

### V2 — the workspace noticing *(shipped behind a toggle)*

The same surfaces, driven by something that can see across threads. A thread's
own agent cannot do this: it runs in its own context and has no idea another
thread exists. Detection has to sit above them.

- **Signals** — a board above all threads. Currently: which live threads have
  uncommitted changes in the same file.
- Findings are **raised into each thread involved**, not left on a dashboard.
- **`manual` / `auto`** modes, like a permission setting. Manual means a person
  decides whether their agent sees a finding. Auto means a model decides, and
  holds what it is unsure about.
- Nothing is spliced into a prompt already in flight — a release lands at a turn
  boundary.

> **[Not exhaustive.]** The interesting detection is not built: comparing what
> each thread *said it would do*, which is the only way to catch two threads
> solving one problem in different files. Also missing: symbol-level rather than
> file-level overlap, alerts to threads that only *call* a changed interface,
> and a place to record what was actually decided.

### Beyond

Routing to the right *person* rather than the right thread — the expertise
lookup exists in `model/overlap.rs` and is unused by the UI. Blame answers "who
wrote this"; it will never surface a designer or a PM, so that wants a second
lookup, not a bigger one.

---

## Demos

Both are scripted and self-driving — `DELTA_MOCK_DEMO` makes the app drive its
own state and type into its own composer, so nothing depends on synthetic
keystrokes.

### V1 — [`media/v1-comment-and-forward.mp4`](media/v1-comment-and-forward.mp4)

| beat | what you are looking at |
|---|---|
| 1 | The **conversation pane**. A paragraph of the agent's own output is highlighted and a comment box opens inline underneath it. `@fran` is typed and the mention list drops out of the box; it completes to `@franciskafyi`. Enter commits. The comment lands mid-transcript with agent output continuing below — it is a note on existing output, not a new prompt. |
| 2 | The **diff pane**, right. A block of the change is highlighted and the same box opens inline there. The note is a heads-up written from what the sender can actually know: which file *they* are touching, and roughly what the other thread is working on. It never claims to know the other thread's code. |
| 3 | **Forward to thread** — a separate button, not a row in the mention list — and the thread picker. |
| 4 | The **recipient thread**. The comment arrived re-anchored to *its* line numbers (`79-91` became `64-76`), with a `re-anchored 80%` badge, provenance naming who sent it and where from, and the mention chip intact. |

### V2 — [`media/v2-proactive-signals.mp4`](media/v2-proactive-signals.mp4)

| beat | what you are looking at |
|---|---|
| 1 | A **thread that was already flagged**. Nobody went looking for this — the finding is sitting in the thread, marked `agent hasn't seen this`, with `show agent` / `ignore`. The hollow ring means nobody authored it. |
| 2 | **Signals**, the board. The same finding, raised in *both* threads that are in that file, both `not notified`. The board reports whether each agent has been told and nothing else. |
| 3 | The mode flips to **auto**. Releases are staggered, so one row turns green while the other stays grey — you are watching a decision land per thread, not a page refresh. |
| 4 | The **thread that went green**. Its agent has the finding now; it is an ordinary comment in the thread. |

Re-record either:

```sh
./demo.sh --record out.mov 30       # v1
./demo.sh v2 --record out.mov 30    # v2
```

The window has to stay frontmost for the whole take. Seeded threads only stay
"live" for about four minutes after launch, and the V2 board is empty once they
go idle — which is correct, but means a late start records nothing.

---

## Setup

The crate depends on Zed by path, pinned to one upstream commit. It is not
vendored, so clone it as a sibling:

```sh
git clone --depth 1 --filter=blob:none https://github.com/zed-industries/zed.git
git -C zed checkout 91bf967e279fba3b326c096aeb66053cb2373547
```

Layout the build expects:

```
Zed-redesign/
  zed/          <- the clone above
  delta-mock/   <- this crate
```

Rust 1.97.1, pinned in `rust-toolchain.toml`.

Two settings are deliberate and should not be "fixed":

- `gpui_platform` is built with `runtime_shaders`, because compiling shaders at
  build time needs Xcode's Metal Toolchain, which is not assumed here.
- `src/theme_setup.rs` hand-rolls a `ThemeSettingsProvider` so Zed's `ui` crate
  works without pulling in the `settings` crate.

## Running

```sh
cargo run                 # bare binary; can open behind other windows
./bundle.sh               # wrap in a .app so it gets focus and cmd-tab, then open
./demo.sh                 # play the v1 demo
./demo.sh v2              # play the v2 demo
cargo test                # 30 tests, no window needed
```

Launched through `open`, the app has no stderr anyone can read, so panics are
appended to `~/Library/Logs/delta-mock-panic.log` with a full backtrace.

## Layout

| | |
|---|---|
| `src/shell.rs` | the three panes, and every interaction |
| `src/composer.rs` | the comment box — a real text field on `EntityInputHandler`, so IME, paste and selection work |
| `src/model/anchor.rs` | `PortableAnchor` — resolves against a *divergent* snapshot, degrading to Exact / Shifted / Orphaned |
| `src/model/transport.rs` | the seam a real backend plugs into: `Envelope`, `Receipt`, `ThreadTransport` |
| `src/model/overlap.rs` | one index, two questions: who is editing this now (conflict), who edited it before (expertise) |
| `src/seed.rs` | all content. Two threads deliberately share a file with divergent worktrees, or the anchors would have nothing to prove |
