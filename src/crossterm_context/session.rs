//! Exclusive ownership of the process terminal. See [`CrosstermSession`] for the model.
use std::{
    fmt,
    io::{self, Stdout, Write, stdout},
    ops::{Deref, DerefMut},
    panic::{self, PanicHookInfo},
    sync::{
        Arc, Mutex, TryLockError, Weak,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    thread,
};

#[cfg(feature = "mouse")]
use ratatui::crossterm::event::EnableMouseCapture;
use ratatui::{
    CompletedFrame, Frame, Terminal,
    backend::CrosstermBackend,
    crossterm::{
        ExecutableCommand, cursor,
        event::{
            DisableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
            PushKeyboardEnhancementFlags,
        },
        terminal::{
            EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
            is_raw_mode_enabled, supports_keyboard_enhancement,
        },
    },
};

/// Options chosen before a session is acquired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionOptions {
    /// Enable mouse capture for the duration of the session.
    #[cfg(feature = "mouse")]
    pub mouse_capture: bool,
    /// Push the kitty keyboard enhancement flags if the terminal supports them.
    pub kitty_keyboard: bool,
    /// Install a panic hook that restores the terminal before the previous hook prints.
    pub panic_hook: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            #[cfg(feature = "mouse")]
            mouse_capture: false,
            kitty_keyboard: true,
            panic_hook: true,
        }
    }
}

/// The capabilities a session acquired. Fixed once the session exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Acquired {
    /// Raw mode was enabled by this session.
    pub raw_mode: bool,
    /// The alternate screen was entered (or may have been entered) by this session.
    pub alternate_screen: bool,
    /// Mouse capture was enabled (or may have been enabled) by this session. Always `false`
    /// without the `mouse` feature.
    pub mouse_capture: bool,
    /// The kitty keyboard enhancement flags were pushed (or may have been pushed) by this session.
    pub kitty_keyboard: bool,
}

/// A step of terminal acquisition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupStep {
    /// Querying whether raw mode is already enabled.
    QueryRawMode,
    /// Enabling raw mode.
    EnableRawMode,
    /// Entering the alternate screen.
    EnterAlternateScreen,
    /// Constructing the Ratatui terminal.
    CreateTerminal,
    /// Enabling mouse capture.
    EnableMouseCapture,
    /// Pushing the kitty keyboard enhancement flags.
    PushKittyKeyboard,
}

impl fmt::Display for SetupStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::QueryRawMode => "query raw mode",
            Self::EnableRawMode => "enable raw mode",
            Self::EnterAlternateScreen => "enter the alternate screen",
            Self::CreateTerminal => "create the terminal",
            Self::EnableMouseCapture => "enable mouse capture",
            Self::PushKittyKeyboard => "push the kitty keyboard enhancement flags",
        })
    }
}

/// A step of terminal restoration, in the order the steps run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreStep {
    /// Popping the kitty keyboard enhancement flags.
    PopKittyKeyboard,
    /// Disabling mouse capture.
    DisableMouseCapture,
    /// Leaving the alternate screen.
    LeaveAlternateScreen,
    /// Showing the cursor.
    ShowCursor,
    /// Disabling raw mode.
    DisableRawMode,
}

impl fmt::Display for RestoreStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PopKittyKeyboard => "pop the kitty keyboard enhancement flags",
            Self::DisableMouseCapture => "disable mouse capture",
            Self::LeaveAlternateScreen => "leave the alternate screen",
            Self::ShowCursor => "show the cursor",
            Self::DisableRawMode => "disable raw mode",
        })
    }
}

/// Why a session could not be acquired.
#[derive(Debug)]
#[non_exhaustive]
pub enum SessionError {
    /// Another session in this process currently owns the terminal.
    AlreadyActive,
    /// `acquire` was called on a panicking thread, where `std` forbids installing the panic hook.
    ThreadPanicking,
    /// Crossterm already reports raw mode for this process: either other code enabled it, or a
    /// previous session failed to disable it. Acquiring would later disable that owner's raw mode.
    RawModeOwnedElsewhere,
    /// A setup step failed. Recorded capabilities were rolled back.
    Setup {
        /// The step that failed.
        step: SetupStep,
        /// The underlying error.
        source: io::Error,
        /// Errors from rolling back the capabilities acquired before the failure, if any.
        rollback: Option<RestoreError>,
    },
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyActive => {
                f.write_str("another terminal session in this process is already active")
            }
            Self::ThreadPanicking => f.write_str(
                "cannot acquire a terminal session on a panicking thread: the panic hook cannot \
                 be installed",
            ),
            Self::RawModeOwnedElsewhere => f.write_str(
                "the terminal is already in raw mode (enabled by other code in this process, \
                 or left behind by a session whose cleanup failed)",
            ),
            Self::Setup {
                step,
                source,
                rollback,
            } => {
                write!(f, "failed to {step}: {source}")?;
                if let Some(rollback) = rollback {
                    write!(f, "; rollback also failed: {rollback}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Setup { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Restoration did not complete cleanly.
///
/// Either one or more steps failed (every applicable step was still attempted), or an earlier
/// restoration attempt panicked part-way, in which case the terminal state is unknown and is not
/// retried.
#[derive(Debug)]
pub struct RestoreError {
    failures: Vec<(RestoreStep, io::Error)>,
    interrupted: bool,
}

impl RestoreError {
    fn interrupted() -> Self {
        Self {
            failures: Vec::new(),
            interrupted: true,
        }
    }

    /// Every failed step in the order the steps were attempted.
    pub fn failures(&self) -> impl Iterator<Item = (RestoreStep, &io::Error)> {
        self.failures.iter().map(|(step, error)| (*step, error))
    }

    /// Whether an earlier restoration attempt panicked part-way, leaving the terminal state
    /// unknown.
    pub fn was_interrupted(&self) -> bool {
        self.interrupted
    }
}

impl fmt::Display for RestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.interrupted {
            return f.write_str(
                "an earlier terminal restoration attempt panicked part-way; the terminal state is \
                 unknown and is not retried",
            );
        }
        for (index, (step, error)) in self.failures.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            write!(f, "failed to {step}: {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for RestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.failures
            .first()
            .map(|(_, error)| error as &(dyn std::error::Error + 'static))
    }
}

/// The terminal operations a session performs. One real implementation talks to crossterm; tests
/// substitute a recording fake so every failure and ordering can be exercised deterministically.
trait TerminalOps: Send + Sync + 'static {
    type Terminal: Send + Sync + 'static;

    fn is_raw_mode_enabled(&self) -> io::Result<bool>;
    fn enable_raw_mode(&self) -> io::Result<()>;
    fn disable_raw_mode(&self) -> io::Result<()>;
    fn enter_alternate_screen(&self) -> io::Result<()>;
    fn leave_alternate_screen(&self) -> io::Result<()>;
    fn show_cursor(&self) -> io::Result<()>;
    #[cfg(feature = "mouse")]
    fn enable_mouse_capture(&self) -> io::Result<()>;
    fn disable_mouse_capture(&self) -> io::Result<()>;
    fn supports_kitty_keyboard(&self) -> io::Result<bool>;
    fn push_kitty_keyboard(&self) -> io::Result<()>;
    fn pop_kitty_keyboard(&self) -> io::Result<()>;
    fn create_terminal(&self) -> io::Result<Self::Terminal>;
    fn take_hook(&self) -> PanicHook;
    fn set_hook(&self, hook: PanicHook);
}

type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static>;

struct SystemOps;

impl TerminalOps for SystemOps {
    type Terminal = Terminal<CrosstermBackend<Stdout>>;

    fn is_raw_mode_enabled(&self) -> io::Result<bool> {
        is_raw_mode_enabled()
    }

    fn enable_raw_mode(&self) -> io::Result<()> {
        enable_raw_mode()
    }

    fn disable_raw_mode(&self) -> io::Result<()> {
        disable_raw_mode()
    }

    fn enter_alternate_screen(&self) -> io::Result<()> {
        stdout().execute(EnterAlternateScreen).map(|_| ())
    }

    fn leave_alternate_screen(&self) -> io::Result<()> {
        stdout().execute(LeaveAlternateScreen).map(|_| ())
    }

    fn show_cursor(&self) -> io::Result<()> {
        stdout().execute(cursor::Show).map(|_| ())
    }

    #[cfg(feature = "mouse")]
    fn enable_mouse_capture(&self) -> io::Result<()> {
        stdout().execute(EnableMouseCapture).map(|_| ())
    }

    fn disable_mouse_capture(&self) -> io::Result<()> {
        stdout().execute(DisableMouseCapture).map(|_| ())
    }

    fn supports_kitty_keyboard(&self) -> io::Result<bool> {
        supports_keyboard_enhancement()
    }

    fn push_kitty_keyboard(&self) -> io::Result<()> {
        stdout()
            .execute(PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::all()))
            .map(|_| ())
    }

    fn pop_kitty_keyboard(&self) -> io::Result<()> {
        stdout().execute(PopKeyboardEnhancementFlags).map(|_| ())
    }

    fn create_terminal(&self) -> io::Result<Self::Terminal> {
        Terminal::new(CrosstermBackend::new(stdout()))
    }

    fn take_hook(&self) -> PanicHook {
        panic::take_hook()
    }

    fn set_hook(&self, hook: PanicHook) {
        panic::set_hook(hook)
    }
}

const LEASE_VACANT: u8 = 0;
const LEASE_HELD: u8 = 1;

/// The process-wide right to own the terminal. Exactly one session can hold it at a time.
struct Lease(AtomicU8);

impl Lease {
    const fn new() -> Self {
        Self(AtomicU8::new(LEASE_VACANT))
    }

    /// One atomic `Vacant -> Held` transition. Losers observe `None` without side effects.
    fn try_acquire(&'static self) -> Option<LeaseGuard> {
        self.0
            .compare_exchange(
                LEASE_VACANT,
                LEASE_HELD,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()
            .map(|_| LeaseGuard(self))
    }
}

/// Holding this value is holding the lease; dropping it is the `Held -> Vacant` transition.
struct LeaseGuard(&'static Lease);

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        self.0.0.store(LEASE_VACANT, Ordering::Release);
    }
}

static LEASE: Lease = Lease::new();

/// Where the session is in its life. Guarded by one mutex so that "decide" and "act" cannot be
/// separated by another actor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionState {
    /// Capabilities are being acquired; a rollback guard on the acquiring thread owns them.
    Acquiring,
    /// The session owns the recorded capabilities.
    Active(Acquired),
    /// Restoration has been attempted. Nothing will be written again.
    Restored,
}

/// The result of asking the state machine to restore the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestoreOutcome {
    /// This call performed the restoration.
    Restored,
    /// Restoration had already been performed (or the session never became active).
    AlreadyRestored,
    /// The emergency path could not take the state lock because another thread is restoring right
    /// now, or an earlier restoration panicked. Either way nothing may be written again.
    Skipped,
}

/// The state shared between the owning session and the panic hook's emergency handle.
struct Shared<O: TerminalOps> {
    ops: O,
    state: Mutex<SessionState>,
    /// Mirrors `state == Active`, so `is_active` (called on every draw) never contends with the
    /// emergency path for the lock.
    active: AtomicBool,
}

impl<O: TerminalOps> Shared<O> {
    /// Normal restoration. Waits for an in-flight restoration on another thread to finish so that
    /// the caller can rely on the terminal being restored when this returns.
    fn restore(&self) -> Result<RestoreOutcome, RestoreError> {
        match self.state.lock() {
            Ok(mut state) => self.restore_locked(&mut state),
            // A restoration panicked while holding the lock. It was attempted; never retry.
            Err(_poisoned) => Err(RestoreError::interrupted()),
        }
    }

