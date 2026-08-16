use std::{
    io::{self, Stdout, stdout},
    sync::atomic::{AtomicBool, Ordering},
};

use bevy::prelude::*;

use ratatui::Terminal;
use ratatui::crossterm::{
    ExecutableCommand, cursor,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        is_raw_mode_enabled,
    },
};

use ratatui::backend::CrosstermBackend;

use crate::{RatatuiPlugins, context::TerminalContext};

use super::{event::EventPlugin, kitty::KittyPlugin};

#[cfg(feature = "mouse")]
use super::mouse::MousePlugin;
#[cfg(feature = "keyboard")]
use super::translation::TranslationPlugin;

/// Ratatui context that will draw to the terminal buffer using crossterm.
#[derive(Deref, DerefMut, Debug)]
pub struct CrosstermContext {
    #[deref]
    terminal: Terminal<CrosstermBackend<Stdout>>,
    cleanup: Option<TerminalCleanup>,
}

#[derive(Clone, Copy, Default, Resource)]
pub(crate) struct CrosstermSettings {
    pub(crate) enable_kitty_protocol: bool,
    #[cfg(feature = "mouse")]
    pub(crate) enable_mouse_capture: bool,
}

trait InitializationOperations {
    type Terminal;

    fn is_raw_mode_enabled(&mut self) -> io::Result<bool>;
    fn enable_raw_mode(&mut self) -> io::Result<()>;
    fn disable_raw_mode(&mut self) -> io::Result<()>;
    fn enter_alternate_screen(&mut self) -> io::Result<()>;
    fn leave_alternate_screen(&mut self) -> io::Result<()>;
    fn create_terminal(&mut self) -> io::Result<Self::Terminal>;
}

struct SystemInitializationOperations;

impl InitializationOperations for SystemInitializationOperations {
    type Terminal = Terminal<CrosstermBackend<Stdout>>;

    fn is_raw_mode_enabled(&mut self) -> io::Result<bool> {
        is_raw_mode_enabled()
    }

    fn enable_raw_mode(&mut self) -> io::Result<()> {
        enable_raw_mode()
    }

    fn disable_raw_mode(&mut self) -> io::Result<()> {
        disable_raw_mode()
    }

    fn enter_alternate_screen(&mut self) -> io::Result<()> {
        stdout().execute(EnterAlternateScreen).map(|_| ())
    }

    fn leave_alternate_screen(&mut self) -> io::Result<()> {
        stdout().execute(LeaveAlternateScreen).map(|_| ())
    }

    fn create_terminal(&mut self) -> io::Result<Self::Terminal> {
        Terminal::new(CrosstermBackend::new(stdout()))
    }
}

struct InitializationGuard<O: InitializationOperations> {
    operations: O,
    raw_mode: bool,
    alternate_screen: bool,
}

impl<O: InitializationOperations> InitializationGuard<O> {
    fn new(operations: O) -> Self {
        Self {
            operations,
            raw_mode: false,
            alternate_screen: false,
        }
    }

    fn disarm(&mut self) {
        self.raw_mode = false;
        self.alternate_screen = false;
    }
}

impl<O: InitializationOperations> Drop for InitializationGuard<O> {
    fn drop(&mut self) {
        if self.alternate_screen {
            let _ = self.operations.leave_alternate_screen();
        }
        if self.raw_mode {
            let _ = self.operations.disable_raw_mode();
        }
    }
}

fn initialize_with<O: InitializationOperations>(operations: O) -> io::Result<O::Terminal> {
    let mut rollback = InitializationGuard::new(operations);

    if rollback.operations.is_raw_mode_enabled()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the terminal is already in raw mode",
        ));
    }

    rollback.operations.enable_raw_mode()?;
    rollback.raw_mode = true;

    // `execute` may write the escape sequence and then fail while flushing, so conservatively
    // attempt the inverse operation even when entering the alternate screen returns an error.
    rollback.alternate_screen = true;
    rollback.operations.enter_alternate_screen()?;

    let terminal = rollback.operations.create_terminal()?;
    rollback.disarm();
    Ok(terminal)
}

