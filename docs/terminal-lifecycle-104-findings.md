# Terminal lifecycle: findings for upstream issue #104

An empirical review of the terminal cleanup problem described in
[ratatui/bevy_ratatui#104][issue-104], and of the four implementations that
exist for it: upstream PR [#107][pr-107] and fork PRs
[#2][pr-2], [#4][pr-4], and [#5][pr-5].

Everything in the result tables was measured, not inferred. The method is in
[Method](#method) and the probe sources are in [Appendix A](#appendix-a-probe-sources)
so any claim here can be re-run.

[issue-104]: https://github.com/ratatui/bevy_ratatui/issues/104
[pr-107]: https://github.com/ratatui/bevy_ratatui/pull/107
[pr-2]: https://github.com/gold-silver-copper/bevy_ratatui/pull/2
[pr-4]: https://github.com/gold-silver-copper/bevy_ratatui/pull/4
[pr-5]: https://github.com/gold-silver-copper/bevy_ratatui/pull/5

---

## Summary

Issue #104 contains **two** distinct defects. Most discussion covers only the first.

| # | Defect | Fixed by |
| --- | --- | --- |
| 1 | The terminal is restored twice, so the second `LeaveAlternateScreen` moves the cursor back over the panic output | #107, PR 2 (partially), PR 4, PR 5 |
| 2 | Optional modes (kitty, mouse) are torn down in an unordered way relative to the alternate screen during unwinding | PR 2, PR 4, PR 5 — **not** #107 |

Additional findings:

- **PR 2 does not fully fix defect 1.** Its guard is `thread::panicking()`, which is
  thread-local unwinding state rather than an ownership fact. A panic on a thread that
  does not terminate the app still produces the #104 double-restore. Measured.
- **PR 2 regresses direct `RatatuiContext::init()`**, leaving the terminal in raw mode
  and on the alternate screen with no diagnostic. Measured.
- **PR 2 and PR 4 silently destroy a panic hook installed after startup.** Measured.
- **#107 latches a global flag on failed initialisation**, after which every later
  `init()` in the process fails with a misleading error. Measured.
- **PR 4 removes every public path to restore the terminal mid-run**, so
  suspend/resume (shelling out to `$EDITOR`) becomes impossible.
- Contrary to a common reading, **`TerminalContext` was never a usable extension
  point** — see [API surface](#api-surface-and-capability-analysis).

---

## Root cause

From the issue author's own diagnosis, confirmed here:

1. `bevy_ratatui` restores the terminal twice on the panic path. The panic hook
   restores it (so the panic can print outside the alternate screen), then unwinding
   drops `RatatuiContext` whose `Drop` restores it again.
2. crossterm's `LeaveAlternateScreen` emits `CSI ? 1049 l`, which per the xterm
   control-sequence documentation is "Use Normal Screen Buffer and restore cursor as
   in DECRC". Many emulators implement that literally as two effects.
3. Switching to the normal buffer when already on it is a no-op, but the *cursor
   restore* is not. The saved cursor position is only updated when entering the
   alternate screen, so a second `1049l` — issued after the panic text has been
   printed — moves the cursor back to where it was before the app started. The shell
   then prints its prompt over the middle of the backtrace.

The enabling condition in the code is the signature:

```rust
fn restore() -> Result<()>
```

A free function that restores "the terminal" with no reference to what is being
restored or who owns it. Nothing in the type system prevents two callers, so two
callers is the default outcome rather than a bug that has to be introduced.

The `main` behaviour compounds this: the panic hook installed by `ErrorPlugin` is
never uninstalled, so a panic long after the `App` has been dropped still emits
terminal escapes:

```
USER-HOOK-INSTALLED
\x1b[?1049h ... \x1b[?1049l \x1b[?25h        <- app runs and exits cleanly
APP-DROPPED
USER-HOOK-RAN
\x1b[?1049l \x1b[?25h                        <- stale hook, app is long gone
thread 'main' panicked at examples/probe3.rs:24:36:
post-app-panic
```

---

## Method

Each measurement runs a probe binary under a real pseudo-terminal (`pty.openpty`)
and asserts on the raw byte stream. The probe sources compile unchanged against
`main`, #107, PR 2, PR 4, and PR 5, so the comparison is like-for-like.

Sequences of interest:

| Bytes | Meaning |
| --- | --- |
| `ESC [ ? 1049 h` | enter alternate screen |
| `ESC [ ? 1049 l` | leave alternate screen (**carries an implicit DECRC**) |
| `ESC [ ? 25 h` | show cursor (does not move it) |
| `ESC 8` / `ESC [ u` | DECRC / SCORC — explicit cursor restore |
| `ESC [ ? u ESC [ c` | crossterm's kitty capability query |
| `ESC [ >` … | push kitty enhancement flags |
| `ESC [ < 1 u` | pop kitty enhancement flags |

Trees measured:

| Label | Ref | Base |
| --- | --- | --- |
| `main` | `ratatui/bevy_ratatui@5a17d83` | — |
| `#107` | `refs/pull/107/head` (`2b82caf`) | `0d413bc`, **4 commits behind main** |
| `PR 2` | `agent/simplify-terminal-lifecycle` | `5a17d83` |
| `PR 4` | `agent/minimal-terminal-lifecycle` | `5a17d83` |
| `PR 5` | `agent/terminal-session` | `5a17d83` |

> #107 branches from `0d413bc` (before the Bevy 0.19 bump `4ce26b6` and before
> `5b42501` "Don't restore the terminal in the eyre hook"). It still merges cleanly
> and current `main`'s hook still calls `RatatuiContext::restore()`, so the approach
> still applies — but its CI has never run and its panic output format differs from
> current `main` because its base still used `color_eyre` in `ErrorPlugin`.

---

## Results

### Probe 1 — the reported reproduction

Panic in `Update`, `RUST_BACKTRACE=1`. Counts `1049l`, and checks whether anything
that moves the cursor appears *after* the panic diagnostic.

| | enter | leave | cursor-mover after diagnostic | |
| --- | --- | --- | --- | --- |
| `main` | 1 | **2** | **yes (`1049l`)** | ❌ reproduces #104 |
| `#107` | 1 | 1 | no | ✅ |
| PR 2 | 1 | 1 | no | ✅ |
| PR 4 | 1 | 1 | no | ✅ |
| PR 5 | 1 | 1 | no | ✅ |

All four implementations fix the reported symptom.

> The diagnostic marker differs by tree: #107's base still routed panics through
> `color_eyre` in `ErrorPlugin`, so it prints `The application panicked (crashed).`
> rather than `panicked at`. Its row was verified against that marker. In every
> passing tree the only sequence emitted after the diagnostic is `ESC [ ? 25 h`,
> which shows the cursor without moving it.

### Probe 2 — a panic that does *not* terminate the app

A `std::thread` panics and is joined; the app then exits normally through `AppExit`.
The panic hook restores the terminal, then normal teardown restores it again.

| | leave-alt count | |
| --- | --- | --- |
| `main` | **2** | ❌ |
| `#107` | 1 | ✅ |
| PR 2 | **2** | ❌ **still reproduces #104** |
| PR 4 | 1 | ✅ |
| PR 5 | 1 | ✅ |

This is the sharpest differentiator. PR 2 guards its cleanup with
`thread::panicking()`, which is true only while unwinding *on the thread that is
dropping the resource*. #107 (a process-global `AtomicBool`), PR 4 (an owned
`AtomicBool` token) and PR 5 (a `Mutex<SessionState>` transitioned before any I/O)
all express exactly-once as a real invariant and hold.

### Probe 3 — a user panic hook installed after startup

A `Startup` system installs a hook wrapping the current one. After the app is torn
down, an unrelated panic is raised: does the user's hook still run?

| | hook survives teardown | |
| --- | --- | --- |
| `main` | yes | (only because the session hook is *never* uninstalled — a leak, not a feature) |
| `#107` | yes | same leak; unchanged from `main` |
| PR 2 | **no** | ❌ blindly `take_hook()` + `set_hook(previous)` |
| PR 4 | **no** | ❌ same |
| PR 5 | yes | ✅ compares the installed hook's identity before reinstating |

### Probe 4 — direct `RatatuiContext::init()`

Construct a context outside the plugin, draw, drop it.

| | restores on drop | raw mode left on | |
| --- | --- | --- | --- |
| `main` | yes | no | ✅ |
| `#107` | yes | no | ✅ |
| PR 2 | **no** | **yes** | ❌ silent regression |
| PR 4 | yes | no | ✅ |
| PR 5 | yes | no | ✅ |

PR 2 removes `Drop for RatatuiContext` and adds no replacement to
`CrosstermContext`, so the plugin-free path leaks terminal state entirely. Its
documentation says the caller must call `restore()`, but this is a silent behaviour
change for existing callers of a public constructor.

### Probe 5 — when does restoration happen?

All four restore **during** `App::run()`, before it returns. Bevy's `App::run` moves
the `App` out of `&mut self`, so the `World` drops inside `run()`. Removing
`CleanupPlugin` (which all three fork PRs do) therefore does **not** delay
restoration to an arbitrary later point, as one might expect. No regression here.

### Probe 6 — cleanup ordering (the second, mostly-unfixed defect)

`RatatuiPlugins::default()` (kitty enabled), with a PTY that answers crossterm's
capability query, then a panic. Order of the emitted sequences:

```
main    enter-alt → push-kitty → LEAVE-alt ×2 → panic text → pop-kitty     ❌
#107    enter-alt → push-kitty → LEAVE-alt ×1 → panic text → pop-kitty     ❌
PR 2    enter-alt → push-kitty → pop-kitty → LEAVE-alt → panic text        ✅
PR 4    enter-alt → push-kitty → pop-kitty → LEAVE-alt → panic text        ✅
PR 5    enter-alt → push-kitty → pop-kitty → LEAVE-alt → panic text        ✅
```

**#107 fixes the duplicate leave and leaves the ordering defect untouched.** The
kitty enhancement flags are still popped after the alternate screen is gone and
after the panic text has printed.

This is precisely what `CleanupPlugin`'s own doc comment exists to prevent:

> If raw mode, the alternate view, and the Kitty protocol are disabled in the wrong
> order, it can cause issues for the terminal buffer after the application exits.

and precisely what the issue author flagged as unguaranteed on the panic path:

> Mouse capture and the Kitty protocol are also disabled during unwinding by their
> respective destructors, but nothing controls the *order* in which this happens.

The documented rationale is that the kitty enhancement-flag stack is maintained
per screen buffer (main vs. alternate), so pushing on the alternate screen and
popping after returning to the main screen operates on the wrong stack.

> **Scope of this claim:** the byte ordering above was measured directly. The
> emulator-level consequence was not — it depends on per-terminal implementation of
> the same under-specified `CSI ?` extensions that produced the original ambiguity in
> #104.

### Probe 7 — #107 latches its flag on failed initialisation

```rust
if TERMINAL_INITIALIZED.swap(true, Ordering::Relaxed) {
    return Err("Only one CrosstermContext can exist at a time".into());
}
stdout.execute(EnterAlternateScreen)?;   // any of these `?` leaves the flag latched
enable_raw_mode()?;
let terminal = Terminal::new(backend)?;
```

Run with stdout not a TTY, so `enable_raw_mode()` fails after the swap:

```
[?1049h  ATTEMPT-1 failed: Device not configured (os error 6)
         ATTEMPT-2 failed: Only one CrosstermContext can exist at a time
```

The first attempt writes the alternate-screen escape, fails, and leaves the flag
`true` with no owner. Every subsequent `init()` in the process then fails with a
misleading error, permanently. A rollback on the error path is a few lines and is
the main thing worth adding to #107 before it merges.

All three fork PRs handle partial-initialisation rollback explicitly.

### Probe 8 — suspend and resume (shell out to `$EDITOR`)

| | public path to restore mid-run | |
| --- | --- | --- |
| `main` | `RatatuiContext::restore()` | works by accident; the context still believes it owns the terminal, so re-`init()` leaves two owners both restoring at drop |
| `#107` | `RatatuiContext::restore()` | flag makes it safe, but the context is now stale |
| PR 2 | `RatatuiContext::restore()` | the private `TerminalSession` still restores at exit → extra `1049l` |
| PR 4 | **none** | `restore()` removed, cleanup token moved into a private resource, and dropping `RatatuiContext` no longer restores |
| PR 5 | `session.close()` | ✅ releases the lease; re-`init()` re-acquires |

Verified on PR 5: remove the resource, `close()`, run an external command,
`RatatuiContext::init()` again. Result is a balanced 2 enter / 2 leave and a clean
exit.

---

## Size analysis

Lines of `src/`, split by kind. "logic" excludes doc comments and `#[cfg(test)]`
modules.

| | logic | docs | unit tests | integration tests | test share of delta |
| --- | ---: | ---: | ---: | ---: | ---: |
| `main` | 1384 | 391 | 53 | 0 | — |
| `#107` | +9 | +3 | 0 | 0 | — |
| PR 2 | +203 | +7 | +56 | +395 | 68% |
| PR 4 | +298 | −4 | +870 | +1104 | 87% |
| PR 5 | +475 | +148 | +1065 | +1318 | 79% |

The headline diffs (+854 / +2547 / +3477) overstate the logic delta by 4–5×. On
logic alone the ratio to #107 is roughly 22× / 33× / 53×.

Why #107 is so much smaller: it fixes the *symptom* (make terminal restoration
idempotent) and changes no structure. `Drop for RatatuiContext`,
`Drop for KittyEnabled`, `Drop for MouseEnabled`, `CleanupPlugin`, and the
never-uninstalled panic hook all remain. The fork PRs took the issue author's
follow-up comment — that the cleanup approach "is already somewhat confused and
having the `Drop` implementations isn't really helping" — as the specification,
which is a redesign brief rather than a bug report.

---

## API surface and capability analysis

### Correction: `TerminalContext` was never an extension point

It is natural to read the trait as the crate's backend extension mechanism. It is
not, and this matters when judging PR 5's removal of it:

- It has exactly two implementations, both in-crate.
- Nothing in the crate is generic over it.
- `DefaultContext` is a `#[cfg]`-selected **type alias**, and
  `RatatuiContext(pub DefaultContext)` is hardwired to it.

A third-party implementation can therefore never reach `RatatuiContext`,
`ContextPlugin`, or `RatatuiPlugins`. The trait is shared vocabulary for two types
that share no code path. Its `restore()` being a *static* method is also where the
bug lives — that signature is what makes double-restore expressible.

### What each implementation removes

| Public item | `main` | #107 | PR 2 | PR 4 | PR 5 |
| --- | :---: | :---: | :---: | :---: | :---: |
| `cleanup::CleanupPlugin` | ✔ | ✔ | ✖ | ✖ | ✖ |
| `error::ErrorPlugin` | ✔ | ✔ | ✖ | ✖ | ✖ |
| `context::TerminalContext` | ✔ | ✔ | ✔ | ✔ (no `restore`) | ✖ |
| `context::CrosstermContext` | ✔ | ✔ | ✔ | ✔ | ✖ (→ `CrosstermSession`) |
| `kitty::KittyPlugin` | ✔ | ✔ | ✔ | ✔ | ✖ |
| `mouse::MousePlugin` | ✔ | ✔ | ✔ | ✔ | ✖ |
| `RatatuiContext::restore()` | ✔ | ✔ | ✔ | ✖ | ✖ (→ `close()`) |
| `kitty::KittyEnabled`, `mouse::MouseEnabled` | ✔ | ✔ | ✔ | ✔ | ✔ |

### Capabilities gained and lost

Counter-intuitively, PR 5 deletes the most type names while preserving the most
capability:

| Capability | `main` | PR 2 | PR 4 | PR 5 |
| --- | :---: | :---: | :---: | :---: |
| Opt out of the panic hook | `.disable::<ErrorPlugin>()` | **none** | **none** | `SessionOptions { panic_hook: false }` |
| Third-party plugin can request mouse/kitty | add `MousePlugin` | ✔ (writes `CrosstermSettings`) | ✔ | **none** — fixed at `ContextPlugin` construction |
| Restore mid-run (suspend/resume) | footgun | footgun | **none** | `close()` |
| Order startup systems around context setup | `.after(context_setup)` | custom `TerminalStartup` schedule | `ContextSetup` set | `ContextSetup` set |

The one deletion in PR 5 that is **not** load-bearing for the fix is
`KittyPlugin`/`MousePlugin`. PR 2 and PR 4 keep both as thin configuration markers
that write into a `CrosstermSettings` resource read at acquisition time, which
preserves the ability for a downstream crate to request a mode. PR 5's
`SessionOptions` are fixed when `ContextPlugin` is constructed, so only the
application author can enable them.

---

## Recommendations

### For upstream

1. **Merge #107**, after adding a rollback so a failed `init()` does not latch the
   flag, plus a regression test. It is small, already approved, and correctly fixes
   the reported symptom.
2. **Follow up with the ordering fix** (probe 6) as a separate, focused PR. #107 does
   not move any of the code that a cleanup-ordering fix would restructure, so the two
   compose cleanly.
3. **Adopt a PTY escape-stream test fixture** as core infrastructure. Every defect in
   this document is an ordering property of a byte stream that `cargo test` cannot
   see. PR 2 passes its own six integration tests and still reproduces #104.

### For the fork PRs

- PR 2 should not be merged as a fix for #104: probe 2 shows it does not fix the bug
  in general, and probe 4 shows it regresses the plugin-free path.
- PR 4's core (`TerminalCleanup`) is correct. Before merging: fix the panic-hook
  clobbering (probe 3), restore a public mid-run restore path (probe 8), and drop the
  100-iteration CI stress loop.
- PR 5 has the strongest correctness story. Before merging: keep `KittyPlugin` /
  `MousePlugin` as configuration markers, add a deprecated
  `pub type CrosstermContext = CrosstermSession;` alias for one release, and justify
  the `TerminalContext` removal in the PR body on the grounds above — as written, the
  deletion reads as unmotivated.

### If the crate were designed again

The defect class disappears under a handful of rules:

1. **Release is consuming.** No free-function `restore()` anywhere in the API. Calling
   it twice becomes a borrow-check error rather than a runtime invariant.
2. **Nobody else can write the escape sequences.** Put every state-mutating crossterm
   call behind a private module only the session type can reach. "Some other code
   restored the terminal" becomes a privacy question the compiler answers.
3. **Ordering enforced by the compiler.** Have each capability borrow the one it
   depends on (`AlternateScreen::enter(&raw)`, `KittyKeyboard::push(&alt)`), so
   dropping the alternate screen while kitty flags are live does not compile. This
   conflicts with `'static` ECS storage, which is the real reason PR 5 pays for
   ordering by hand; a scoped `run`-closure API resolves it.
4. **ECS resources are observations, not owners.** Never put I/O in the `Drop` of
   something the ECS owns — Bevy does not specify resource drop order, and any system
   with `Commands` can trigger it.
5. **The panic hook collects; it never acts.** Every implementation reviewed here
   makes the hook a second restorer, which is what forces the coordination machinery
   (PR 4's token, PR 5's mutex + lease). Invert it: install a hook that records the
   panic report and touches nothing, `catch_unwind` at the runner boundary, drop the
   session (the only restore, on both paths), print the captured report, then
   `resume_unwind`. One call site, nothing to coordinate. As a bonus, an off-thread
   panic no longer tears down a running TUI.
6. **Own the entry point.** `ratatui::run` does not exhibit this bug because it owns
   its boundary; `RatatuiPlugins` merely hopes the `App` is dropped.
7. **Make the failure benign anyway.** Destructors do not run under `panic = "abort"`,
   `process::exit`, `SIGKILL`, or an unhandled `SIGTERM`. Do not depend on `1049l`'s
   implicit DECRC — emit the leave, then position the cursor explicitly, so a
   redundant restore is harmless rather than corrupting.

---

## Appendix A: probe sources

Each probe is a Cargo example that compiles unchanged against all five trees, run
under a Python PTY driver that captures the raw byte stream.

### Probe 1 — reported reproduction

```rust
use std::time::Duration;
use bevy::{app::ScheduleRunnerPlugin, prelude::*};
use bevy_ratatui::{RatatuiContext, RatatuiPlugins};
use ratatui::text::Text;

fn main() {
    App::new()
        .add_plugins((
            MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(Duration::from_millis(16))),
            RatatuiPlugins {
                enable_kitty_protocol: false,
                enable_mouse_capture: false,
                enable_input_forwarding: false,
            },
        ))
        .add_systems(Update, (draw_system, panic_system).chain())
        .run();
}

fn draw_system(mut context: ResMut<RatatuiContext>) -> Result {
    context.draw(|frame| {
        frame.render_widget(Text::raw("issue 104 probe"), frame.area());
    })?;
    Ok(())
}

fn panic_system() {
    panic!("issue-104-probe-panic");
}
```

### Probe 2 — panic that does not terminate the app

```rust
fn off_thread_panic() {
    let handle = std::thread::spawn(|| panic!("off-thread-probe-panic"));
    let _ = handle.join();
    eprintln!("OFF-THREAD-PANIC-JOINED");
}

fn tick(
    mut context: ResMut<RatatuiContext>,
    mut frames: ResMut<Frames>,
    mut exit: MessageWriter<AppExit>,
) {
    frames.0 += 1;
    let _ = context.draw(|frame| {
        frame.render_widget(Text::raw("probe2"), frame.area());
    });
    if frames.0 >= 3 {
        exit.write_default();
    }
}
```

`off_thread_panic` runs in `Startup`; the app then exits normally.

### Probe 3 — user panic hook installed after startup

```rust
fn install_user_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        eprintln!("USER-HOOK-RAN");
        previous(info);
    }));
    eprintln!("USER-HOOK-INSTALLED");
}

// main: run the app in a scope, then after it is dropped:
//   let _ = panic::catch_unwind(|| panic!("post-app-panic"));
```

### Probe 4 — direct `RatatuiContext::init()`

```rust
fn main() {
    {
        let mut context = RatatuiContext::init().expect("init");
        let _ = context.draw(|frame| {
            frame.render_widget(Text::raw("probe4"), frame.area());
        });
    } // dropped here
    eprintln!("CONTEXT-DROPPED");
    let raw = ratatui::crossterm::terminal::is_raw_mode_enabled().unwrap();
    eprintln!("RAW-MODE-STILL-ON={raw}");
}
```

### Probe 6 — cleanup ordering

Same as probe 1 but with `RatatuiPlugins::default()` (kitty enabled), driven by a
PTY that answers crossterm's capability query:

```python
QUERY = b"\x1b[?u\x1b[c"        # crossterm's supports_keyboard_enhancement probe
REPLY = b"\x1b[?1u\x1b[?1;2c"   # "yes, kitty protocol supported"
# ... on seeing QUERY in the stream, write REPLY back to the master fd
```

### PTY driver

```python
import os, pty, select, subprocess, sys

master, slave = pty.openpty()
env = dict(os.environ)
env["RUST_BACKTRACE"] = "1"
p = subprocess.Popen([sys.argv[1]], stdin=slave, stdout=slave, stderr=slave,
                     env=env, close_fds=True)
os.close(slave)
out = b""
while True:
    r, _, _ = select.select([master], [], [], 15)
    if not r:
        break
    try:
        chunk = os.read(master, 65536)
    except OSError:          # EIO when the child closes the slave side
        break
    if not chunk:
        break
    out += chunk
p.wait()
os.close(master)

LEAVE = b"\x1b[?1049l"
print("leave-alt count:", out.count(LEAVE))
i = out.find(b"panicked at")
if i >= 0:
    tail = out[i:]
    for seq in (LEAVE, b"\x1b[?1049h", b"\x1b8", b"\x1b[u"):
        if seq in tail:
            print("cursor-moving sequence after the diagnostic:", seq)
```

## Appendix B: reproducing the whole matrix

```bash
# one worktree per tree under test
git worktree add /tmp/wt-main            origin/main
git worktree add /tmp/wt-pr2             fork/agent/simplify-terminal-lifecycle
git worktree add /tmp/wt-pr4             fork/agent/minimal-terminal-lifecycle
git worktree add /tmp/wt-pr5             fork/agent/terminal-session
gh pr checkout 107 --repo ratatui/bevy_ratatui --branch pr107
git worktree add /tmp/wt-pr107           pr107

# copy the probes from Appendix A into each worktree's examples/, then
for d in /tmp/wt-*; do (cd "$d" && cargo build --example issue104_probe); done
for d in /tmp/wt-*; do
  echo "== $d =="
  python3 drive.py "$d/target/debug/examples/issue104_probe"
done
```
