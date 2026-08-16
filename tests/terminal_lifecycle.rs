//! End-to-end lifecycle tests that run the application in an isolated pseudo-terminal.
//!
//! Each test launches a fresh copy of this test binary so global panic hooks and Crossterm state
//! cannot leak between scenarios. The parent process captures the actual terminal escape stream
//! and, on Unix, compares the pseudo-terminal's complete termios state before and after the child.

#![cfg(all(unix, feature = "crossterm", not(feature = "windowed")))]

use std::{
    env,
    fs::File,
    io::Read,
    panic,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use bevy::{
    app::{AppExit, ScheduleRunnerPlugin},
    prelude::*,
};
use bevy_ratatui::{RatatuiContext, RatatuiPlugins};
use nix::{
    errno::Errno,
    pty::{Winsize, openpty},
    sys::termios::tcgetattr,
};
use ratatui::crossterm::terminal::is_raw_mode_enabled;

const MODE_ENV: &str = "BEVY_RATATUI_LIFECYCLE_TEST_MODE";
const LEAVE_ALTERNATE_SCREEN: &[u8] = b"\x1b[?1049l";
const PANIC_MESSAGE: &[u8] = b"lifecycle panic probe";
const FINAL_PANIC_MESSAGE: &[u8] = b"panic after sequential lifecycle probes";
const PREVIOUS_HOOK_MESSAGE: &[u8] = b"previous-panic-hook-called";
const RAW_MODE_RESTORED: &[u8] = b"raw-mode-restored=true";
const CHILD_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProbeMode {
    Panic,
    Exit,
    Drop,
    DirectDrop,
    Sequential,
}

#[derive(Clone, Copy)]
enum AppAction {
    None,
    Panic,
    Exit,
}

impl ProbeMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Panic => "panic",
            Self::Exit => "exit",
            Self::Drop => "drop",
            Self::DirectDrop => "direct-drop",
            Self::Sequential => "sequential",
        }
    }
}

struct ProbeResult {
    output: Vec<u8>,
    success: bool,
    terminal_attributes_restored: bool,
}

/// Before the lifecycle change, `RatatuiContext::drop` emitted a second cleanup after the panic
/// hook, which could overwrite panic output. Cleanup must now finish before the prior hook prints.
#[test]
fn panic_cleanup_precedes_panic_output() {
    if run_child_if_requested(ProbeMode::Panic) {
        return;
    }

    let probe = run_probe("panic_cleanup_precedes_panic_output", ProbeMode::Panic);
    assert!(!probe.success, "panic probe unexpectedly succeeded");
    assert_terminal_attributes_restored(&probe);

    let cleanup = find(&probe.output, LEAVE_ALTERNATE_SCREEN);
    let panic = find(&probe.output, PANIC_MESSAGE);
    assert!(
        cleanup < panic,
        "terminal cleanup did not precede panic output: {}",
        escaped(&probe.output)
    );
    assert_eq!(
        count(&probe.output, LEAVE_ALTERNATE_SCREEN),
        1,
        "cleanup ran again after the panic hook: {}",
        escaped(&probe.output)
    );
}

/// A panic used to emit `LeaveAlternateScreen` twice: once from the hook and once from `Drop`.
#[test]
fn panic_leaves_alternate_screen_exactly_once() {
    if run_child_if_requested(ProbeMode::Panic) {
        return;
    }

    let probe = run_probe(
        "panic_leaves_alternate_screen_exactly_once",
        ProbeMode::Panic,
    );
    assert!(!probe.success, "panic probe unexpectedly succeeded");
    assert_terminal_attributes_restored(&probe);
    assert_eq!(
        count(&probe.output, LEAVE_ALTERNATE_SCREEN),
        1,
        "panic cleanup must leave the alternate screen once: {}",
        escaped(&probe.output)
    );
}

