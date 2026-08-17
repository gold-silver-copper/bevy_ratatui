//! End-to-end lifecycle tests that run the application in an isolated pseudo-terminal.
//!
//! Each test launches a fresh copy of this test binary so process-global panic hooks and
//! crossterm state cannot leak between scenarios. The child detaches from any controlling
//! terminal, so terminal queries go to the PTY rather than to a developer's real terminal. The
//! parent captures a bounded escape stream, applies a hard timeout, optionally emulates kitty
//! keyboard-protocol responses, and compares the PTY's stable, restorable termios settings before
//! and after the child.

#![cfg(all(unix, feature = "crossterm", not(feature = "windowed")))]

use std::{
    env,
    fs::File,
    io::{self, Read, Write},
    os::unix::process::ExitStatusExt,
    panic,
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use bevy::{
    app::{AppExit, ScheduleRunnerPlugin},
    prelude::*,
};
#[cfg(feature = "mouse")]
use bevy_ratatui::mouse::MouseEnabled;
use bevy_ratatui::{
    RatatuiContext, RatatuiPlugins,
    context::{ContextSetup, SessionError},
    kitty::KittyEnabled,
};
use nix::{
    errno::Errno,
    fcntl::{FcntlArg, FdFlag, fcntl},
    pty::{OpenptyResult, Winsize, openpty},
    sys::termios::{LocalFlags, SetArg, Termios, cfgetispeed, cfgetospeed, tcgetattr, tcsetattr},
    unistd::setsid,
};
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode, is_raw_mode_enabled};

const MODE_ENV: &str = "BEVY_RATATUI_LIFECYCLE_TEST_MODE";
const ENTER_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049h";
const LEAVE_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
const RESTORE_CURSOR_DEC: &[u8] = b"\x1b8";
const RESTORE_CURSOR_SCO: &[u8] = b"\x1b[u";
#[cfg(feature = "mouse")]
const ENABLE_MOUSE_CAPTURE: &[u8] = b"\x1b[?1000h";
#[cfg(feature = "mouse")]
const DISABLE_MOUSE_CAPTURE: &[u8] = b"\x1b[?1000l";
const QUERY_KITTY: &[u8] = b"\x1b[?u\x1b[c";
const PUSH_KITTY: &[u8] = b"\x1b[>";
const POP_KITTY: &[u8] = b"\x1b[<1u";
const KITTY_SUPPORTED_RESPONSE: &[u8] = b"\x1b[?1u\x1b[?1;2c";
const KITTY_UNSUPPORTED_RESPONSE: &[u8] = b"\x1b[?1;2c";
const PANICKED_AT: &[u8] = b"panicked at";
const PANIC_MESSAGE: &[u8] = b"lifecycle panic probe";
const FINAL_PANIC_MESSAGE: &[u8] = b"panic after sequential lifecycle probes";
const PREVIOUS_HOOK_MESSAGE: &[u8] = b"previous-panic-hook-called";
const FIRST_SESSION_HOOK_MESSAGE: &[u8] = b"first-session-hook-called";
const SECOND_SESSION_HOOK_MESSAGE: &[u8] = b"second-session-hook-called";
const PREVIOUS_HOOK_PANIC: &[u8] = b"previous panic hook panicked";
const RAW_MODE_RESTORED: &[u8] = b"raw-mode-restored=true";
const ALREADY_RAW_PRESERVED: &[u8] = b"already-raw-preserved=true";
const NESTED_SESSION_REJECTED: &[u8] = b"nested-session-rejected=true";
const NON_TTY_REJECTED: &[u8] = b"non-tty-rejected=true";
const OFF_THREAD_INACTIVE: &[u8] = b"session-inactive-after-off-thread-panic=true";
const CHILD_TIMEOUT: Duration = Duration::from_secs(15);
const OUTPUT_COLLECTION_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const SEQUENTIAL_SESSION_COUNT: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProbeMode {
    PanicUpdate,
    PanicPreStartup,
    PanicStartup,
    PanicPostStartup,
    PanicPreUpdate,
    PanicPostUpdate,
    PanicLast,
    PanicRunner,
    PanicOffThread,
    CustomHookPanic,
    PreviousHookPanics,
    Exit,
    Drop,
    DirectDrop,
    Sequential,
    SequentialHooks,
    AlreadyRaw,
    Nested,
    NonTty,
    #[cfg(feature = "mouse")]
    Mouse,
    KittySupported,
    KittyUnsupported,
    KittyTimeout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PanicPhase {
    PreStartup,
    Startup,
    PostStartup,
    PreUpdate,
    Update,
    PostUpdate,
    Last,
    Runner,
    OffThread,
}

#[derive(Clone, Copy)]
enum AppAction {
    None,
    Panic(PanicPhase),
    Exit,
}

#[derive(Clone, Copy, Default)]
struct AppSettings {
    kitty: bool,
    mouse: bool,
    expected_kitty: Option<bool>,
    expected_mouse: Option<bool>,
}

#[derive(Clone, Copy, Default)]
struct ProbeOptions {
    sentinel_termios: bool,
    terminal_response: TerminalResponse,
    backtrace: bool,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
enum TerminalResponse {
    #[default]
    None,
    KittySupported,
    KittyUnsupported,
}

impl TerminalResponse {
    const fn bytes(self) -> Option<&'static [u8]> {
        match self {
            Self::None => None,
            Self::KittySupported => Some(KITTY_SUPPORTED_RESPONSE),
            Self::KittyUnsupported => Some(KITTY_UNSUPPORTED_RESPONSE),
        }
    }
}

impl ProbeMode {
    const ALL: &[Self] = &[
        Self::PanicUpdate,
        Self::PanicPreStartup,
        Self::PanicStartup,
        Self::PanicPostStartup,
        Self::PanicPreUpdate,
        Self::PanicPostUpdate,
        Self::PanicLast,
        Self::PanicRunner,
        Self::PanicOffThread,
        Self::CustomHookPanic,
        Self::PreviousHookPanics,
        Self::Exit,
        Self::Drop,
        Self::DirectDrop,
        Self::Sequential,
        Self::SequentialHooks,
        Self::AlreadyRaw,
        Self::Nested,
        Self::NonTty,
        #[cfg(feature = "mouse")]
        Self::Mouse,
        Self::KittySupported,
        Self::KittyUnsupported,
        Self::KittyTimeout,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::PanicUpdate => "panic-update",
            Self::PanicPreStartup => "panic-pre-startup",
            Self::PanicStartup => "panic-startup",
            Self::PanicPostStartup => "panic-post-startup",
            Self::PanicPreUpdate => "panic-pre-update",
            Self::PanicPostUpdate => "panic-post-update",
            Self::PanicLast => "panic-last",
            Self::PanicRunner => "panic-runner",
            Self::PanicOffThread => "panic-off-thread",
            Self::CustomHookPanic => "custom-hook-panic",
            Self::PreviousHookPanics => "previous-hook-panics",
            Self::Exit => "exit",
            Self::Drop => "drop",
            Self::DirectDrop => "direct-drop",
            Self::Sequential => "sequential",
            Self::SequentialHooks => "sequential-hooks",
            Self::AlreadyRaw => "already-raw",
            Self::Nested => "nested",
            Self::NonTty => "non-tty",
            #[cfg(feature = "mouse")]
            Self::Mouse => "mouse",
            Self::KittySupported => "kitty-supported",
            Self::KittyUnsupported => "kitty-unsupported",
            Self::KittyTimeout => "kitty-timeout",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|mode| mode.as_str() == value)
    }

    const fn panic_phase(self) -> Option<PanicPhase> {
        match self {
            Self::PanicPreStartup => Some(PanicPhase::PreStartup),
            Self::PanicStartup => Some(PanicPhase::Startup),
            Self::PanicPostStartup => Some(PanicPhase::PostStartup),
            Self::PanicPreUpdate => Some(PanicPhase::PreUpdate),
            Self::PanicUpdate => Some(PanicPhase::Update),
            Self::PanicPostUpdate => Some(PanicPhase::PostUpdate),
            Self::PanicLast => Some(PanicPhase::Last),
            Self::PanicRunner => Some(PanicPhase::Runner),
            Self::PanicOffThread => Some(PanicPhase::OffThread),
            _ => None,
        }
    }
}

