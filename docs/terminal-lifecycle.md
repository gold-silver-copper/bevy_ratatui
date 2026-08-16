# Terminal lifecycle contract and test matrix

This document describes the lifecycle guaranteed by the Crossterm backend and maps each guarantee
to deterministic tests, real pseudo-terminal (PTY) tests, and CI jobs. The implementation tracks
capabilities independently rather than assuming setup is one indivisible operation.

## Contract

The lifecycle moves through these conceptual states:

1. Dormant: no state is owned.
2. Raw acquired.
3. Alternate screen possibly entered.
4. Optional mouse capture possibly enabled.
5. Optional kitty keyboard mode possibly enabled.
6. The previous panic hook is captured and the session hook is installed.
7. Active: the Bevy world owns the context and session; the panic hook shares the cleanup claim.
8. Cleaning: one caller atomically claims cleanup.
9. Cleaned: all acquired capabilities have had their inverse operation attempted and the old hook is
   restored on non-panicking teardown.

Initialization is transactional. A query or raw-mode enable failure changes nothing. Alternate
screen commands can fail after writing but before flushing, so an alternate-screen error still
attempts `LeaveAlternateScreen`. A failure after an optional mode command similarly attempts that
mode's inverse. Every rollback operation is attempted even if an earlier rollback operation fails.

Cleanup ownership begins in a directly initialized `CrosstermContext`. The plugin moves the unique
cleanup token into its session, then shares that token with the panic hook. The token uses one atomic
`swap`; the winning direct-context, app-session, or hook path performs cleanup and every later path is
a no-op. An error or panic from the winning cleanup consumes the claim rather than allowing a second
owner to replay terminal commands over diagnostic output.

The cleanup order is kitty pop, mouse disable when enabled, raw-mode disable, alternate-screen
leave, and cursor show. All applicable operations run and the first error is reported. `Drop`
reports an error best-effort and never panics because an operation returned `Err`.

The terminal is acquired in `PreStartup`. Systems that require it during that schedule can order
themselves after `ContextSetup`. Normal runner return, `AppExit`, app/world drop, direct context drop,
and unwinding panic all restore the session. A previous panic hook runs after terminal cleanup and is
restored after ordinary teardown. Sequential sessions capture the then-current previous hook.

Kitty support-query errors are deliberately treated as "unsupported". No kitty mode is marked as
owned or later popped in that case.

## Deliberate boundaries

These outcomes are not promised:

- Replacing the process panic hook after the session starts. Normal teardown restores the hook that
  existed before the session, so a later replacement can be discarded.
- Continuing to use an app after catching its panic. The hook has already restored the terminal and
  consumed cleanup ownership; drop the failed app and create a new session.
- A background-thread panic while the terminal app continues. Panic hooks are process-wide, so that
  panic restores the active terminal session.
- Runtime behavior during simultaneous panics. The terminal token still has one cleanup winner, but
  Rust's panic-hook and abort behavior is outside this library's control.
- Destructor cleanup after `std::process::exit`, `SIGKILL`, or another hard process termination.
- `SIGINT`, `SIGTERM`, and Windows console-control recovery. This crate installs no signal handler.
- Guaranteed recovery when a previous panic hook aborts or `panic = "abort"` prevents unwinding.
  Cleanup is invoked before the previous hook, but the operating system can terminate the process
  immediately afterward.

The real PTY suite does not intentionally break the process-global stdout descriptor after setup.
Doing so portably would require unsafe descriptor replacement and would make libtest itself
unreliable. Deterministic fault injection covers each output cleanup error, complete continuation,
first-error selection, and non-panicking `Drop` instead.

Windows is compile-checked. Runtime terminal restoration is exercised on Linux and macOS PTYs;
there is no ConPTY runtime job yet.

## Deterministic matrix

All rows run in the default-feature `ubuntu - stable`, `ubuntu - beta`, and `macos-latest - stable`
jobs. The stable Ubuntu no-mouse job repeats applicable rows without the `mouse` feature.