impl CrosstermContext {
    pub(crate) fn restore_terminal() -> io::Result<()> {
        let mut stdout = stdout();
        let raw_mode = disable_raw_mode();
        let alternate_screen = stdout.execute(LeaveAlternateScreen).map(|_| ());
        let cursor = stdout.execute(cursor::Show).map(|_| ());

        raw_mode.and(alternate_screen).and(cursor)
    }

    #[cfg(not(feature = "windowed"))]
    pub(crate) fn take_cleanup(&mut self) -> TerminalCleanup {
        self.cleanup
            .take()
            .expect("terminal cleanup ownership is available")
    }
}

impl Drop for CrosstermContext {
    fn drop(&mut self) {
        // Dropping the token restores a directly owned terminal; a plugin-owned context has
        // already moved the token into its `TerminalSession`.
        drop(self.cleanup.take());
    }
}

/// The unique right to restore an initialized terminal.
///
/// The app session shares this token with its panic hook, so the atomic transition is what makes
/// cleanup exactly once across those two process-wide paths. Moving the token out of a direct
/// context transfers that right to the session without a second owner.
#[derive(Debug)]
pub(crate) struct TerminalCleanup {
    active: AtomicBool,
}

impl TerminalCleanup {
    pub(crate) fn new() -> Self {
        Self {
            active: AtomicBool::new(true),
        }
    }

    pub(crate) fn restore_with(&self, restore: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        if !self.active.swap(false, Ordering::AcqRel) {
            return Ok(());
        }

        restore()
    }

    fn restore(&self) -> io::Result<()> {
        self.restore_with(CrosstermContext::restore_terminal)
    }
}

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

impl TerminalContext<CrosstermBackend<Stdout>> for CrosstermContext {
    fn init() -> Result<Self> {
        let terminal = initialize_with(SystemInitializationOperations)?;
        Ok(Self {
            terminal,
            cleanup: Some(TerminalCleanup::new()),
        })
    }

    fn configure_plugin_group(
        group: &RatatuiPlugins,
        mut builder: bevy::app::PluginGroupBuilder,
    ) -> bevy::app::PluginGroupBuilder {
        builder = builder.add(EventPlugin::default()).add(KittyPlugin);

        #[cfg(feature = "mouse")]
        let builder = builder.add(MousePlugin);
        #[cfg(feature = "keyboard")]
        let builder = builder.add(TranslationPlugin);

        let mut builder = builder;
        if !group.enable_kitty_protocol {
            builder = builder.disable::<KittyPlugin>();
        }

        #[cfg(feature = "mouse")]
        if !group.enable_mouse_capture {
            builder = builder.disable::<MousePlugin>();
        }

        #[cfg(feature = "keyboard")]
        if !group.enable_input_forwarding {
            builder = builder.disable::<TranslationPlugin>();
        }

        builder
    }
}