struct ProbeResult {
    output: Vec<u8>,
    status: ExitStatus,
    terminal_attributes_restored: bool,
    terminal_attributes: String,
}

/// The exact sequence from upstream issue 104: a panic with a backtrace must leave the alternate
/// screen exactly once, before the diagnostic, and never move the cursor back over it afterwards.
#[test]
fn issue_104_panic_leaves_alternate_screen_once_and_never_after_the_diagnostic() {
    if run_child_if_requested(ProbeMode::PanicUpdate) {
        return;
    }

    let probe = run_probe(
        "issue_104_panic_leaves_alternate_screen_once_and_never_after_the_diagnostic",
        ProbeMode::PanicUpdate,
        ProbeOptions {
            backtrace: true,
            ..Default::default()
        },
    );
    assert_probe_failed(&probe);
    assert_eq!(
        probe.status.code(),
        Some(101),
        "expected a panic exit: {}",
        probe_diagnostics(&probe)
    );
    assert_terminal_attributes_restored(&probe);
    assert_eq!(
        count(&probe.output, LEAVE_ALTERNATE_SCREEN),
        1,
        "the alternate screen must be left exactly once: {}",
        probe_diagnostics(&probe)
    );
    assert_before(&probe, LEAVE_ALTERNATE_SCREEN, PANICKED_AT);

    let diagnostic = find(&probe.output, PANICKED_AT);
    let after_diagnostic = &probe.output[diagnostic..];
    for sequence in [
        LEAVE_ALTERNATE_SCREEN,
        ENTER_ALTERNATE_SCREEN,
        RESTORE_CURSOR_DEC,
        RESTORE_CURSOR_SCO,
    ] {
        assert!(
            !contains(after_diagnostic, sequence),
            "{:?} was written after the panic diagnostic and would move the cursor over it: {}",
            escaped(sequence),
            probe_diagnostics(&probe)
        );
    }
}

/// Cleanup must finish before the previous panic hook writes diagnostics, and the later
/// app/resource drops must not emit a second terminal restoration.
#[test]
fn panic_cleanup_precedes_panic_output() {
    if run_child_if_requested(ProbeMode::PanicUpdate) {
        return;
    }

    let probe = run_probe(
        "panic_cleanup_precedes_panic_output",
        ProbeMode::PanicUpdate,
        ProbeOptions::default(),
    );
    assert_probe_failed(&probe);
    assert_terminal_attributes_restored(&probe);
    assert_before(&probe, LEAVE_ALTERNATE_SCREEN, PANIC_MESSAGE);
    assert_eq!(
        count(&probe.output, LEAVE_ALTERNATE_SCREEN),
        1,
        "cleanup ran again after the panic hook: {}",
        probe_diagnostics(&probe)
    );
}