    /// Emergency restoration from the panic hook. Never blocks and never panics.
    fn emergency_restore(&self) -> Result<RestoreOutcome, RestoreError> {
        match self.state.try_lock() {
            Ok(mut state) => self.restore_locked(&mut state),
            Err(TryLockError::WouldBlock | TryLockError::Poisoned(_)) => {
                Ok(RestoreOutcome::Skipped)
            }
        }
    }

    fn restore_locked(&self, state: &mut SessionState) -> Result<RestoreOutcome, RestoreError> {
        let SessionState::Active(acquired) = *state else {
            return Ok(RestoreOutcome::AlreadyRestored);
        };
        // Transition first, then act: an error or panic below must never reopen the state.
        *state = SessionState::Restored;
        self.active.store(false, Ordering::Release);
        perform_restore(&self.ops, acquired)?;
        Ok(RestoreOutcome::Restored)
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
}

/// Undoes `acquired` in cleanup order, attempting every applicable step even after an error.
fn perform_restore<O: TerminalOps>(ops: &O, acquired: Acquired) -> Result<(), RestoreError> {
    let mut failures = Vec::new();
    let mut attempt = |step: RestoreStep, result: io::Result<()>| {
        if let Err(error) = result {
            failures.push((step, error));
        }
    };

    if acquired.kitty_keyboard {
        attempt(RestoreStep::PopKittyKeyboard, ops.pop_kitty_keyboard());
    }
    if acquired.mouse_capture {
        attempt(
            RestoreStep::DisableMouseCapture,
            ops.disable_mouse_capture(),
        );
    }
    if acquired.alternate_screen {
        attempt(
            RestoreStep::LeaveAlternateScreen,
            ops.leave_alternate_screen(),
        );
        attempt(RestoreStep::ShowCursor, ops.show_cursor());
    }
    if acquired.raw_mode {
        attempt(RestoreStep::DisableRawMode, ops.disable_raw_mode());
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(RestoreError {
            failures,
            interrupted: false,
        })
    }
}

/// Undoes partially acquired capabilities when acquisition fails or panics.
struct Rollback<'a, O: TerminalOps> {
    ops: &'a O,
    acquired: Acquired,
    armed: bool,
}

impl<'a, O: TerminalOps> Rollback<'a, O> {
    fn new(ops: &'a O) -> Self {
        Self {
            ops,
            acquired: Acquired::default(),
            armed: true,
        }
    }

    /// Roll back now on the error path, so the result can be attached to the setup error.
    fn undo(&mut self) -> Result<(), RestoreError> {
        self.armed = false;
        perform_restore(self.ops, self.acquired)
    }

    /// Acquisition succeeded: hand the record to the session.
    fn disarm(mut self) -> Acquired {
        self.armed = false;
        self.acquired
    }
}

impl<O: TerminalOps> Drop for Rollback<'_, O> {
    fn drop(&mut self) {
        // Reached only when acquisition panicked; the error path calls `undo` explicitly.
        if !self.armed {
            return;
        }
        if let Err(error) = perform_restore(self.ops, self.acquired) {
            report(format_args!(
                "failed to roll back terminal setup after a panic: {error}"
            ));
        }
    }
}

/// The panic hook this session installed and the hook it replaced.
struct HookRegistration {
    previous: Arc<PanicHook>,
    installed: usize,
}