| Contract / invariant | Initial state and owner | Transition or injection | Features / repetition | Exact test | Expected trace and final state |
| --- | --- | --- | --- | --- | --- |
| Successful acquisition | Dormant; initializer | All setup operations succeed | Any Crossterm build | `initialization_success_acquires_every_capability_without_rollback` | query -> raw -> alternate -> terminal; no rollback |
| Preserve caller raw mode | Raw owned by caller | Already-raw query returns true | Any | `initialization_rejects_preexisting_raw_mode_without_modifying_it` | query only; `AlreadyExists` |
| Roll back every setup error | Dormant; initializer | Error at raw query, raw enable, alternate enter, or terminal creation | Table-driven | `initialization_errors_roll_back_only_possibly_acquired_capabilities` | only possibly acquired capabilities released, in inverse order |
| Roll back every setup panic | Dormant; initializer | Panic at each setup boundary | Table-driven | `initialization_panics_run_the_same_partial_rollback` | unwind follows the same partial rollback trace |
| Continue rollback after errors | Raw and alternate possibly active | Terminal creation plus both rollback operations fail | Multiple failures | `initialization_rollback_attempts_every_release_after_errors` | leave alternate and disable raw both attempted |
| Exactly-once claim | Cleanup token | Two sequential cleanup calls | Direct/session/hook equivalent | `terminal_cleanup_token_allows_one_restore` | first closure once; second no-op |
| Failed winner remains winner | Cleanup token | Winning closure returns `Err` or panics | Error and unwind | `terminal_cleanup_error_or_panic_still_consumes_ownership` | later cleanup is a no-op |
| Bounded lifecycle model | Cleanup token | 729 six-step traces of direct/session/hook contenders | Repeated ownership transitions | `terminal_cleanup_bounded_owner_sequences_have_one_winner` | exactly one trace winner |
| Concurrent claim | Shared cleanup token | 32 callers released by one barrier | Concurrent contenders | `terminal_cleanup_concurrent_callers_have_one_winner` | one atomic winner and one closure call |
| Ordered cleanup | Full acquired set | Cleanup called twice | Mouse conditional | `cleanup_runs_once_in_order` | kitty -> mouse -> raw -> alternate -> cursor, once |
| Do not release unowned modes | Terminal only; kitty-only; mouse-only | Normal cleanup | Settings combinations | `cleanup_only_runs_enabled_actions` | only enabled optional modes plus terminal restoration |
| Continue and choose first error | Full acquired set | Every cleanup operation returns `Err` | Multiple failures | `cleanup_attempts_every_action_and_returns_the_first_error` | complete trace; kitty error returned |
| Cover each cleanup failure | Full acquired set | One failure at each cleanup action | Table-driven | `every_cleanup_failure_is_reported_after_the_complete_trace` | complete trace; injected error returned |
| `Drop` never panics on `Err` | Full acquired set | Every cleanup action fails | Multiple failures | `cleanup_drop_never_panics_when_every_restoration_fails` | complete trace; best-effort report |
| Disabled/unsupported modes | Terminal token | Kitty disabled or unsupported | Kitty settings | `mode_configuration_covers_disabled_and_unsupported_settings` | no unowned kitty pop |
| Query error is unsupported | Terminal token | Kitty support query returns `Err` | Kitty enabled | `kitty_query_errors_are_treated_as_unsupported_without_leaking_terminal` | query -> terminal cleanup; no push/pop |
| Ambiguous kitty enable failure | Terminal plus possible kitty | Kitty push returns `Err` | Kitty supported | `kitty_enable_error_rolls_back_every_possibly_enabled_mode` | kitty pop then full terminal cleanup |
| Ambiguous mouse enable failure | Terminal plus possible mouse | Mouse enable returns `Err` | Mouse feature | `mouse_enable_error_rolls_back_mouse_and_terminal` | mouse disable then full terminal cleanup |
| Optional-mode inverse order | Terminal, mouse, kitty | Normal teardown | Mouse + kitty | `enabled_mouse_and_kitty_are_acquired_and_released_in_inverse_order` | mouse/query/kitty acquire; kitty/mouse/terminal release |
| Optional-mode setup panic | Terminal plus partial modes | Panic at query, kitty enable, or mouse enable | Table-driven | `panic_at_each_optional_mode_boundary_runs_partial_rollback`; `panic_while_enabling_mouse_runs_partial_rollback` | precise partial rollback during unwind |
| Normal hook restoration | Previous hook installed; guard active | Ordinary guard drop, then caught probe panic | Isolated unit subprocess | `normal_drop_restores_the_previous_hook` | previous hook runs once; removed cleanup wrapper runs zero times |

The cleanup claim is one standard-library `AtomicBool::swap`, not a multi-operation lock-free
algorithm. The real type is tested with 32 simultaneous callers; a separate Loom mirror would only
re-test Loom's atomic primitive rather than this code, so it is intentionally omitted.

## PTY matrix

Each row runs in a fresh child process with a ten-second timeout, a one-MiB output cap, and
escaped-byte diagnostics. Real-terminal rows use independent PTY state and accept only EOF or Linux
PTY `EIO`; the non-TTY row starts a new session without a controlling terminal. Default-feature rows
run on Ubuntu stable/beta and macOS stable. Stable Ubuntu repeats the suite 100 times and also runs
the applicable cases without the mouse feature.