/// The panic hook must own recovery after terminal setup in every Bevy phase and at the runner
/// boundary.
#[test]
fn panic_in_each_bevy_phase_restores_exactly_once() {
    if let Some(mode) = requested_mode() {
        assert!(
            mode.panic_phase().is_some(),
            "unexpected child mode {mode:?}"
        );
        run_child(mode);
        return;
    }

    for mode in [
        ProbeMode::PanicPreStartup,
        ProbeMode::PanicStartup,
        ProbeMode::PanicPostStartup,
        ProbeMode::PanicPreUpdate,
        ProbeMode::PanicUpdate,
        ProbeMode::PanicPostUpdate,
        ProbeMode::PanicLast,
        ProbeMode::PanicRunner,
    ] {
        let probe = run_probe(
            "panic_in_each_bevy_phase_restores_exactly_once",
            mode,
            ProbeOptions::default(),
        );
        assert_probe_failed(&probe);
        assert_terminal_attributes_restored(&probe);
        assert_eq!(
            count(&probe.output, LEAVE_ALTERNATE_SCREEN),
            1,
            "{mode:?} did not restore exactly once: {}",
            probe_diagnostics(&probe)
        );
        assert_before(&probe, LEAVE_ALTERNATE_SCREEN, PANIC_MESSAGE);
    }
}

/// A panic on another thread restores the terminal through the same hook; the surviving session
/// must then be visibly inactive and refuse to draw.
#[test]
fn off_thread_panic_restores_once_and_poisons_the_session() {
    if run_child_if_requested(ProbeMode::PanicOffThread) {
        return;
    }

    let probe = run_probe(
        "off_thread_panic_restores_once_and_poisons_the_session",
        ProbeMode::PanicOffThread,
        ProbeOptions::default(),
    );
    assert_probe_failed(&probe);
    assert_terminal_attributes_restored(&probe);
    assert_eq!(
        count(&probe.output, LEAVE_ALTERNATE_SCREEN),
        1,
        "{}",
        probe_diagnostics(&probe)
    );
    assert_before(&probe, LEAVE_ALTERNATE_SCREEN, PANIC_MESSAGE);
    assert!(
        contains(&probe.output, OFF_THREAD_INACTIVE),
        "the session stayed active after an off-thread panic: {}",
        probe_diagnostics(&probe)
    );
}

/// A custom hook installed before the app must run once, after terminal recovery.
#[test]
fn custom_previous_panic_hook_runs_once_after_cleanup() {
    if run_child_if_requested(ProbeMode::CustomHookPanic) {
        return;
    }

    let probe = run_probe(
        "custom_previous_panic_hook_runs_once_after_cleanup",
        ProbeMode::CustomHookPanic,
        ProbeOptions::default(),
    );
    assert_probe_failed(&probe);
    assert_terminal_attributes_restored(&probe);
    assert_eq!(count(&probe.output, PREVIOUS_HOOK_MESSAGE), 1);
    assert_before(&probe, LEAVE_ALTERNATE_SCREEN, PREVIOUS_HOOK_MESSAGE);
}

/// Even when the previous hook aborts by panicking, terminal recovery must already be complete.
#[test]
fn cleanup_precedes_a_panicking_previous_hook() {
    if run_child_if_requested(ProbeMode::PreviousHookPanics) {
        return;
    }

    let probe = run_probe(
        "cleanup_precedes_a_panicking_previous_hook",
        ProbeMode::PreviousHookPanics,
        ProbeOptions::default(),
    );
    assert_probe_failed(&probe);
    assert_terminal_attributes_restored(&probe);
    assert_eq!(count(&probe.output, LEAVE_ALTERNATE_SCREEN), 1);
    assert_before(&probe, LEAVE_ALTERNATE_SCREEN, PREVIOUS_HOOK_PANIC);
}

/// A normal Bevy `AppExit` must restore the terminal exactly once.
#[test]
fn graceful_app_exit_restores_exactly_once() {
    if run_child_if_requested(ProbeMode::Exit) {
        return;
    }

    let probe = run_probe(
        "graceful_app_exit_restores_exactly_once",
        ProbeMode::Exit,
        ProbeOptions::default(),
    );
    assert_probe_succeeded(&probe);
    assert_terminal_attributes_restored(&probe);
    assert_eq!(count(&probe.output, LEAVE_ALTERNATE_SCREEN), 1);
    assert_before(&probe, LEAVE_ALTERNATE_SCREEN, SHOW_CURSOR);
}

/// Custom and run-once runners may return without `AppExit`; dropping the app must still restore.
#[test]
fn app_drop_without_app_exit_restores_exactly_once() {
    if run_child_if_requested(ProbeMode::Drop) {
        return;
    }

    let probe = run_probe(
        "app_drop_without_app_exit_restores_exactly_once",
        ProbeMode::Drop,
        ProbeOptions::default(),
    );
    assert_probe_succeeded(&probe);
    assert_terminal_attributes_restored(&probe);
    assert_eq!(count(&probe.output, LEAVE_ALTERNATE_SCREEN), 1);
}

/// Finished apps must reinstate the hook they replaced: a final panic after many sessions runs the
/// caller's hook exactly once and must not replay terminal cleanup from any earlier session.
#[test]
fn many_sequential_apps_do_not_replay_cleanup_on_a_later_panic() {
    if run_child_if_requested(ProbeMode::Sequential) {
        return;
    }

    let probe = run_probe(
        "many_sequential_apps_do_not_replay_cleanup_on_a_later_panic",
        ProbeMode::Sequential,
        ProbeOptions::default(),
    );
    assert_probe_failed(&probe);
    assert!(
        contains(&probe.output, FINAL_PANIC_MESSAGE),
        "final panic was not observed: {}",
        probe_diagnostics(&probe)
    );
    assert_eq!(
        count(&probe.output, PREVIOUS_HOOK_MESSAGE),
        1,
        "the caller's hook was not restored exactly once: {}",
        probe_diagnostics(&probe)
    );
    assert_eq!(
        count(&probe.output, LEAVE_ALTERNATE_SCREEN),
        SEQUENTIAL_SESSION_COUNT,
        "the final panic replayed a stale terminal hook: {}",
        probe_diagnostics(&probe)
    );
}