/// A normal Bevy `AppExit` must still restore the terminal exactly once.
#[test]
fn graceful_app_exit_restores_exactly_once() {
    if run_child_if_requested(ProbeMode::Exit) {
        return;
    }

    let probe = run_probe("graceful_app_exit_restores_exactly_once", ProbeMode::Exit);
    assert_probe_succeeded(&probe);
    assert_eq!(
        count(&probe.output, LEAVE_ALTERNATE_SCREEN),
        1,
        "graceful exit must clean up once: {}",
        escaped(&probe.output)
    );
}

/// Custom and run-once runners may return without sending `AppExit`; app ownership must still clean
/// up the terminal when the runner drops its app.
#[test]
fn app_drop_without_app_exit_restores_exactly_once() {
    if run_child_if_requested(ProbeMode::Drop) {
        return;
    }

    let probe = run_probe(
        "app_drop_without_app_exit_restores_exactly_once",
        ProbeMode::Drop,
    );
    assert_probe_succeeded(&probe);
    assert_eq!(
        count(&probe.output, LEAVE_ALTERNATE_SCREEN),
        1,
        "dropping the app must clean up once: {}",
        escaped(&probe.output)
    );
}

/// Finished apps must reinstate the hook they replaced. Otherwise a later panic replays cleanup
/// from every earlier app in the process.
#[test]
fn sequential_apps_do_not_leave_stale_cleanup_hooks() {
    if run_child_if_requested(ProbeMode::Sequential) {
        return;
    }

    let probe = run_probe(
        "sequential_apps_do_not_leave_stale_cleanup_hooks",
        ProbeMode::Sequential,
    );
    assert!(
        !probe.success,
        "sequential panic probe unexpectedly succeeded"
    );
    assert!(
        contains(&probe.output, FINAL_PANIC_MESSAGE),
        "final panic was not observed: {}",
        escaped(&probe.output)
    );
    assert_eq!(
        count(&probe.output, PREVIOUS_HOOK_MESSAGE),
        1,
        "the caller's panic hook was not restored exactly once: {}",
        escaped(&probe.output)
    );
    assert_eq!(
        count(&probe.output, LEAVE_ALTERNATE_SCREEN),
        2,
        "the final panic replayed a stale terminal hook: {}",
        escaped(&probe.output)
    );
}

/// Cleanup must restore both Crossterm's raw-mode state and the terminal attributes that existed
/// before the app started.
#[test]
fn terminal_attributes_are_restored_after_the_session() {
    if run_child_if_requested(ProbeMode::Drop) {
        return;
    }

    let probe = run_probe(
        "terminal_attributes_are_restored_after_the_session",
        ProbeMode::Drop,
    );
    assert_probe_succeeded(&probe);
    assert!(
        contains(&probe.output, RAW_MODE_RESTORED),
        "Crossterm still reported raw mode after cleanup: {}",
        escaped(&probe.output)
    );
    assert!(
        probe.terminal_attributes_restored,
        "the pseudo-terminal attributes changed during the session: {}",
        escaped(&probe.output)
    );
}

/// A directly initialized context owns its cleanup token until it is dropped, without a separate
/// plugin session that could restore the terminal a second time.
#[test]
fn direct_context_drop_restores_exactly_once() {
    if run_child_if_requested(ProbeMode::DirectDrop) {
        return;
    }

    let probe = run_probe(
        "direct_context_drop_restores_exactly_once",
        ProbeMode::DirectDrop,
    );
    assert_probe_succeeded(&probe);
    assert_terminal_attributes_restored(&probe);
    assert_eq!(
        count(&probe.output, LEAVE_ALTERNATE_SCREEN),
        1,
        "dropping a directly initialized context must clean up once: {}",
        escaped(&probe.output)
    );
}

fn run_child_if_requested(expected: ProbeMode) -> bool {
    let Ok(mode) = env::var(MODE_ENV) else {
        return false;
    };

    assert_eq!(mode, expected.as_str());
    run_child(expected);
    true
}