#[cfg(test)]
mod tests {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{
            Arc, Barrier, Mutex,
            atomic::{AtomicUsize, Ordering as AtomicOrdering},
        },
        thread,
    };

    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum InitializationAction {
        QueryRawMode,
        EnableRawMode,
        EnterAlternateScreen,
        CreateTerminal,
        LeaveAlternateScreen,
        DisableRawMode,
    }

    #[derive(Clone)]
    struct FakeInitializationOperations {
        state: Arc<Mutex<FakeInitializationState>>,
    }

    struct FakeInitializationState {
        actions: Vec<InitializationAction>,
        already_raw: bool,
        fail_at: Vec<InitializationAction>,
        panic_at: Option<InitializationAction>,
    }

    impl FakeInitializationOperations {
        fn new(
            already_raw: bool,
            fail_at: Option<InitializationAction>,
            panic_at: Option<InitializationAction>,
        ) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeInitializationState {
                    actions: Vec::new(),
                    already_raw,
                    fail_at: fail_at.into_iter().collect(),
                    panic_at,
                })),
            }
        }

        fn perform(&self, action: InitializationAction) -> io::Result<()> {
            let (should_panic, should_fail) = {
                let mut state = self.state.lock().unwrap();
                state.actions.push(action);
                (
                    state.panic_at == Some(action),
                    state.fail_at.contains(&action),
                )
            };
            assert!(!should_panic, "injected {action:?} panic");
            if should_fail {
                return Err(io::Error::other(format!("injected {action:?} error")));
            }
            Ok(())
        }

        fn actions(&self) -> Vec<InitializationAction> {
            self.state.lock().unwrap().actions.clone()
        }
    }

    impl InitializationOperations for FakeInitializationOperations {
        type Terminal = ();

        fn is_raw_mode_enabled(&mut self) -> io::Result<bool> {
            self.perform(InitializationAction::QueryRawMode)?;
            Ok(self.state.lock().unwrap().already_raw)
        }

        fn enable_raw_mode(&mut self) -> io::Result<()> {
            self.perform(InitializationAction::EnableRawMode)
        }

        fn disable_raw_mode(&mut self) -> io::Result<()> {
            self.perform(InitializationAction::DisableRawMode)
        }

        fn enter_alternate_screen(&mut self) -> io::Result<()> {
            self.perform(InitializationAction::EnterAlternateScreen)
        }

        fn leave_alternate_screen(&mut self) -> io::Result<()> {
            self.perform(InitializationAction::LeaveAlternateScreen)
        }

        fn create_terminal(&mut self) -> io::Result<Self::Terminal> {
            self.perform(InitializationAction::CreateTerminal)
        }
    }

    fn assert_initialization_error(
        fail_at: InitializationAction,
        expected: &[InitializationAction],
    ) {
        let operations = FakeInitializationOperations::new(false, Some(fail_at), None);
        assert!(initialize_with(operations.clone()).is_err());
        assert_eq!(operations.actions(), expected);
    }

    #[test]
    fn initialization_success_acquires_every_capability_without_rollback() {
        let operations = FakeInitializationOperations::new(false, None, None);
        initialize_with(operations.clone()).unwrap();
        assert_eq!(
            operations.actions(),
            vec![
                InitializationAction::QueryRawMode,
                InitializationAction::EnableRawMode,
                InitializationAction::EnterAlternateScreen,
                InitializationAction::CreateTerminal,
            ]
        );
    }

    #[test]
    fn initialization_rejects_preexisting_raw_mode_without_modifying_it() {
        let operations = FakeInitializationOperations::new(true, None, None);
        let error = initialize_with(operations.clone()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            operations.actions(),
            vec![InitializationAction::QueryRawMode]
        );
    }

    #[test]
    fn initialization_errors_roll_back_only_possibly_acquired_capabilities() {
        use InitializationAction::*;

        assert_initialization_error(QueryRawMode, &[QueryRawMode]);
        assert_initialization_error(EnableRawMode, &[QueryRawMode, EnableRawMode]);
        assert_initialization_error(
            EnterAlternateScreen,
            &[
                QueryRawMode,
                EnableRawMode,
                EnterAlternateScreen,
                LeaveAlternateScreen,
                DisableRawMode,
            ],
        );
        assert_initialization_error(
            CreateTerminal,
            &[
                QueryRawMode,
                EnableRawMode,
                EnterAlternateScreen,
                CreateTerminal,
                LeaveAlternateScreen,
                DisableRawMode,
            ],
        );
    }

    #[test]
    fn initialization_panics_run_the_same_partial_rollback() {
        use InitializationAction::*;

        let cases = [
            (QueryRawMode, vec![QueryRawMode]),
            (EnableRawMode, vec![QueryRawMode, EnableRawMode]),
            (
                EnterAlternateScreen,
                vec![
                    QueryRawMode,
                    EnableRawMode,
                    EnterAlternateScreen,
                    LeaveAlternateScreen,
                    DisableRawMode,
                ],
            ),
            (
                CreateTerminal,
                vec![
                    QueryRawMode,
                    EnableRawMode,
                    EnterAlternateScreen,
                    CreateTerminal,
                    LeaveAlternateScreen,
                    DisableRawMode,
                ],
            ),
        ];

        for (panic_at, expected) in cases {
            let operations = FakeInitializationOperations::new(false, None, Some(panic_at));
            let result = catch_unwind(AssertUnwindSafe(|| {
                let _ = initialize_with(operations.clone());
            }));
            assert!(result.is_err(), "{panic_at:?} did not panic");
            assert_eq!(operations.actions(), expected, "panic at {panic_at:?}");
        }
    }

    #[test]
    fn initialization_rollback_attempts_every_release_after_errors() {
        use InitializationAction::*;

        let operations = FakeInitializationOperations::new(false, Some(CreateTerminal), None);
        {
            let mut state = operations.state.lock().unwrap();
            state.fail_at = vec![CreateTerminal, LeaveAlternateScreen, DisableRawMode];
        }
        assert!(initialize_with(operations.clone()).is_err());
        assert_eq!(
            operations.actions(),
            vec![
                QueryRawMode,
                EnableRawMode,
                EnterAlternateScreen,
                CreateTerminal,
                LeaveAlternateScreen,
                DisableRawMode,
            ]
        );
    }

    #[test]
    fn terminal_cleanup_token_allows_one_restore() {
        let cleanup = TerminalCleanup::new();
        let calls = AtomicUsize::new(0);

        cleanup
            .restore_with(|| {
                calls.fetch_add(1, AtomicOrdering::Relaxed);
                Ok(())
            })
            .unwrap();
        cleanup
            .restore_with(|| {
                calls.fetch_add(1, AtomicOrdering::Relaxed);
                Ok(())
            })
            .unwrap();

        assert_eq!(calls.load(AtomicOrdering::Relaxed), 1);
    }

    #[test]
    fn terminal_cleanup_error_or_panic_still_consumes_ownership() {
        let cleanup = TerminalCleanup::new();
        assert!(
            cleanup
                .restore_with(|| Err(io::Error::other("cleanup failed")))
                .is_err()
        );
        let mut retried = false;
        cleanup
            .restore_with(|| {
                retried = true;
                Ok(())
            })
            .unwrap();
        assert!(!retried);

        let cleanup = TerminalCleanup::new();
        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _ = cleanup.restore_with(|| panic!("cleanup panic"));
        }));
        assert!(panic.is_err());
        let mut retried = false;
        cleanup
            .restore_with(|| {
                retried = true;
                Ok(())
            })
            .unwrap();
        assert!(!retried);
    }

    #[test]
    fn terminal_cleanup_bounded_owner_sequences_have_one_winner() {
        for encoded in 0..3_usize.pow(6) {
            let cleanup = TerminalCleanup::new();
            let winners = AtomicUsize::new(0);
            let winning_owner = AtomicUsize::new(usize::MAX);
            let mut sequence = encoded;
            let expected_owner = encoded % 3;

            for _ in 0..6 {
                let owner = sequence % 3; // direct context, app session, or panic hook
                sequence /= 3;
                cleanup
                    .restore_with(|| {
                        winners.fetch_add(1, AtomicOrdering::Relaxed);
                        winning_owner.store(owner, AtomicOrdering::Relaxed);
                        Ok(())
                    })
                    .unwrap();
            }

            assert_eq!(winners.load(AtomicOrdering::Relaxed), 1, "trace {encoded}");
            assert_eq!(
                winning_owner.load(AtomicOrdering::Relaxed),
                expected_owner,
                "trace {encoded}"
            );
        }
    }

    #[test]
    fn terminal_cleanup_concurrent_callers_have_one_winner() {
        const CALLERS: usize = 32;
        let cleanup = Arc::new(TerminalCleanup::new());
        let winners = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(Barrier::new(CALLERS));

        let threads: Vec<_> = (0..CALLERS)
            .map(|_| {
                let cleanup = Arc::clone(&cleanup);
                let winners = Arc::clone(&winners);
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    cleanup
                        .restore_with(|| {
                            winners.fetch_add(1, AtomicOrdering::Relaxed);
                            Ok(())
                        })
                        .unwrap();
                })
            })
            .collect();

        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(winners.load(AtomicOrdering::Relaxed), 1);
    }
}