/// A later session must capture the hook that exists when that session begins, not the hook that
/// existed when an earlier session began.
#[test]
fn sequential_sessions_capture_the_current_previous_hook() {
    if run_child_if_requested(ProbeMode::SequentialHooks) {
        return;
    }

    let probe = run_probe(
        "sequential_sessions_capture_the_current_previous_hook",
        ProbeMode::SequentialHooks,
        ProbeOptions::default(),
    );
    assert_probe_failed(&probe);
    assert_eq!(count(&probe.output, FIRST_SESSION_HOOK_MESSAGE), 0);
    assert_eq!(count(&probe.output, SECOND_SESSION_HOOK_MESSAGE), 1);
    assert_eq!(count(&probe.output, LEAVE_ALTERNATE_SCREEN), 2);
}

/// Cleanup must restore the stable termios settings that existed before the app, including a
/// deliberate sentinel change rather than only the PTY defaults.
#[test]
fn sentinel_terminal_attributes_are_restored_after_the_session() {
    if run_child_if_requested(ProbeMode::Drop) {
        return;
    }

    let probe = run_probe(
        "sentinel_terminal_attributes_are_restored_after_the_session",
        ProbeMode::Drop,
        ProbeOptions {
            sentinel_termios: true,
            ..Default::default()
        },
    );
    assert_probe_succeeded(&probe);
    assert_terminal_attributes_restored(&probe);
    assert!(
        contains(&probe.output, RAW_MODE_RESTORED),
        "crossterm still reported raw mode: {}",
        probe_diagnostics(&probe)
    );
}

/// A directly constructed context is the same session type and restores once on drop.
#[test]
fn direct_context_drop_restores_exactly_once() {
    if run_child_if_requested(ProbeMode::DirectDrop) {
        return;
    }

    let probe = run_probe(
        "direct_context_drop_restores_exactly_once",
        ProbeMode::DirectDrop,
        ProbeOptions::default(),
    );
    assert_probe_succeeded(&probe);
    assert_terminal_attributes_restored(&probe);
    assert_eq!(count(&probe.output, ENTER_ALTERNATE_SCREEN), 1);
    assert_eq!(count(&probe.output, LEAVE_ALTERNATE_SCREEN), 1);
    assert_before(&probe, LEAVE_ALTERNATE_SCREEN, SHOW_CURSOR);
}

/// Acquisition must reject a terminal already in raw mode without entering the alternate screen
/// or changing the caller-owned raw state.
#[test]
fn preexisting_raw_mode_is_rejected_and_preserved() {
    if run_child_if_requested(ProbeMode::AlreadyRaw) {
        return;
    }

    let probe = run_probe(
        "preexisting_raw_mode_is_rejected_and_preserved",
        ProbeMode::AlreadyRaw,
        ProbeOptions::default(),
    );
    assert_probe_succeeded(&probe);
    assert_terminal_attributes_restored(&probe);
    assert!(
        contains(&probe.output, ALREADY_RAW_PRESERVED),
        "{}",
        probe_diagnostics(&probe)
    );
    assert_eq!(count(&probe.output, ENTER_ALTERNATE_SCREEN), 0);
    assert_eq!(count(&probe.output, LEAVE_ALTERNATE_SCREEN), 0);
}

/// A second session while the first is active must fail without disturbing the first.
#[test]
fn nested_session_is_rejected_and_first_owner_recovers_once() {
    if run_child_if_requested(ProbeMode::Nested) {
        return;
    }

    let probe = run_probe(
        "nested_session_is_rejected_and_first_owner_recovers_once",
        ProbeMode::Nested,
        ProbeOptions::default(),
    );
    assert_probe_succeeded(&probe);
    assert_terminal_attributes_restored(&probe);
    assert!(
        contains(&probe.output, NESTED_SESSION_REJECTED),
        "{}",
        probe_diagnostics(&probe)
    );
    assert_eq!(count(&probe.output, ENTER_ALTERNATE_SCREEN), 1);
    assert_eq!(count(&probe.output, LEAVE_ALTERNATE_SCREEN), 1);
}

/// With no controlling terminal, acquisition must fail without emitting setup or cleanup escapes.
/// This subprocess intentionally runs without a PTY.
#[test]
fn non_tty_initialization_fails_without_cleanup_side_effects() {
    if run_child_if_requested(ProbeMode::NonTty) {
        return;
    }

    let (status, output) = run_plain_probe(
        "non_tty_initialization_fails_without_cleanup_side_effects",
        ProbeMode::NonTty,
    );
    assert!(
        status.success(),
        "non-TTY probe failed with {status:?}: {}",
        escaped(&output)
    );
    assert!(contains(&output, NON_TTY_REJECTED), "{}", escaped(&output));
    assert_eq!(count(&output, b"\x1b["), 0, "{}", escaped(&output));
}

/// Mouse capture must be enabled after the alternate screen and released before it is left.
#[cfg(feature = "mouse")]
#[test]
fn mouse_capture_is_enabled_and_disabled_once_in_order() {
    if run_child_if_requested(ProbeMode::Mouse) {
        return;
    }

    let probe = run_probe(
        "mouse_capture_is_enabled_and_disabled_once_in_order",
        ProbeMode::Mouse,
        ProbeOptions::default(),
    );
    assert_probe_succeeded(&probe);
    assert_terminal_attributes_restored(&probe);
    assert_eq!(count(&probe.output, ENABLE_MOUSE_CAPTURE), 1);
    assert_eq!(count(&probe.output, DISABLE_MOUSE_CAPTURE), 1);
    assert_before(&probe, ENTER_ALTERNATE_SCREEN, ENABLE_MOUSE_CAPTURE);
    assert_before(&probe, ENABLE_MOUSE_CAPTURE, DISABLE_MOUSE_CAPTURE);
    assert_before(&probe, DISABLE_MOUSE_CAPTURE, LEAVE_ALTERNATE_SCREEN);
}