fn run_child(mode: ProbeMode) {
    match mode {
        ProbeMode::Panic => run_app(AppAction::Panic),
        ProbeMode::Exit => run_app(AppAction::Exit),
        ProbeMode::Drop => run_app(AppAction::None),
        ProbeMode::DirectDrop => {
            let context = RatatuiContext::init().expect("initialize direct terminal context");
            drop(context);
        }
        ProbeMode::Sequential => {
            let previous_hook = panic::take_hook();
            panic::set_hook(Box::new(move |panic_info| {
                eprintln!("previous-panic-hook-called");
                previous_hook(panic_info);
            }));
            run_app(AppAction::None);
            run_app(AppAction::None);
            panic!("panic after sequential lifecycle probes");
        }
    }

    let restored = !is_raw_mode_enabled().expect("query raw mode after app cleanup");
    println!("raw-mode-restored={restored}");
}

fn run_app(action: AppAction) {
    let mut app = App::new();
    let runner = match action {
        AppAction::Exit => ScheduleRunnerPlugin::run_loop(Duration::from_millis(1)),
        AppAction::None | AppAction::Panic => ScheduleRunnerPlugin::run_once(),
    };
    app.add_plugins((
        MinimalPlugins.set(runner),
        RatatuiPlugins {
            enable_kitty_protocol: false,
            enable_mouse_capture: false,
            enable_input_forwarding: false,
        },
    ));

    match action {
        AppAction::None => {}
        AppAction::Panic => {
            app.add_systems(Update, panic_system);
        }
        AppAction::Exit => {
            app.add_systems(Update, exit_system);
        }
    }

    app.run();
}

fn panic_system() {
    panic!("lifecycle panic probe");
}

fn exit_system(mut exit: MessageWriter<AppExit>) {
    exit.write_default();
}

fn run_probe(test_name: &str, mode: ProbeMode) -> ProbeResult {
    let pty = openpty(
        Some(&Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }),
        None,
    )
    .expect("create pseudo-terminal");
    let original_attributes = tcgetattr(&pty.master).expect("read initial terminal attributes");

    let mut reader = File::from(pty.master.try_clone().expect("clone PTY reader"));
    let (output_sender, output_receiver) = mpsc::sync_channel(1);
    let reader_thread = thread::spawn(move || {
        let mut output = Vec::new();
        let result = reader.read_to_end(&mut output);
        output_sender
            .send((output, result))
            .expect("send PTY output");
    });

    let mut child = Command::new(env::current_exe().expect("find test executable"))
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(MODE_ENV, mode.as_str())
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::from(
            pty.slave.try_clone().expect("clone PTY for stdin"),
        ))
        .stdout(Stdio::from(
            pty.slave.try_clone().expect("clone PTY for stdout"),
        ))
        .stderr(Stdio::from(pty.slave))
        .spawn()
        .expect("spawn lifecycle probe");

    let deadline = Instant::now() + CHILD_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll lifecycle probe") {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().expect("kill timed-out lifecycle probe");
            let _ = child.wait();
            panic!("lifecycle probe timed out after {CHILD_TIMEOUT:?}");
        }
        thread::sleep(Duration::from_millis(10));
    };

    let terminal_attributes_restored = tcgetattr(&pty.master).as_ref() == Ok(&original_attributes);
    drop(pty.master);

    let (output, read_result) = output_receiver
        .recv_timeout(Duration::from_secs(1))
        .expect("collect lifecycle probe output");
    reader_thread.join().expect("join PTY reader");
    if let Err(error) = read_result {
        assert_eq!(
            error.raw_os_error(),
            Some(Errno::EIO as i32),
            "read lifecycle probe output"
        );
    }

    ProbeResult {
        output,
        success: status.success(),
        terminal_attributes_restored,
    }
}

fn assert_probe_succeeded(probe: &ProbeResult) {
    assert!(
        probe.success,
        "lifecycle probe failed: {}",
        escaped(&probe.output)
    );
}

fn assert_terminal_attributes_restored(probe: &ProbeResult) {
    assert!(
        probe.terminal_attributes_restored,
        "the panic path changed the pseudo-terminal attributes: {}",
        escaped(&probe.output)
    );
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
        .unwrap_or_else(|| panic!("missing {:?} in {}", needle, escaped(haystack)))
}

fn escaped(output: &[u8]) -> String {
    String::from_utf8_lossy(output).escape_debug().to_string()
}