| Contract / invariant | Initial state and owner | Exit / hook / response | Feature / platform axis | Exact test | Expected trace and final state |
| --- | --- | --- | --- | --- | --- |
| Panic cleanup ordering | Ordinary PTY; app session + hook | `Update` panic | Unix PTY | `panic_cleanup_precedes_panic_output` | leave alternate before panic text; exactly once; stable termios restored |
| Exactly-once panic cleanup | Ordinary PTY; app session + hook | `Update` panic | Unix PTY | `panic_leaves_alternate_screen_exactly_once` | one leave sequence |
| Every Bevy phase | Ordinary PTY; app session + hook | `PreStartup` after `ContextSetup`, `Startup`, `Update`, `PostUpdate`, runner panic | Five child cases | `panic_in_each_bevy_phase_restores_exactly_once` | cleanup before panic, once, stable termios restored |
| Previous hook ordering | Ordinary PTY; custom previous hook | Panic | Hook-before-session | `custom_previous_panic_hook_runs_once_after_cleanup` | cleanup then one previous-hook sentinel |
| Previous hook abort boundary | Ordinary PTY; panicking previous hook | Panic then abort | Hook failure | `cleanup_precedes_a_panicking_previous_hook` | cleanup complete before previous-hook panic/abort |
| Graceful exit | Ordinary PTY; app session | `AppExit` / runner return | Default features | `graceful_app_exit_restores_exactly_once` | one leave; termios restored |
| Runner return without exit | Ordinary PTY; app session | Run-once return and app drop | Default features | `app_drop_without_app_exit_restores_exactly_once` | one leave; termios restored |
| Hook restoration and repetition | Ordinary PTY; 16 sequential sessions | Final panic after all sessions | Repeated process state | `many_sequential_apps_do_not_leave_stale_cleanup_hooks` | 16 cleanups; previous hook once; no stale cleanup |
| Current hook capture | Ordinary PTY; two sequential sessions | Replace the hook between sessions, then panic | Hook lifecycle | `sequential_sessions_capture_the_current_previous_hook` | only the second session's previous hook runs; two cleanups |
| Exact caller attributes | PTY with toggled `ECHO` sentinel | Normal app drop | Linux/macOS termios | `sentinel_terminal_attributes_are_restored_after_the_session` | all stable termios settings equal and raw=false |
| Direct owner | Ordinary PTY; direct context | Direct drop | No Bevy session | `direct_context_drop_restores_exactly_once` | one cleanup and stable termios restoration |
| Pre-existing raw state | Caller enables raw | Initialization rejection, caller restores | Initial-state axis | `preexisting_raw_mode_is_rejected_and_preserved` | no alternate escapes; caller raw remains active until caller restores |
| Nested ownership attempt | First direct context active | Second init rejected; first dropped | Nested/repetition axis | `nested_context_is_rejected_and_first_owner_recovers_once` | one enter/leave pair; first remains valid |
| No terminal | Null stdin and piped output | Direct initialization error | Non-TTY environment | `non_tty_initialization_fails_without_cleanup_side_effects` | no alternate-screen setup or cleanup escapes |
| Mouse acquisition/order | Ordinary PTY; app session | Normal drop | Mouse feature | `mouse_capture_is_enabled_and_disabled_once_in_order` | one enable/disable; disable before terminal leave |
| Kitty supported | PTY emulator returns kitty flags and device attributes | Normal drop | Kitty enabled/supported | `kitty_supported_terminal_is_enabled_and_restored_once` | query, one push/pop, pop before terminal leave |
| Kitty unsupported | PTY emulator returns only device attributes | Normal drop | Kitty enabled/unsupported | `kitty_unsupported_terminal_is_not_modified` | query, no push/pop, normal terminal cleanup |
| Cursor ordering | Ordinary PTY; app session | Normal drop | Unix PTY | `cleanup_escape_order_is_terminal_then_cursor_show` | alternate-screen leave before cursor show |

The raw-mode query/enable and terminal-construction error branches are deterministic unit cases
because reliably forcing those operating-system failures in a real PTY would require global file
descriptor mutation. Kitty query failure is likewise deterministic; supported and unsupported
protocol branches use real PTY responses. These are equivalence-based omissions, not untested
branches.

Termios comparisons include input/output/control/local configuration flags, control characters,
line discipline where exposed, and input/output baud rates. They exclude only `PENDIN` and `FLUSHO`,
which are kernel-maintained state indicators rather than restorable configuration; raw before/after
structures remain in failure diagnostics.

Resource insertion has no fallible Bevy API boundary. Before insertion, the context and cleanup plan
are stack-owned RAII values, so any unwind follows the setup-panic rollback tests. After insertion,
the app session is the only owner and is covered by normal drop and panic tests.

Rust's panic-hook installation API is also infallible during ordinary non-panicking setup. There is
therefore no fabricated hook-install error case; custom, restored, stale, panicking, and
phase-specific hook behavior is exercised in isolated subprocesses instead.

## CI coverage

| CI job / step | Evidence |
| --- | --- |
| `ubuntu - stable` default features | Deterministic tests and full PTY suite |
| `ubuntu - stable` Crossterm without mouse | Deterministic and applicable PTY tests with `std,async_executor,crossterm,keyboard` |
| `ubuntu - stable` lifecycle stress | Full PTY suite repeated 100 times |
| `ubuntu - beta` default features | Deterministic tests and full PTY suite on the beta toolchain |
| `macos-latest - stable` default features | Full PTY behavior and macOS EOF/termios behavior |
| Ubuntu/macOS all features | Windowed/all-feature compile and regression tests; the PTY crate intentionally has zero tests under `windowed` |
| `windows-latest - stable (compile)` | Default and all-feature Windows compilation; no ConPTY runtime guarantee |

Formatting, default/all-feature Clippy, docs, and all-target tests remain part of the repository's
checks workflow and completion gate.