/// Without mouse capture requested, no mouse sequences may be written.
#[cfg(feature = "mouse")]
#[test]
fn mouse_capture_is_not_touched_when_disabled() {
    if run_child_if_requested(ProbeMode::Drop) {
        return;
    }

    let probe = run_probe(
        "mouse_capture_is_not_touched_when_disabled",
        ProbeMode::Drop,
        ProbeOptions::default(),
    );
    assert_probe_succeeded(&probe);
    assert_eq!(count(&probe.output, ENABLE_MOUSE_CAPTURE), 0);
    assert_eq!(count(&probe.output, DISABLE_MOUSE_CAPTURE), 0);
}

/// An emulated supporting terminal must cause one kitty push after entering the alternate screen
/// and one pop before leaving it.
#[test]
fn kitty_supported_terminal_is_enabled_and_restored_once() {
    if run_child_if_requested(ProbeMode::KittySupported) {
        return;
    }

    let probe = run_probe(
        "kitty_supported_terminal_is_enabled_and_restored_once",
        ProbeMode::KittySupported,
        ProbeOptions {
            terminal_response: TerminalResponse::KittySupported,
            ..Default::default()
        },
    );
    assert_probe_succeeded(&probe);
    assert_terminal_attributes_restored(&probe);
    assert_eq!(count(&probe.output, QUERY_KITTY), 1);
    assert_eq!(count(&probe.output, PUSH_KITTY), 1);
    assert_eq!(count(&probe.output, POP_KITTY), 1);
    assert_before(&probe, ENTER_ALTERNATE_SCREEN, PUSH_KITTY);
    assert_before(&probe, PUSH_KITTY, POP_KITTY);
    assert_before(&probe, POP_KITTY, LEAVE_ALTERNATE_SCREEN);
}

/// A terminal that replies to primary device attributes but not kitty flags is unsupported and
/// must not receive a push or pop sequence.
#[test]
fn kitty_unsupported_terminal_is_not_modified() {
    if run_child_if_requested(ProbeMode::KittyUnsupported) {
        return;
    }

    let probe = run_probe(
        "kitty_unsupported_terminal_is_not_modified",
        ProbeMode::KittyUnsupported,
        ProbeOptions {
            terminal_response: TerminalResponse::KittyUnsupported,
            ..Default::default()
        },
    );
    assert_probe_succeeded(&probe);
    assert_terminal_attributes_restored(&probe);
    assert_eq!(count(&probe.output, QUERY_KITTY), 1);
    assert_eq!(count(&probe.output, PUSH_KITTY), 0);
    assert_eq!(count(&probe.output, POP_KITTY), 0);
    assert_eq!(count(&probe.output, LEAVE_ALTERNATE_SCREEN), 1);
}

/// A terminal that never answers the kitty query must not fail the session, and no push or pop
/// may be written.
#[test]
fn kitty_query_timeout_does_not_fail_the_session() {
    if run_child_if_requested(ProbeMode::KittyTimeout) {
        return;
    }

    let probe = run_probe(
        "kitty_query_timeout_does_not_fail_the_session",
        ProbeMode::KittyTimeout,
        ProbeOptions::default(),
    );
    assert_probe_succeeded(&probe);
    assert_terminal_attributes_restored(&probe);
    assert_eq!(count(&probe.output, QUERY_KITTY), 1);
    assert_eq!(count(&probe.output, PUSH_KITTY), 0);
    assert_eq!(count(&probe.output, POP_KITTY), 0);
    assert_eq!(count(&probe.output, LEAVE_ALTERNATE_SCREEN), 1);
}

/// Kitty disabled in the plugin group must not even query the terminal.
#[test]
fn kitty_disabled_is_not_queried() {
    if run_child_if_requested(ProbeMode::Drop) {
        return;
    }

    let probe = run_probe(
        "kitty_disabled_is_not_queried",
        ProbeMode::Drop,
        ProbeOptions::default(),
    );
    assert_probe_succeeded(&probe);
    assert_eq!(count(&probe.output, QUERY_KITTY), 0);
    assert_eq!(count(&probe.output, PUSH_KITTY), 0);
    assert_eq!(count(&probe.output, POP_KITTY), 0);
}

fn requested_mode() -> Option<ProbeMode> {
    env::var(MODE_ENV)
        .ok()
        .and_then(|mode| ProbeMode::parse(&mode))
}

fn run_child_if_requested(expected: ProbeMode) -> bool {
    let Some(mode) = requested_mode() else {
        return false;
    };

    assert_eq!(mode, expected);
    run_child(expected);
    true
}