impl HookRegistration {
    /// Wraps the current hook. Must be called on a thread that is not panicking.
    fn install<O: TerminalOps>(shared: &Arc<Shared<O>>) -> Self {
        let previous = Arc::new(shared.ops.take_hook());
        let hook_previous = Arc::clone(&previous);
        let emergency: Weak<Shared<O>> = Arc::downgrade(shared);
        let wrapper: PanicHook = Box::new(move |info| {
            // Emergency handle: restore through the one state machine, then let the previous
            // hook print. Nothing here may panic or block.
            if let Some(shared) = emergency.upgrade()
                && let Err(error) = shared.emergency_restore()
            {
                report(format_args!("failed to restore the terminal: {error}"));
            }
            hook_previous(info);
        });
        let installed = hook_identity(&wrapper);
        shared.ops.set_hook(wrapper);
        Self {
            previous,
            installed,
        }
    }

    /// Reinstates the previous hook if this session's wrapper is still the installed hook. Must be
    /// called on a thread that is not panicking.
    fn uninstall<O: TerminalOps>(self, ops: &O) {
        let current = ops.take_hook();
        // The wrapper holds the second strong reference to `previous`, so a count of two means it
        // is still alive somewhere. A live allocation cannot share its address with another live
        // allocation, so "alive and same address" identifies our wrapper exactly.
        let wrapper_alive = Arc::strong_count(&self.previous) == 2;
        if !wrapper_alive || hook_identity(&current) != self.installed {
            // Someone installed a hook after us, either wrapping ours or replacing it. Leave it
            // (and, through it, our now-inert wrapper) alone.
            ops.set_hook(current);
            return;
        }
        drop(current);
        match Arc::try_unwrap(self.previous) {
            Ok(previous) => ops.set_hook(previous),
            Err(previous) => ops.set_hook(Box::new(move |info| previous(info))),
        }
    }
}