fn run_child(mode: ProbeMode) {
    // Detach from any controlling terminal so `/dev/tty` cannot be opened: crossterm then talks
    // only to the PTY on stdin/stdout, never to a developer's real terminal.
    setsid().expect("detach lifecycle probe from its controlling terminal");

    match mode {
        ProbeMode::PanicUpdate
        | ProbeMode::PanicPreStartup
        | ProbeMode::PanicStartup
        | ProbeMode::PanicPostStartup
        | ProbeMode::PanicPreUpdate
        | ProbeMode::PanicPostUpdate
        | ProbeMode::PanicLast
        | ProbeMode::PanicRunner
        | ProbeMode::PanicOffThread => run_app(
            AppAction::Panic(mode.panic_phase().unwrap()),
            AppSettings::default(),
        ),
        ProbeMode::CustomHookPanic => {
            install_previous_hook_sentinel();
            run_app(AppAction::Panic(PanicPhase::Update), AppSettings::default());
        }
        ProbeMode::PreviousHookPanics => {
            panic::set_hook(Box::new(|_| {
                eprintln!("previous panic hook panicked");
                panic!("previous panic hook panicked");
            }));
            run_app(AppAction::Panic(PanicPhase::Update), AppSettings::default());
        }
        ProbeMode::Exit => run_app(AppAction::Exit, AppSettings::default()),
        ProbeMode::Drop => run_app(AppAction::None, AppSettings::default()),
        ProbeMode::DirectDrop => {
            let context = RatatuiContext::init().expect("acquire direct terminal session");
            drop(context);
        }
        ProbeMode::Sequential => {
            install_previous_hook_sentinel();
            for _ in 0..SEQUENTIAL_SESSION_COUNT {
                run_app(AppAction::None, AppSettings::default());
            }
            panic!("panic after sequential lifecycle probes");
        }
        ProbeMode::SequentialHooks => {
            panic::set_hook(Box::new(|_| eprintln!("first-session-hook-called")));
            run_app(AppAction::None, AppSettings::default());
            panic::set_hook(Box::new(|_| eprintln!("second-session-hook-called")));
            run_app(AppAction::None, AppSettings::default());
            panic!("panic after sequential hook probes");
        }
        ProbeMode::AlreadyRaw => {
            enable_raw_mode().expect("enable caller-owned raw mode");
            let rejected = matches!(
                RatatuiContext::init().map(drop),
                Err(error) if error.downcast_ref::<SessionError>().is_some_and(|error| {
                    matches!(error, SessionError::RawModeOwnedElsewhere)
                })
            );
            let preserved = is_raw_mode_enabled().expect("query caller-owned raw mode");
            disable_raw_mode().expect("restore caller-owned raw mode");
            println!("already-raw-preserved={}", rejected && preserved);
        }
        ProbeMode::Nested => {
            let first = RatatuiContext::init().expect("acquire first session");
            let rejected = matches!(
                RatatuiContext::init().map(drop),
                Err(error) if error.downcast_ref::<SessionError>().is_some_and(|error| {
                    matches!(error, SessionError::AlreadyActive)
                })
            );
            assert!(
                first.is_active(),
                "nested acquisition disturbed the first session"
            );
            assert!(
                is_raw_mode_enabled().expect("first session still owns raw mode"),
                "nested acquisition disturbed the first session"
            );
            drop(first);
            println!("nested-session-rejected={rejected}");
        }
        ProbeMode::NonTty => {
            let rejected = RatatuiContext::init().is_err();
            println!("non-tty-rejected={rejected}");
        }
        #[cfg(feature = "mouse")]
        ProbeMode::Mouse => run_app(
            AppAction::None,
            AppSettings {
                mouse: true,
                expected_mouse: Some(true),
                ..Default::default()
            },
        ),
        ProbeMode::KittySupported => run_app(
            AppAction::None,
            AppSettings {
                kitty: true,
                expected_kitty: Some(true),
                ..Default::default()
            },
        ),
        ProbeMode::KittyUnsupported | ProbeMode::KittyTimeout => run_app(
            AppAction::None,
            AppSettings {
                kitty: true,
                expected_kitty: Some(false),
                ..Default::default()
            },
        ),
    }

    let restored = !is_raw_mode_enabled().expect("query raw mode after app cleanup");
    println!("raw-mode-restored={restored}");
}

fn install_previous_hook_sentinel() {
    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        eprintln!("previous-panic-hook-called");
        previous_hook(panic_info);
    }));
}

fn run_app(action: AppAction, settings: AppSettings) {
    let mut app = App::new();
    let runner = match action {
        AppAction::Exit => ScheduleRunnerPlugin::run_loop(Duration::from_millis(1)),
        AppAction::None | AppAction::Panic(_) => ScheduleRunnerPlugin::run_once(),
    };
    app.add_plugins((
        MinimalPlugins.set(runner),
        RatatuiPlugins {
            enable_kitty_protocol: settings.kitty,
            enable_mouse_capture: settings.mouse,
            enable_input_forwarding: false,
        },
    ));

    if let Some(expected) = settings.expected_kitty {
        app.add_systems(Startup, move |kitty: Option<Res<KittyEnabled>>| {
            assert_eq!(kitty.is_some(), expected, "unexpected KittyEnabled state");
        });
    }

    #[cfg(feature = "mouse")]
    if let Some(expected) = settings.expected_mouse {
        app.add_systems(Startup, move |mouse: Option<Res<MouseEnabled>>| {
            assert_eq!(mouse.is_some(), expected, "unexpected MouseEnabled state");
        });
    }
    #[cfg(not(feature = "mouse"))]
    let _ = settings.expected_mouse;

    // Every app draws once so the terminal is in the state a real application leaves it in.
    app.add_systems(Update, draw_system);

    match action {
        AppAction::None => {}
        AppAction::Exit => {
            app.add_systems(Update, exit_system.after(draw_system));
        }
        AppAction::Panic(PanicPhase::PreStartup) => {
            app.add_systems(PreStartup, panic_system.after(ContextSetup));
        }
        AppAction::Panic(PanicPhase::Startup) => {
            app.add_systems(Startup, panic_system);
        }
        AppAction::Panic(PanicPhase::PostStartup) => {
            app.add_systems(PostStartup, panic_system);
        }
        AppAction::Panic(PanicPhase::PreUpdate) => {
            app.add_systems(PreUpdate, panic_system);
        }
        AppAction::Panic(PanicPhase::Update) => {
            app.add_systems(Update, panic_system.after(draw_system));
        }
        AppAction::Panic(PanicPhase::PostUpdate) => {
            app.add_systems(PostUpdate, panic_system);
        }
        AppAction::Panic(PanicPhase::Last) => {
            app.add_systems(Last, panic_system);
        }
        AppAction::Panic(PanicPhase::Runner) => {
            app.set_runner(|mut app| {
                app.update();
                panic!("lifecycle panic probe");
            });
        }
        AppAction::Panic(PanicPhase::OffThread) => {
            app.add_systems(Update, off_thread_panic_system.after(draw_system));
        }
    }

    app.run();
}

fn draw_system(mut context: ResMut<RatatuiContext>) -> Result {
    context.draw(|frame| {
        frame.render_widget(ratatui::text::Text::raw("lifecycle probe"), frame.area());
    })?;
    Ok(())
}

fn panic_system() {
    panic!("lifecycle panic probe");
}

fn off_thread_panic_system(mut context: ResMut<RatatuiContext>) -> Result {
    let _ = thread::spawn(|| panic!("lifecycle panic probe")).join();
    // The hook restored the terminal on the other thread; this session must now be inactive and
    // refuse to draw. The returned error ends the app through Bevy's default error handler.
    let inactive = !context.is_active();
    println!("session-inactive-after-off-thread-panic={inactive}");
    context.draw(|_| {})?;
    Ok(())
}

fn exit_system(mut exit: MessageWriter<AppExit>) {
    exit.write_default();
}

fn run_probe(test_name: &str, mode: ProbeMode, options: ProbeOptions) -> ProbeResult {
    let pty = open_lifecycle_pty();

    if options.sentinel_termios {
        let mut attributes = tcgetattr(&pty.master).expect("read PTY attributes for sentinel");
        attributes.local_flags.toggle(LocalFlags::ECHO);
        tcsetattr(&pty.master, SetArg::TCSANOW, &attributes)
            .expect("install sentinel PTY attributes");
    }
    let original_attributes = tcgetattr(&pty.master).expect("read initial terminal attributes");

    let reader = File::from(pty.master.try_clone().expect("clone PTY reader"));
    let responder = options
        .terminal_response
        .bytes()
        .map(|_| File::from(pty.master.try_clone().expect("clone PTY responder")));
    let (output_sender, output_receiver) = mpsc::sync_channel(1);
    let reader_thread = thread::spawn(move || {
        output_sender
            .send(read_pty_output(
                reader,
                responder,
                options.terminal_response,
            ))
            .expect("send PTY output");
    });

    let mut child = Command::new(env::current_exe().expect("find test executable"))
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(MODE_ENV, mode.as_str())
        .env("RUST_BACKTRACE", if options.backtrace { "1" } else { "0" })
        .stdin(Stdio::from(
            pty.slave.try_clone().expect("clone PTY for stdin"),
        ))
        .stdout(Stdio::from(
            pty.slave.try_clone().expect("clone PTY for stdout"),
        ))
        .stderr(Stdio::from(pty.slave))
        .spawn()
        .expect("spawn lifecycle probe");

    let (status, timed_out) = wait_for_child(&mut child);
    let final_attributes = tcgetattr(&pty.master);
    let terminal_attributes_restored = final_attributes
        .as_ref()
        .is_ok_and(|current| stable_terminal_attributes_equal(&original_attributes, current));
    let terminal_attributes = format!("before={original_attributes:?}, after={final_attributes:?}");
    drop(pty.master);

    let (output, read_result) = output_receiver
        .recv_timeout(OUTPUT_COLLECTION_TIMEOUT)
        .expect("collect lifecycle probe output");
    reader_thread.join().expect("join PTY reader");
    if let Err(error) = read_result {
        // Linux reports EIO once the slave side is closed; macOS reports EOF instead.
        assert_eq!(
            error.raw_os_error(),
            Some(Errno::EIO as i32),
            "read lifecycle output ({status:?}): {}",
            escaped(&output)
        );
    }
    assert!(
        !timed_out,
        "lifecycle probe timed out after {CHILD_TIMEOUT:?}: {}",
        escaped(&output)
    );

    ProbeResult {
        output,
        status,
        terminal_attributes_restored,
        terminal_attributes,
    }
}

fn open_lifecycle_pty() -> OpenptyResult {
    const MAX_ATTEMPTS: usize = 4;
    const RETRY_DELAY: Duration = Duration::from_millis(25);

    let winsize = Winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    for attempt in 1..=MAX_ATTEMPTS {
        match openpty(Some(&winsize), None) {
            Ok(pty) => {
                // `openpty` returns inheritable descriptors. Without close-on-exec, a sibling
                // test's child spawned in this window would inherit our slave and keep the
                // master from seeing EOF until that unrelated child exits.
                for fd in [&pty.master, &pty.slave] {
                    fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))
                        .expect("mark PTY descriptor close-on-exec");
                }
                return pty;
            }
            Err(error)
                if attempt < MAX_ATTEMPTS
                    && matches!(
                        error,
                        Errno::UnknownErrno | Errno::EAGAIN | Errno::EMFILE | Errno::ENFILE
                    ) =>
            {
                // Parallel probes can briefly contend for PTYs or descriptor slots. A bounded retry
                // keeps that infrastructure noise from hiding lifecycle failures while persistent
                // allocation errors still fail.
                thread::sleep(RETRY_DELAY);
            }
            Err(error) => {
                panic!("create pseudo-terminal after {attempt} attempt(s): {error}");
            }
        }
    }

    unreachable!("the PTY attempt loop always returns or panics")
}