/// The address of a hook's allocation. Our wrapper captures state, so it is never a dangling
/// zero-sized pointer, and the address is stable and unique while the box is alive.
fn hook_identity(hook: &PanicHook) -> usize {
    let hook: &(dyn Fn(&PanicHookInfo<'_>) + Send + Sync) = &**hook;
    (hook as *const (dyn Fn(&PanicHookInfo<'_>) + Send + Sync))
        .cast::<()>()
        .addr()
}

/// The generic session core: everything except the concrete crossterm terminal type.
struct Session<O: TerminalOps> {
    // Field order is drop order: the terminal, then the shared state, then the hook registration,
    // and the lease last, after `Drop::drop` has restored the terminal.
    terminal: O::Terminal,
    shared: Arc<Shared<O>>,
    hook: Option<HookRegistration>,
    /// What this session acquired; fixed for its whole life, unlike the restore state.
    acquired: Acquired,
    _lease: LeaseGuard,
}

impl<O: TerminalOps> Session<O> {
    fn acquire(
        lease: &'static Lease,
        ops: O,
        options: SessionOptions,
    ) -> Result<Self, SessionError> {
        if options.panic_hook && thread::panicking() {
            return Err(SessionError::ThreadPanicking);
        }
        let lease = lease.try_acquire().ok_or(SessionError::AlreadyActive)?;

        // Policy check, not a lock: crossterm's record says other code in this process owns raw
        // mode, and disabling it later would clobber that owner.
        match ops.is_raw_mode_enabled() {
            Ok(false) => {}
            Ok(true) => return Err(SessionError::RawModeOwnedElsewhere),
            Err(source) => {
                return Err(SessionError::Setup {
                    step: SetupStep::QueryRawMode,
                    source,
                    rollback: None,
                });
            }
        }

        let shared = Arc::new(Shared {
            ops,
            state: Mutex::new(SessionState::Acquiring),
            active: AtomicBool::new(false),
        });
        let mut rollback = Rollback::new(&shared.ops);
        let terminal = match acquire_capabilities(&shared.ops, options, &mut rollback) {
            Ok(terminal) => terminal,
            Err((step, source)) => {
                let rollback = rollback.undo().err();
                return Err(SessionError::Setup {
                    step,
                    source,
                    rollback,
                });
            }
        };

        let hook = options
            .panic_hook
            .then(|| HookRegistration::install(&shared));

        let acquired = rollback.disarm();
        {
            let mut state = lock_ignoring_poison(&shared.state);
            *state = SessionState::Active(acquired);
            shared.active.store(true, Ordering::Release);
        }

        Ok(Self {
            terminal,
            shared,
            hook,
            acquired,
            _lease: lease,
        })
    }

    fn acquired(&self) -> Acquired {
        self.acquired
    }

    fn is_active(&self) -> bool {
        self.shared.is_active()
    }

    fn close(mut self) -> Result<(), RestoreError> {
        let result = self.shared.restore().map(|_| ());
        self.uninstall_hook();
        result
    }

    fn uninstall_hook(&mut self) {
        // `std` forbids `take_hook`/`set_hook` on a panicking thread. Skipping here is safe: the
        // wrapper is inert once the state is `Restored`.
        if thread::panicking() {
            return;
        }
        if let Some(hook) = self.hook.take() {
            hook.uninstall(&self.shared.ops);
        }
    }
}

fn lock_ignoring_poison<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Steps 3–7 of acquisition. Records each capability in `rollback` before attempting it.
fn acquire_capabilities<O: TerminalOps>(
    ops: &O,
    options: SessionOptions,
    rollback: &mut Rollback<'_, O>,
) -> Result<O::Terminal, (SetupStep, io::Error)> {
    ops.enable_raw_mode()
        .map_err(|error| (SetupStep::EnableRawMode, error))?;
    rollback.acquired.raw_mode = true;

    rollback.acquired.alternate_screen = true;
    ops.enter_alternate_screen()
        .map_err(|error| (SetupStep::EnterAlternateScreen, error))?;

    let terminal = ops
        .create_terminal()
        .map_err(|error| (SetupStep::CreateTerminal, error))?;

    #[cfg(feature = "mouse")]
    if options.mouse_capture {
        rollback.acquired.mouse_capture = true;
        ops.enable_mouse_capture()
            .map_err(|error| (SetupStep::EnableMouseCapture, error))?;
    }

    if options.kitty_keyboard {
        match ops.supports_kitty_keyboard() {
            Ok(true) => {
                rollback.acquired.kitty_keyboard = true;
                ops.push_kitty_keyboard()
                    .map_err(|error| (SetupStep::PushKittyKeyboard, error))?;
            }
            Ok(false) => {}
            Err(error) => {
                tracing::debug!(
                    "kitty keyboard support query failed; continuing without it: {error}"
                );
            }
        }
    }

    Ok(terminal)
}

impl<O: TerminalOps> fmt::Debug for Session<O> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("active", &self.is_active())
            .field("acquired", &self.acquired())
            .finish_non_exhaustive()
    }
}

impl<O: TerminalOps> Drop for Session<O> {
    fn drop(&mut self) {
        if let Err(error) = self.shared.restore() {
            // The terminal has been restored as far as possible, so stderr is readable again.
            report(format_args!("failed to restore the terminal: {error}"));
        }
        self.uninstall_hook();
    }
}

/// Writes a diagnostic to stderr without panicking if stderr is closed (unlike `eprintln!`), so it
/// is safe inside `Drop` during unwinding and inside the panic hook.
fn report(message: fmt::Arguments<'_>) {
    let _ = writeln!(io::stderr(), "{message}");
}

/// The exclusive, process-wide terminal session backed by crossterm.
///
/// This is the one value that owns the terminal from successful acquisition until it has been
/// restored. Bevy's [`RatatuiContext`](crate::RatatuiContext) wraps it directly, and direct users
/// construct exactly the same type with [`CrosstermSession::acquire`]. Nothing transfers cleanup
/// responsibility after construction. It dereferences to the underlying [`Terminal`] for
/// convenience; prefer [`CrosstermSession::draw`], which refuses to draw once the session has been
/// restored, because methods reached through `Deref` are not guarded.
///
/// # Ownership model
///
/// ```text
/// static LEASE                Vacant | Held        process-wide right to own the terminal
///        |
///        v  CrosstermSession::acquire(options)
/// CrosstermSession            the one owning value
///   |- Terminal<CrosstermBackend<Stdout>>
///   |- Arc<Shared>            Acquiring | Active(Acquired) | Restored, behind one Mutex
///   |- panic-hook registration (previous hook + identity of the installed wrapper)
///   `- lease guard            released after restoration has completed
///
/// panic hook                  Weak<Shared>: an emergency handle to the same state machine,
///                             never a second lifecycle owner
/// ```
///
/// # Lifecycle
///
/// 1. `acquire` takes the lease with one atomic transition. Losers get
///    [`SessionError::AlreadyActive`] and perform no terminal operation at all.
/// 2. If crossterm already reports raw mode for this process, acquisition fails with
///    [`SessionError::RawModeOwnedElsewhere`] and touches nothing. That query is a policy check
///    made after the lease is held; the lease, not the query, is the ownership primitive.
/// 3. Raw mode, the alternate screen, the terminal, mouse capture (if requested), and the kitty
///    keyboard protocol (if requested and supported) are acquired in that order. Raw mode comes
///    first so a non-TTY environment fails before any escape sequence is written, and is recorded
///    on success (crossterm's `enable_raw_mode` changes nothing when it fails). The alternate
///    screen, mouse capture, and kitty flags are recorded *before* their write is attempted, so an
///    ambiguous flush failure is conservatively undone. Any error or panic rolls back exactly the
///    recorded capabilities in cleanup order and releases the lease.
/// 4. The panic hook is installed last, wrapping the hook that was current at that moment.
/// 5. Restoration happens exactly once: the state moves to `Restored` *before* any cleanup I/O,
///    then kitty is popped, mouse capture disabled, the alternate screen left, the cursor shown,
///    and raw mode disabled, attempting every applicable step even if an earlier one fails. Explicit
///    [`CrosstermSession::close`], `Drop` (including `AppExit`, runner return, and `App`/`World`
///    drop), and the panic hook all run this same function. Cleanup is not retryable: a second
///    `LeaveAlternateScreen` would move the cursor back over output already printed, which is the
///    bug this design exists to prevent.
/// 6. Normal teardown reinstates the previous panic hook, but only if the hook that is installed at
///    that moment is still this session's wrapper. A hook installed by someone else after the
///    session started is left in place; the session's wrapper then stays in the chain and becomes
///    inert. The lease is released after all of that.
///
/// # Policies
///
/// | Situation | Behaviour |
/// | --- | --- |
/// | Second or nested session while one is active | `Err(AlreadyActive)`; the first is untouched. |
/// | Concurrent acquirers | Exactly one wins. |
/// | Raw mode already recorded by crossterm in this process | `Err(RawModeOwnedElsewhere)`; nothing changes. |
/// | Non-TTY stdio | Fails at `enable_raw_mode` with no escape output. |
/// | Setup error or panic after partial acquisition | Recorded capabilities rolled back; lease released. |
/// | Panic on any thread | The hook restores once, then the previous hook prints. |
/// | Session dropped during unwinding | Restore is a no-op (already restored); hook uninstall is skipped because `std` forbids `set_hook` on a panicking thread; the wrapper stays installed and inert. |
/// | Caught panic, app survives | The session is inactive: [`CrosstermSession::draw`] fails. Dropping it releases the lease; a new session may then be acquired. |
/// | Concurrent panics | One restores; the other proceeds to the previous hook without waiting. |
/// | Cleanup step fails | Later steps still run; the first error is reported; not retried. |
/// | Panic inside cleanup | Remaining steps are skipped; the state stays `Restored`; not retried. A later [`close`](Self::close) reports it as interrupted. |
/// | Hook installed or replaced by another thread *during* teardown | Unsupported: `std`'s hook API has no atomic swap, so teardown briefly reads and rewrites the global hook. |
/// | `acquire` on a panicking thread | `Err(ThreadPanicking)` when a panic hook is requested; nothing changes. |
///
/// After restoration the session writes nothing more. The wrapped Ratatui [`Terminal`] may still
/// show the cursor (`ESC[?25h`) when it is dropped, which does not move the cursor.
///
/// # Support boundary
///
/// `SIGKILL` and other hard termination cannot run cleanup, `std::process::exit` skips destructors,
/// no signal handlers are installed, and with `panic = "abort"` only the panic hook runs.
pub struct CrosstermSession(Session<SystemOps>);

impl CrosstermSession {
    /// Acquires the terminal: raw mode, the alternate screen, the requested optional modes, and
    /// the panic hook. Fails without side effects if another session is active or if crossterm
    /// already reports raw mode; rolls back on any other failure.
    pub fn acquire(options: SessionOptions) -> Result<Self, SessionError> {
        Session::acquire(&LEASE, SystemOps, options).map(Self)
    }

    /// The capabilities this session acquired.
    pub fn acquired(&self) -> Acquired {
        self.0.acquired()
    }

    /// Whether the terminal is still owned by this session. `false` after [`close`](Self::close),
    /// or after a panic hook restored the terminal on any thread.
    pub fn is_active(&self) -> bool {
        self.0.is_active()
    }

    /// Draws a frame, or fails if the session is no longer active.
    pub fn draw<F>(&mut self, render: F) -> io::Result<CompletedFrame<'_>>
    where
        F: FnOnce(&mut Frame),
    {
        if !self.0.is_active() {
            return Err(io::Error::other("the terminal session is no longer active"));
        }
        self.0.terminal.draw(render)
    }

    /// Restores the terminal now and reinstates the previous panic hook. Dropping the session does
    /// the same; this form reports restoration errors.
    pub fn close(self) -> Result<(), RestoreError> {
        self.0.close()
    }
}

impl Deref for CrosstermSession {
    type Target = Terminal<CrosstermBackend<Stdout>>;

    fn deref(&self) -> &Self::Target {
        &self.0.terminal
    }
}

impl DerefMut for CrosstermSession {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0.terminal
    }
}

impl fmt::Debug for CrosstermSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CrosstermSession").field(&self.0).finish()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{Arc, Barrier, Mutex},
        thread,
    };

    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    enum Op {
        QueryRawMode,
        EnableRawMode,
        DisableRawMode,
        EnterAlternateScreen,
        LeaveAlternateScreen,
        ShowCursor,
        EnableMouseCapture,
        DisableMouseCapture,
        QueryKittyKeyboard,
        PushKittyKeyboard,
        PopKittyKeyboard,
        CreateTerminal,
        TakeHook,
        SetHook,
    }

    #[derive(Clone, Copy, Debug)]
    enum Fault {
        Error,
        Panic,
    }

    /// Two-party rendezvous used to hold a thread inside one operation.
    #[derive(Clone)]
    struct Gate {
        op: Op,
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
    }

    #[derive(Default)]
    struct FakeState {
        trace: Vec<Op>,
        faults: HashMap<Op, Fault>,
        raw_mode: bool,
        kitty_supported: bool,
        hook: Option<PanicHook>,
        gate: Option<Gate>,
    }

    /// A recording terminal that can fail, panic, or block at any operation.
    #[derive(Clone, Default)]
    struct FakeOps(Arc<Mutex<FakeState>>);

    impl FakeOps {
        fn new() -> Self {
            Self::default()
        }

        fn with_kitty_support() -> Self {
            let ops = Self::new();
            ops.lock().kitty_supported = true;
            ops
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        fn trace(&self) -> Vec<Op> {
            self.lock().trace.clone()
        }

        fn clear_trace(&self) {
            self.lock().trace.clear();
        }

        fn fail_at(&self, op: Op) {
            self.lock().faults.insert(op, Fault::Error);
        }

        fn panic_at(&self, op: Op) {
            self.lock().faults.insert(op, Fault::Panic);
        }

        fn set_raw_mode(&self, enabled: bool) {
            self.lock().raw_mode = enabled;
        }

        fn raw_mode(&self) -> bool {
            self.lock().raw_mode
        }

        /// Installs `hook` as if user code had called `panic::set_hook`; returns its identity.
        fn install_user_hook(&self, hook: PanicHook) -> usize {
            let identity = hook_identity(&hook);
            self.lock().hook = Some(hook);
            identity
        }

        fn current_hook_identity(&self) -> Option<usize> {
            self.lock().hook.as_ref().map(hook_identity)
        }

        /// Blocks the next `op` until the returned gate is released.
        fn gate(&self, op: Op) -> Gate {
            let gate = Gate {
                op,
                entered: Arc::new(Barrier::new(2)),
                release: Arc::new(Barrier::new(2)),
            };
            self.lock().gate = Some(gate.clone());
            gate
        }

        fn perform(&self, op: Op) -> io::Result<()> {
            let (fault, gate) = {
                let mut state = self.lock();
                state.trace.push(op);
                let gate = match &state.gate {
                    Some(gate) if gate.op == op => state.gate.take(),
                    _ => None,
                };
                (state.faults.get(&op).copied(), gate)
            };
            if let Some(gate) = gate {
                gate.entered.wait();
                gate.release.wait();
            }
            match fault {
                None => Ok(()),
                Some(Fault::Error) => Err(io::Error::other(format!("injected failure at {op:?}"))),
                Some(Fault::Panic) => panic!("injected panic at {op:?}"),
            }
        }
    }

    impl TerminalOps for FakeOps {
        type Terminal = ();

        fn is_raw_mode_enabled(&self) -> io::Result<bool> {
            self.perform(Op::QueryRawMode)?;
            Ok(self.raw_mode())
        }

        fn enable_raw_mode(&self) -> io::Result<()> {
            self.perform(Op::EnableRawMode)?;
            self.set_raw_mode(true);
            Ok(())
        }

        fn disable_raw_mode(&self) -> io::Result<()> {
            self.perform(Op::DisableRawMode)?;
            self.set_raw_mode(false);
            Ok(())
        }

        fn enter_alternate_screen(&self) -> io::Result<()> {
            self.perform(Op::EnterAlternateScreen)
        }

        fn leave_alternate_screen(&self) -> io::Result<()> {
            self.perform(Op::LeaveAlternateScreen)
        }

        fn show_cursor(&self) -> io::Result<()> {
            self.perform(Op::ShowCursor)
        }

        #[cfg(feature = "mouse")]
        fn enable_mouse_capture(&self) -> io::Result<()> {
            self.perform(Op::EnableMouseCapture)
        }

        fn disable_mouse_capture(&self) -> io::Result<()> {
            self.perform(Op::DisableMouseCapture)
        }

        fn supports_kitty_keyboard(&self) -> io::Result<bool> {
            self.perform(Op::QueryKittyKeyboard)?;
            Ok(self.lock().kitty_supported)
        }

        fn push_kitty_keyboard(&self) -> io::Result<()> {
            self.perform(Op::PushKittyKeyboard)
        }

        fn pop_kitty_keyboard(&self) -> io::Result<()> {
            self.perform(Op::PopKittyKeyboard)
        }

        fn create_terminal(&self) -> io::Result<Self::Terminal> {
            self.perform(Op::CreateTerminal)
        }

        fn take_hook(&self) -> PanicHook {
            self.perform(Op::TakeHook).expect(
                "take_hook cannot fail; a panic fault models set_hook on a panicking thread",
            );
            self.lock().hook.take().unwrap_or_else(|| Box::new(|_| {}))
        }

        fn set_hook(&self, hook: PanicHook) {
            self.perform(Op::SetHook).expect(
                "set_hook cannot fail; a panic fault models set_hook on a panicking thread",
            );
            self.lock().hook = Some(hook);
        }
    }

    fn lease() -> &'static Lease {
        Box::leak(Box::new(Lease::new()))
    }

    fn options(kitty: bool, mouse: bool) -> SessionOptions {
        #[cfg(not(feature = "mouse"))]
        let _ = mouse;
        SessionOptions {
            #[cfg(feature = "mouse")]
            mouse_capture: mouse,
            kitty_keyboard: kitty,
            panic_hook: true,
        }
    }

    fn acquire(
        lease: &'static Lease,
        ops: &FakeOps,
        options: SessionOptions,
    ) -> Result<Session<FakeOps>, SessionError> {
        Session::acquire(lease, ops.clone(), options)
    }

    const fn mouse_enabled() -> bool {
        cfg!(feature = "mouse")
    }

    /// The trace of a complete acquisition with kitty support and (feature permitting) mouse.
    fn full_acquire_trace() -> Vec<Op> {
        let mut trace = vec![
            Op::QueryRawMode,
            Op::EnableRawMode,
            Op::EnterAlternateScreen,
            Op::CreateTerminal,
        ];
        if mouse_enabled() {
            trace.push(Op::EnableMouseCapture);
        }
        trace.extend([
            Op::QueryKittyKeyboard,
            Op::PushKittyKeyboard,
            Op::TakeHook,
            Op::SetHook,
        ]);
        trace
    }

    /// The trace of a complete restoration matching `full_acquire_trace`.
    fn full_restore_trace() -> Vec<Op> {
        let mut trace = vec![Op::PopKittyKeyboard];
        if mouse_enabled() {
            trace.push(Op::DisableMouseCapture);
        }
        trace.extend([Op::LeaveAlternateScreen, Op::ShowCursor, Op::DisableRawMode]);
        trace
    }

    fn full_acquired() -> Acquired {
        Acquired {
            raw_mode: true,
            alternate_screen: true,
            mouse_capture: mouse_enabled(),
            kitty_keyboard: true,
        }
    }

    fn setup_step(error: &SessionError) -> SetupStep {
        match error {
            SessionError::Setup { step, .. } => *step,
            other => panic!("expected a setup error, got {other:?}"),
        }
    }

    fn assert_lease_released(lease: &'static Lease, ops: &FakeOps) {
        ops.clear_trace();
        let session = acquire(lease, ops, options(false, false)).expect("lease was not released");
        drop(session);
        ops.clear_trace();
    }

    #[test]
    fn acquisition_records_every_capability_in_order_and_restores_in_reverse() {
        let ops = FakeOps::with_kitty_support();
        let session = acquire(lease(), &ops, options(true, true)).unwrap();

        assert_eq!(ops.trace(), full_acquire_trace());
        assert_eq!(session.acquired(), full_acquired());
        assert!(session.is_active());
        assert!(!ops.raw_mode() || session.acquired().raw_mode);

        ops.clear_trace();
        drop(session);
        let mut expected = full_restore_trace();
        expected.extend([Op::TakeHook, Op::SetHook]);
        assert_eq!(ops.trace(), expected);
        assert!(!ops.raw_mode());
    }

    #[test]
    fn close_reports_success_and_restores_exactly_once() {
        let ops = FakeOps::with_kitty_support();
        let session = acquire(lease(), &ops, options(true, true)).unwrap();
        ops.clear_trace();

        session.close().unwrap();

        let mut expected = full_restore_trace();
        expected.extend([Op::TakeHook, Op::SetHook]);
        assert_eq!(ops.trace(), expected, "drop after close must write nothing");
    }

    #[test]
    fn a_second_session_is_rejected_without_any_terminal_operation() {
        let lease = lease();
        let ops = FakeOps::new();
        let first = acquire(lease, &ops, options(false, false)).unwrap();
        ops.clear_trace();

        let second = acquire(lease, &ops, options(false, false));
        assert!(matches!(second, Err(SessionError::AlreadyActive)));
        assert!(ops.trace().is_empty(), "the loser touched the terminal");
        assert!(first.is_active());

        drop(first);
        assert_lease_released(lease, &ops);
    }

    #[test]
    fn concurrent_acquirers_have_exactly_one_winner() {
        let lease = lease();
        let ops = FakeOps::new();
        let start = Arc::new(Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let ops = ops.clone();
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    acquire(lease, &ops, options(false, false)).ok()
                })
            })
            .collect();
        let winners: Vec<_> = handles
            .into_iter()
            .filter_map(|handle| handle.join().unwrap())
            .collect();

        assert_eq!(winners.len(), 1);
        assert_eq!(
            ops.trace()
                .iter()
                .filter(|op| **op == Op::EnableRawMode)
                .count(),
            1
        );
        drop(winners);
        assert_lease_released(lease, &ops);
    }

    #[test]
    fn the_lease_never_has_two_holders_under_contention() {
        const THREADS: usize = 8;
        const ITERATIONS: usize = 2000;

        let lease = lease();
        let holders = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_holders = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(THREADS));

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let holders = Arc::clone(&holders);
                let max_holders = Arc::clone(&max_holders);
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    // Each thread has its own fake so the lease is the only shared state.
                    let ops = FakeOps::new();
                    let mut wins = 0;
                    start.wait();
                    for _ in 0..ITERATIONS {
                        let Ok(session) = acquire(lease, &ops, options(false, false)) else {
                            thread::yield_now();
                            continue;
                        };
                        let now = holders.fetch_add(1, Ordering::SeqCst) + 1;
                        max_holders.fetch_max(now, Ordering::SeqCst);
                        holders.fetch_sub(1, Ordering::SeqCst);
                        drop(session);
                        ops.clear_trace();
                        wins += 1;
                    }
                    wins
                })
            })
            .collect();
        let wins: usize = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .sum();

        assert!(wins > 0);
        assert_eq!(
            max_holders.load(Ordering::SeqCst),
            1,
            "two sessions held the lease at the same time"
        );
    }

    #[test]
    fn preexisting_raw_mode_is_rejected_without_writes() {
        let lease = lease();
        let ops = FakeOps::new();
        ops.set_raw_mode(true);

        let error = acquire(lease, &ops, options(true, true)).unwrap_err();
        assert!(matches!(error, SessionError::RawModeOwnedElsewhere));
        assert_eq!(ops.trace(), [Op::QueryRawMode]);
        assert!(ops.raw_mode(), "the caller's raw mode was disturbed");

        ops.set_raw_mode(false);
        assert_lease_released(lease, &ops);
    }

    #[test]
    fn raw_mode_query_failure_is_reported_without_writes() {
        let lease = lease();
        let ops = FakeOps::new();
        ops.fail_at(Op::QueryRawMode);

        let error = acquire(lease, &ops, options(true, true)).unwrap_err();
        assert_eq!(setup_step(&error), SetupStep::QueryRawMode);
        assert_eq!(ops.trace(), [Op::QueryRawMode]);

        ops.lock().faults.clear();
        assert_lease_released(lease, &ops);
    }

    #[test]
    fn setup_failures_roll_back_exactly_the_recorded_capabilities() {
        struct Case {
            fail: Op,
            step: SetupStep,
            expected: Vec<Op>,
        }
        let mut cases = vec![
            Case {
                fail: Op::EnableRawMode,
                step: SetupStep::EnableRawMode,
                expected: vec![Op::QueryRawMode, Op::EnableRawMode],
            },
            Case {
                fail: Op::EnterAlternateScreen,
                step: SetupStep::EnterAlternateScreen,
                expected: vec![
                    Op::QueryRawMode,
                    Op::EnableRawMode,
                    Op::EnterAlternateScreen,
                    Op::LeaveAlternateScreen,
                    Op::ShowCursor,
                    Op::DisableRawMode,
                ],
            },
            Case {
                fail: Op::CreateTerminal,
                step: SetupStep::CreateTerminal,
                expected: vec![
                    Op::QueryRawMode,
                    Op::EnableRawMode,
                    Op::EnterAlternateScreen,
                    Op::CreateTerminal,
                    Op::LeaveAlternateScreen,
                    Op::ShowCursor,
                    Op::DisableRawMode,
                ],
            },
        ];
        if mouse_enabled() {
            cases.push(Case {
                fail: Op::EnableMouseCapture,
                step: SetupStep::EnableMouseCapture,
                expected: vec![
                    Op::QueryRawMode,
                    Op::EnableRawMode,
                    Op::EnterAlternateScreen,
                    Op::CreateTerminal,
                    Op::EnableMouseCapture,
                    Op::DisableMouseCapture,
                    Op::LeaveAlternateScreen,
                    Op::ShowCursor,
                    Op::DisableRawMode,
                ],
            });
        }
        let mut push_kitty = vec![
            Op::QueryRawMode,
            Op::EnableRawMode,
            Op::EnterAlternateScreen,
            Op::CreateTerminal,
        ];
        if mouse_enabled() {
            push_kitty.push(Op::EnableMouseCapture);
        }
        push_kitty.extend([
            Op::QueryKittyKeyboard,
            Op::PushKittyKeyboard,
            Op::PopKittyKeyboard,
        ]);
        if mouse_enabled() {
            push_kitty.push(Op::DisableMouseCapture);
        }
        push_kitty.extend([Op::LeaveAlternateScreen, Op::ShowCursor, Op::DisableRawMode]);
        cases.push(Case {
            fail: Op::PushKittyKeyboard,
            step: SetupStep::PushKittyKeyboard,
            expected: push_kitty,
        });

        for case in cases {
            let lease = lease();
            let ops = FakeOps::with_kitty_support();
            ops.fail_at(case.fail);

            let error = acquire(lease, &ops, options(true, true)).unwrap_err();
            assert_eq!(setup_step(&error), case.step, "{:?}", case.fail);
            assert_eq!(ops.trace(), case.expected, "{:?}", case.fail);
            assert!(!ops.raw_mode(), "{:?} left raw mode enabled", case.fail);
            assert!(
                matches!(error, SessionError::Setup { rollback: None, .. }),
                "{:?} reported a rollback failure: {error}",
                case.fail
            );

            ops.lock().faults.clear();
            assert_lease_released(lease, &ops);
        }
    }

    #[test]
    fn rollback_attempts_every_step_and_reports_its_failures() {
        let lease = lease();
        let ops = FakeOps::with_kitty_support();
        ops.fail_at(Op::PushKittyKeyboard);
        ops.fail_at(Op::PopKittyKeyboard);
        ops.fail_at(Op::LeaveAlternateScreen);

        let error = acquire(lease, &ops, options(true, true)).unwrap_err();
        let SessionError::Setup {
            step,
            rollback: Some(rollback),
            ..
        } = &error
        else {
            panic!("expected a setup error with rollback failures, got {error:?}");
        };
        assert_eq!(*step, SetupStep::PushKittyKeyboard);
        let failed: Vec<_> = rollback.failures().map(|(step, _)| step).collect();
        assert_eq!(
            failed,
            [
                RestoreStep::PopKittyKeyboard,
                RestoreStep::LeaveAlternateScreen
            ]
        );
        assert_eq!(
            rollback.failures().next().map(|(step, _)| step),
            Some(RestoreStep::PopKittyKeyboard)
        );
        assert!(!rollback.was_interrupted());
        let trace = ops.trace();
        assert!(trace.ends_with(&[Op::LeaveAlternateScreen, Op::ShowCursor, Op::DisableRawMode]));
        assert!(error.to_string().contains("rollback also failed"));

        ops.lock().faults.clear();
        assert_lease_released(lease, &ops);
    }

    #[test]
    fn kitty_query_failure_and_unsupported_terminals_do_not_fail_the_session() {
        for (supported, fail_query) in [(false, false), (true, true), (false, true)] {
            let lease = lease();
            let ops = FakeOps::new();
            ops.lock().kitty_supported = supported;
            if fail_query {
                ops.fail_at(Op::QueryKittyKeyboard);
            }

            let session = acquire(lease, &ops, options(true, false)).unwrap();
            assert!(!session.acquired().kitty_keyboard);
            assert!(!ops.trace().contains(&Op::PushKittyKeyboard));
            assert!(ops.trace().contains(&Op::QueryKittyKeyboard));
            drop(session);
            assert!(!ops.trace().contains(&Op::PopKittyKeyboard));
        }
    }

    #[test]
    fn disabled_options_are_not_queried_or_touched() {
        let ops = FakeOps::with_kitty_support();
        let session = acquire(
            lease(),
            &ops,
            SessionOptions {
                #[cfg(feature = "mouse")]
                mouse_capture: false,
                kitty_keyboard: false,
                panic_hook: false,
            },
        )
        .unwrap();
        assert_eq!(
            ops.trace(),
            [
                Op::QueryRawMode,
                Op::EnableRawMode,
                Op::EnterAlternateScreen,
                Op::CreateTerminal
            ]
        );
        assert_eq!(
            session.acquired(),
            Acquired {
                raw_mode: true,
                alternate_screen: true,
                mouse_capture: false,
                kitty_keyboard: false,
            }
        );
        ops.clear_trace();
        drop(session);
        assert_eq!(
            ops.trace(),
            [Op::LeaveAlternateScreen, Op::ShowCursor, Op::DisableRawMode]
        );
    }

    #[test]
    fn panics_during_acquisition_run_the_same_partial_rollback() {
        let mut boundaries = vec![
            (Op::EnableRawMode, vec![Op::QueryRawMode, Op::EnableRawMode]),
            (
                Op::EnterAlternateScreen,
                vec![
                    Op::QueryRawMode,
                    Op::EnableRawMode,
                    Op::EnterAlternateScreen,
                    Op::LeaveAlternateScreen,
                    Op::ShowCursor,
                    Op::DisableRawMode,
                ],
            ),
        ];
        // The hook boundary: `take_hook` panics on a panicking thread. Everything acquired so far
        // must be rolled back.
        let mut hook_boundary = full_acquire_trace();
        hook_boundary.truncate(hook_boundary.len() - 1); // panics inside TakeHook
        hook_boundary.extend(full_restore_trace());
        boundaries.push((Op::TakeHook, hook_boundary));

        for (op, expected) in boundaries {
            let lease = lease();
            let ops = FakeOps::with_kitty_support();
            ops.panic_at(op);

            let result = catch_unwind(AssertUnwindSafe(|| {
                acquire(lease, &ops, options(true, true))
            }));
            assert!(result.is_err(), "{op:?} did not panic");
            assert_eq!(ops.trace(), expected, "{op:?}");
            assert!(!ops.raw_mode(), "{op:?} left raw mode enabled");

            ops.lock().faults.clear();
            assert_lease_released(lease, &ops);
        }
    }

    #[test]
    fn restoration_attempts_every_step_after_an_error_and_returns_all_failures() {
        let ops = FakeOps::with_kitty_support();
        let session = acquire(lease(), &ops, options(true, true)).unwrap();
        ops.fail_at(Op::PopKittyKeyboard);
        ops.fail_at(Op::LeaveAlternateScreen);
        ops.clear_trace();

        let error = session.close().unwrap_err();
        assert!(!error.was_interrupted());
        let failed: Vec<_> = error.failures().map(|(step, _)| step).collect();
        assert_eq!(
            failed,
            [
                RestoreStep::PopKittyKeyboard,
                RestoreStep::LeaveAlternateScreen
            ]
        );
        let mut expected = full_restore_trace();
        expected.extend([Op::TakeHook, Op::SetHook]);
        assert_eq!(ops.trace(), expected);
        assert!(error.to_string().contains("leave the alternate screen"));
    }

    #[test]
    fn emergency_restore_and_owner_drop_converge_on_one_restoration() {
        let ops = FakeOps::with_kitty_support();
        let session = acquire(lease(), &ops, options(true, true)).unwrap();
        let shared = Arc::clone(&session.shared);
        ops.clear_trace();

        assert_eq!(
            shared.emergency_restore().unwrap(),
            RestoreOutcome::Restored
        );
        assert_eq!(ops.trace(), full_restore_trace());
        assert!(!session.is_active(), "the session must be visibly inactive");
        assert_eq!(
            session.acquired(),
            full_acquired(),
            "what was acquired is history and does not change"
        );

        assert_eq!(
            shared.emergency_restore().unwrap(),
            RestoreOutcome::AlreadyRestored
        );
        assert_eq!(shared.restore().unwrap(), RestoreOutcome::AlreadyRestored);

        ops.clear_trace();
        drop(session);
        assert_eq!(
            ops.trace(),
            [Op::TakeHook, Op::SetHook],
            "drop after an emergency restore must only reinstate the hook"
        );
    }

    #[test]
    fn emergency_restore_after_owner_drop_writes_nothing() {
        let ops = FakeOps::with_kitty_support();
        let session = acquire(lease(), &ops, options(true, true)).unwrap();
        let shared = Arc::clone(&session.shared);
        drop(session);
        ops.clear_trace();

        assert_eq!(
            shared.emergency_restore().unwrap(),
            RestoreOutcome::AlreadyRestored
        );
        assert!(ops.trace().is_empty());
    }

    #[test]
    fn racing_restorers_have_one_winner_and_the_lease_is_released_afterwards() {
        let lease = lease();
        let ops = FakeOps::with_kitty_support();
        let session = acquire(lease, &ops, options(true, true)).unwrap();
        let shared = Arc::clone(&session.shared);
        let gate = ops.gate(Op::LeaveAlternateScreen);
        ops.clear_trace();

        let closer = thread::spawn(move || session.close());
        gate.entered.wait();

        // The owner is mid-restoration and holds the state lock. The transition happened before
        // the I/O, so the session is already inactive.
        assert!(
            !shared.is_active(),
            "state must transition before cleanup I/O"
        );
        assert_eq!(shared.emergency_restore().unwrap(), RestoreOutcome::Skipped);
        assert!(matches!(
            acquire(lease, &ops, options(false, false)),
            Err(SessionError::AlreadyActive)
        ));
        let waiter = {
            let shared = Arc::clone(&shared);
            thread::spawn(move || shared.restore())
        };
        assert!(
            !waiter.is_finished(),
            "a normal restorer must wait for the winner"
        );

        gate.release.wait();
        closer.join().unwrap().unwrap();
        assert_eq!(
            waiter.join().unwrap().unwrap(),
            RestoreOutcome::AlreadyRestored
        );
        assert_eq!(
            shared.emergency_restore().unwrap(),
            RestoreOutcome::AlreadyRestored
        );
        assert_eq!(
            ops.trace()
                .iter()
                .filter(|op| **op == Op::LeaveAlternateScreen)
                .count(),
            1
        );
        assert_lease_released(lease, &ops);
    }

    #[test]
    fn a_panic_inside_restoration_never_reopens_the_state() {
        let lease = lease();
        let ops = FakeOps::with_kitty_support();
        let session = acquire(lease, &ops, options(true, true)).unwrap();
        let shared = Arc::clone(&session.shared);
        ops.panic_at(Op::LeaveAlternateScreen);
        ops.clear_trace();

        let result = catch_unwind(AssertUnwindSafe(|| drop(session)));
        assert!(result.is_err());

        let interrupted = shared.restore().unwrap_err();
        assert!(interrupted.was_interrupted());
        assert_eq!(interrupted.failures().count(), 0);
        assert!(interrupted.to_string().contains("panicked part-way"));
        assert_eq!(shared.emergency_restore().unwrap(), RestoreOutcome::Skipped);
        assert!(!shared.is_active());
        let trace = ops.trace();
        assert_eq!(
            trace
                .iter()
                .filter(|op| **op == Op::LeaveAlternateScreen)
                .count(),
            1
        );
        assert!(
            !trace.contains(&Op::DisableRawMode),
            "no retry after a panic"
        );
        assert!(
            ops.raw_mode(),
            "the skipped steps stay undone; the next acquire in this process is rejected by policy"
        );

        ops.lock().faults.clear();
        ops.set_raw_mode(false);
        assert_lease_released(lease, &ops);
    }

    #[test]
    fn hook_wraps_the_current_hook_and_teardown_reinstates_that_exact_hook() {
        let ops = FakeOps::new();
        let marker = Arc::new(());
        let user_hook = ops.install_user_hook(Box::new(move |_| {
            let _ = &marker;
        }));

        let session = acquire(lease(), &ops, options(false, false)).unwrap();
        let wrapper = ops.current_hook_identity().unwrap();
        assert_ne!(
            wrapper, user_hook,
            "the session did not install its wrapper"
        );
        assert_eq!(
            hook_identity(&session.hook.as_ref().unwrap().previous),
            user_hook
        );

        session.close().unwrap();
        assert_eq!(ops.current_hook_identity(), Some(user_hook));
    }

    #[test]
    fn a_hook_installed_after_the_session_is_left_in_place() {
        let ops = FakeOps::new();
        let session = acquire(lease(), &ops, options(false, false)).unwrap();
        let marker = Arc::new(());
        let foreign = ops.install_user_hook(Box::new(move |_| {
            let _ = &marker;
        }));
        ops.clear_trace();

        session.close().unwrap();
        assert_eq!(ops.current_hook_identity(), Some(foreign));
        assert!(ops.trace().ends_with(&[Op::TakeHook, Op::SetHook]));
    }

    #[test]
    fn sequential_sessions_capture_the_current_hook_and_do_not_accumulate_wrappers() {
        let lease = lease();
        let ops = FakeOps::new();
        let marker = Arc::new(());
        let original = ops.install_user_hook(Box::new(move |_| {
            let _ = &marker;
        }));

        for _ in 0..32 {
            let session = acquire(lease, &ops, options(false, false)).unwrap();
            assert_eq!(
                hook_identity(&session.hook.as_ref().unwrap().previous),
                original
            );
            session.close().unwrap();
            assert_eq!(ops.current_hook_identity(), Some(original));
        }

        let marker = Arc::new(());
        let replaced = ops.install_user_hook(Box::new(move |_| {
            let _ = &marker;
        }));
        let session = acquire(lease, &ops, options(false, false)).unwrap();
        assert_eq!(
            hook_identity(&session.hook.as_ref().unwrap().previous),
            replaced,
            "a later session must wrap the hook current at its start"
        );
        drop(session);
        assert_eq!(ops.current_hook_identity(), Some(replaced));
    }

    #[test]
    fn hook_uninstall_is_skipped_while_the_thread_is_panicking() {
        struct DropDuringUnwind(Option<Session<FakeOps>>);
        impl Drop for DropDuringUnwind {
            fn drop(&mut self) {
                self.0.take();
            }
        }

        let ops = FakeOps::with_kitty_support();
        let session = acquire(lease(), &ops, options(true, true)).unwrap();
        let wrapper = ops.current_hook_identity().unwrap();
        ops.clear_trace();

        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = DropDuringUnwind(Some(session));
            panic!("unwind through the session");
        }));
        assert!(result.is_err());

        assert_eq!(ops.trace(), full_restore_trace(), "restore must still run");
        assert_eq!(
            ops.current_hook_identity(),
            Some(wrapper),
            "set_hook is forbidden on a panicking thread, so the wrapper must remain"
        );
    }

    #[test]
    fn a_session_without_a_panic_hook_never_touches_the_hook() {
        let ops = FakeOps::new();
        let session = acquire(
            lease(),
            &ops,
            SessionOptions {
                panic_hook: false,
                ..options(false, false)
            },
        )
        .unwrap();
        assert!(session.hook.is_none());
        drop(session);
        assert!(!ops.trace().contains(&Op::TakeHook));
        assert!(!ops.trace().contains(&Op::SetHook));
    }

    #[test]
    fn a_user_hook_that_wraps_ours_after_the_session_started_is_preserved() {
        let ops = FakeOps::new();
        let session = acquire(lease(), &ops, options(false, false)).unwrap();
        let wrapper = ops.current_hook_identity().unwrap();

        // User code does `let previous = take_hook(); set_hook(|info| { ...; previous(info) })`.
        let ours = ops.take_hook();
        let user_wrapper = ops.install_user_hook(Box::new(move |info| ours(info)));
        assert_ne!(user_wrapper, wrapper);
        ops.clear_trace();

        session.close().unwrap();
        assert_eq!(
            ops.current_hook_identity(),
            Some(user_wrapper),
            "teardown must not drop a hook that wraps ours"
        );
        assert!(ops.trace().ends_with(&[Op::TakeHook, Op::SetHook]));
    }

    #[test]
    fn acquiring_on_a_panicking_thread_fails_cleanly_when_a_hook_is_requested() {
        struct AcquireDuringUnwind {
            lease: &'static Lease,
            ops: FakeOps,
            result: Arc<Mutex<Option<Result<(), SessionError>>>>,
        }
        impl Drop for AcquireDuringUnwind {
            fn drop(&mut self) {
                let result = acquire(self.lease, &self.ops, options(true, true)).map(drop);
                *self.result.lock().unwrap() = Some(result);
            }
        }

        let lease = lease();
        let ops = FakeOps::with_kitty_support();
        let result = Arc::new(Mutex::new(None));
        let unwind = catch_unwind(AssertUnwindSafe(|| {
            let _guard = AcquireDuringUnwind {
                lease,
                ops: ops.clone(),
                result: Arc::clone(&result),
            };
            panic!("unwind through an acquire");
        }));
        assert!(unwind.is_err());

        let result = result
            .lock()
            .unwrap()
            .take()
            .expect("acquire ran during unwinding");
        assert!(
            matches!(result, Err(SessionError::ThreadPanicking)),
            "{result:?}"
        );
        assert!(ops.trace().is_empty(), "nothing may be touched");
        assert_lease_released(lease, &ops);
    }

    #[test]
    fn errors_display_their_steps() {
        let error = SessionError::Setup {
            step: SetupStep::EnterAlternateScreen,
            source: io::Error::other("boom"),
            rollback: None,
        };
        assert_eq!(
            error.to_string(),
            "failed to enter the alternate screen: boom"
        );
        assert!(std::error::Error::source(&error).is_some());
        assert!(
            SessionError::AlreadyActive
                .to_string()
                .contains("already active")
        );
        assert!(
            SessionError::RawModeOwnedElsewhere
                .to_string()
                .contains("raw mode")
        );
    }

    #[test]
    fn the_session_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CrosstermSession>();
        assert_send_sync::<Session<FakeOps>>();
    }
}