fn read_pty_output(
    mut reader: File,
    mut responder: Option<File>,
    response: TerminalResponse,
) -> (Vec<u8>, io::Result<()>) {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    let mut responded = false;

    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return (output, Ok(())),
            Ok(read) => {
                if output.len() + read > MAX_OUTPUT_BYTES {
                    return (
                        output,
                        Err(io::Error::other(format!(
                            "lifecycle output exceeded {MAX_OUTPUT_BYTES} bytes"
                        ))),
                    );
                }
                output.extend_from_slice(&buffer[..read]);

                if !responded && contains(&output, QUERY_KITTY) {
                    let Some(bytes) = response.bytes() else {
                        continue;
                    };
                    let terminal = responder.as_mut().expect("kitty responder handle");
                    if let Err(error) = terminal.write_all(bytes).and_then(|()| terminal.flush()) {
                        return (output, Err(error));
                    }
                    responded = true;
                }
            }
            Err(error) => return (output, Err(error)),
        }
    }
}

fn stable_terminal_attributes_equal(before: &Termios, after: &Termios) -> bool {
    let mut before_local = before.local_flags;
    let mut after_local = after.local_flags;

    // These are kernel-maintained state indicators, not restorable configuration. In particular,
    // macOS can transiently add PENDIN after raw-mode input is reprocessed.
    #[cfg(not(any(target_os = "redox", target_os = "cygwin")))]
    {
        before_local.remove(LocalFlags::PENDIN);
        after_local.remove(LocalFlags::PENDIN);
    }
    #[cfg(not(target_os = "redox"))]
    {
        before_local.remove(LocalFlags::FLUSHO);
        after_local.remove(LocalFlags::FLUSHO);
    }

    before.input_flags == after.input_flags
        && before.output_flags == after.output_flags
        && before.control_flags == after.control_flags
        && before_local == after_local
        && before.control_chars == after.control_chars
        && cfgetispeed(before) == cfgetispeed(after)
        && cfgetospeed(before) == cfgetospeed(after)
        && line_discipline_equal(before, after)
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "haiku"))]
fn line_discipline_equal(before: &Termios, after: &Termios) -> bool {
    before.line_discipline == after.line_discipline
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "haiku")))]
fn line_discipline_equal(_before: &Termios, _after: &Termios) -> bool {
    true
}

fn run_plain_probe(test_name: &str, mode: ProbeMode) -> (ExitStatus, Vec<u8>) {
    let mut child = Command::new(env::current_exe().expect("find test executable"))
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(MODE_ENV, mode.as_str())
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn plain lifecycle probe");

    let stdout = child.stdout.take().expect("capture child stdout");
    let stderr = child.stderr.take().expect("capture child stderr");
    let stdout_thread = thread::spawn(move || read_capped(stdout));
    let stderr_thread = thread::spawn(move || read_capped(stderr));
    let (status, timed_out) = wait_for_child(&mut child);

    let (mut output, stdout_result) = stdout_thread.join().expect("join stdout reader");
    let (stderr, stderr_result) = stderr_thread.join().expect("join stderr reader");
    output.extend_from_slice(&stderr);
    stdout_result.expect("read plain probe stdout");
    stderr_result.expect("read plain probe stderr");
    assert!(
        !timed_out,
        "plain lifecycle probe timed out after {CHILD_TIMEOUT:?}: {}",
        escaped(&output)
    );
    (status, output)
}

fn read_capped(mut reader: impl Read) -> (Vec<u8>, io::Result<()>) {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return (output, Ok(())),
            Ok(read) if output.len() + read <= MAX_OUTPUT_BYTES => {
                output.extend_from_slice(&buffer[..read]);
            }
            Ok(_) => {
                return (
                    output,
                    Err(io::Error::other(format!(
                        "lifecycle output exceeded {MAX_OUTPUT_BYTES} bytes"
                    ))),
                );
            }
            Err(error) => return (output, Err(error)),
        }
    }
}

fn wait_for_child(child: &mut Child) -> (ExitStatus, bool) {
    let deadline = Instant::now() + CHILD_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("poll lifecycle probe") {
            return (status, false);
        }
        if Instant::now() >= deadline {
            child.kill().expect("kill timed-out lifecycle probe");
            return (child.wait().expect("wait for killed lifecycle probe"), true);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn assert_probe_succeeded(probe: &ProbeResult) {
    assert!(
        probe.status.success(),
        "lifecycle probe failed: {}",
        probe_diagnostics(probe)
    );
}

fn assert_probe_failed(probe: &ProbeResult) {
    assert!(
        !probe.status.success(),
        "panic probe unexpectedly succeeded: {}",
        probe_diagnostics(probe)
    );
}

fn assert_terminal_attributes_restored(probe: &ProbeResult) {
    assert!(
        probe.terminal_attributes_restored,
        "the lifecycle changed PTY attributes: {}",
        probe_diagnostics(probe)
    );
}

fn assert_before(probe: &ProbeResult, before: &[u8], after: &[u8]) {
    let before_position = find(&probe.output, before);
    let after_position = find(&probe.output, after);
    assert!(
        before_position < after_position,
        "expected {:?} before {:?}: {}",
        escaped(before),
        escaped(after),
        probe_diagnostics(probe)
    );
}

fn probe_diagnostics(probe: &ProbeResult) -> String {
    format!(
        "status={:?}, signal={:?}, termios_restored={}, {}, output={}",
        probe.status,
        probe.status.signal(),
        probe.terminal_attributes_restored,
        probe.terminal_attributes,
        escaped(&probe.output)
    )
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn find(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap_or_else(|| panic!("missing {:?} in {}", escaped(needle), escaped(haystack)))
}

fn escaped(output: &[u8]) -> String {
    String::from_utf8_lossy(output).escape_debug().to_string()
}
