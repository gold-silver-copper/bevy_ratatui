use std::{
    io::{self, Write},
    sync::Arc,
};

use bevy::prelude::*;

use crate::RatatuiContext;

use super::{
    context::{CrosstermSettings, TerminalCleanup},
    error::PanicHookGuard,
    kitty::{KittyEnabled, disable_kitty_protocol, enable_kitty_protocol, supports_kitty_protocol},
};

#[cfg(feature = "mouse")]
use super::mouse::{MouseEnabled, disable_mouse_capture, enable_mouse_capture};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CleanupAction {
    Kitty,
    #[cfg(feature = "mouse")]
    Mouse,
    RawMode,
    AlternateScreen,
    Cursor,
}

trait LifecycleOperations: Send + Sync + 'static {
    #[cfg(feature = "mouse")]
    fn enable_mouse_capture(&self) -> io::Result<()>;
    #[cfg(feature = "mouse")]
    fn disable_mouse_capture(&self) -> io::Result<()>;
    fn supports_kitty_protocol(&self) -> io::Result<bool>;
    fn enable_kitty_protocol(&self) -> io::Result<()>;
    fn disable_kitty_protocol(&self) -> io::Result<()>;
    fn disable_raw_mode(&self) -> io::Result<()>;
    fn leave_alternate_screen(&self) -> io::Result<()>;
    fn show_cursor(&self) -> io::Result<()>;
}

#[derive(Default)]
struct SystemLifecycleOperations;

impl LifecycleOperations for SystemLifecycleOperations {
    #[cfg(feature = "mouse")]
    fn enable_mouse_capture(&self) -> io::Result<()> {
        enable_mouse_capture()
    }

    #[cfg(feature = "mouse")]
    fn disable_mouse_capture(&self) -> io::Result<()> {
        disable_mouse_capture()
    }

    fn supports_kitty_protocol(&self) -> io::Result<bool> {
        supports_kitty_protocol()
    }

    fn enable_kitty_protocol(&self) -> io::Result<()> {
        enable_kitty_protocol()
    }

    fn disable_kitty_protocol(&self) -> io::Result<()> {
        disable_kitty_protocol()
    }

    fn disable_raw_mode(&self) -> io::Result<()> {
        ratatui::crossterm::terminal::disable_raw_mode()
    }

    fn leave_alternate_screen(&self) -> io::Result<()> {
        use ratatui::crossterm::{ExecutableCommand, terminal::LeaveAlternateScreen};

        io::stdout().execute(LeaveAlternateScreen).map(|_| ())
    }

    fn show_cursor(&self) -> io::Result<()> {
        use ratatui::crossterm::{ExecutableCommand, cursor};

        io::stdout().execute(cursor::Show).map(|_| ())
    }
}

struct CleanupPlan<O: LifecycleOperations = SystemLifecycleOperations> {
    terminal: TerminalCleanup,
    operations: O,
    kitty: bool,
    #[cfg(feature = "mouse")]
    mouse: bool,
}

impl<O: LifecycleOperations> CleanupPlan<O> {
    fn new(terminal: TerminalCleanup, operations: O) -> Self {
        Self {
            terminal,
            operations,
            kitty: false,
            #[cfg(feature = "mouse")]
            mouse: false,
        }
    }

    fn run(&self, mut cleanup: impl FnMut(CleanupAction) -> io::Result<()>) -> io::Result<()> {
        self.terminal.restore_with(|| {
            let mut first_error = None;

            if self.kitty {
                record_error(&mut first_error, cleanup(CleanupAction::Kitty));
            }
            #[cfg(feature = "mouse")]
            if self.mouse {
                record_error(&mut first_error, cleanup(CleanupAction::Mouse));
            }
            record_error(&mut first_error, cleanup(CleanupAction::RawMode));
            record_error(&mut first_error, cleanup(CleanupAction::AlternateScreen));
            record_error(&mut first_error, cleanup(CleanupAction::Cursor));

            first_error.map_or(Ok(()), Err)
        })
    }

    fn restore(&self) -> io::Result<()> {
        self.run(|action| match action {
            CleanupAction::Kitty => self.operations.disable_kitty_protocol(),
            #[cfg(feature = "mouse")]
            CleanupAction::Mouse => self.operations.disable_mouse_capture(),
            CleanupAction::RawMode => self.operations.disable_raw_mode(),
            CleanupAction::AlternateScreen => self.operations.leave_alternate_screen(),
            CleanupAction::Cursor => self.operations.show_cursor(),
        })
    }
}

impl<O: LifecycleOperations> Drop for CleanupPlan<O> {
    fn drop(&mut self) {
        report_cleanup_error(self.restore());
    }
}

#[derive(Resource)]
struct TerminalSession {
    cleanup: Arc<CleanupPlan>,
    _hook: PanicHookGuard,
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        report_cleanup_error(self.cleanup.restore());
    }
}

pub(crate) fn setup(world: &mut World) -> Result {
    let settings = *world.resource::<CrosstermSettings>();
    let mut context = RatatuiContext::init()?;
    let mut cleanup = CleanupPlan::new(context.0.take_cleanup(), SystemLifecycleOperations);

    configure_modes(settings, &mut cleanup)?;

    let cleanup = Arc::new(cleanup);
    let panic_cleanup = Arc::clone(&cleanup);
    let hook = PanicHookGuard::install(move || {
        report_cleanup_error(panic_cleanup.restore());
    });
    let session = TerminalSession {
        cleanup,
        _hook: hook,
    };

    world.insert_resource(context);
    if session.cleanup.kitty {
        world.insert_resource(KittyEnabled);
    }
    #[cfg(feature = "mouse")]
    if session.cleanup.mouse {
        world.insert_resource(MouseEnabled);
    }
    world.insert_resource(session);

    Ok(())
}

fn configure_modes<O: LifecycleOperations>(
    settings: CrosstermSettings,
    cleanup: &mut CleanupPlan<O>,
) -> io::Result<()> {
    #[cfg(feature = "mouse")]
    if settings.enable_mouse_capture {
        // Crossterm commands can write successfully and then fail while flushing. Mark each mode
        // before enabling it so an ambiguous failure still attempts the inverse operation.
        cleanup.mouse = true;
        cleanup.operations.enable_mouse_capture()?;
    }

    if settings.enable_kitty_protocol
        && cleanup
            .operations
            .supports_kitty_protocol()
            .unwrap_or(false)
    {
        cleanup.kitty = true;
        cleanup.operations.enable_kitty_protocol()?;
    }

    Ok(())
}

fn record_error(first_error: &mut Option<io::Error>, result: io::Result<()>) {
    if let Err(err) = result
        && first_error.is_none()
    {
        *first_error = Some(err);
    }
}

fn report_cleanup_error(result: io::Result<()>) {
    if let Err(err) = result {
        let _ = writeln!(io::stderr(), "Failed to restore terminal: {err}");
    }
}

#[cfg(test)]
mod tests {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind},
        sync::{Arc, Mutex},
    };

    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum LifecycleEvent {
        #[cfg(feature = "mouse")]
        EnableMouse,
        #[cfg(feature = "mouse")]
        DisableMouse,
        QueryKitty,
        EnableKitty,
        DisableKitty,
        DisableRawMode,
        LeaveAlternateScreen,
        ShowCursor,
    }

    #[derive(Clone)]
    struct FakeLifecycleOperations {
        state: Arc<Mutex<FakeLifecycleState>>,
    }

    struct FakeLifecycleState {
        events: Vec<LifecycleEvent>,
        kitty_supported: bool,
        fail_at: Vec<LifecycleEvent>,
        panic_at: Option<LifecycleEvent>,
    }

    impl FakeLifecycleOperations {
        fn new(kitty_supported: bool) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeLifecycleState {
                    events: Vec::new(),
                    kitty_supported,
                    fail_at: Vec::new(),
                    panic_at: None,
                })),
            }
        }

        fn perform(&self, event: LifecycleEvent) -> io::Result<()> {
            let (should_fail, should_panic) = {
                let mut state = self.state.lock().unwrap();
                state.events.push(event);
                (
                    state.fail_at.contains(&event),
                    state.panic_at == Some(event),
                )
            };
            assert!(!should_panic, "injected {event:?} panic");
            if should_fail {
                return Err(io::Error::other(format!("injected {event:?} error")));
            }
            Ok(())
        }

        fn events(&self) -> Vec<LifecycleEvent> {
            self.state.lock().unwrap().events.clone()
        }

        fn fail_at(&self, events: &[LifecycleEvent]) {
            self.state.lock().unwrap().fail_at = events.to_vec();
        }

        fn panic_at(&self, event: LifecycleEvent) {
            self.state.lock().unwrap().panic_at = Some(event);
        }
    }

    impl LifecycleOperations for FakeLifecycleOperations {
        #[cfg(feature = "mouse")]
        fn enable_mouse_capture(&self) -> io::Result<()> {
            self.perform(LifecycleEvent::EnableMouse)
        }

        #[cfg(feature = "mouse")]
        fn disable_mouse_capture(&self) -> io::Result<()> {
            self.perform(LifecycleEvent::DisableMouse)
        }

        fn supports_kitty_protocol(&self) -> io::Result<bool> {
            self.perform(LifecycleEvent::QueryKitty)?;
            Ok(self.state.lock().unwrap().kitty_supported)
        }

        fn enable_kitty_protocol(&self) -> io::Result<()> {
            self.perform(LifecycleEvent::EnableKitty)
        }

        fn disable_kitty_protocol(&self) -> io::Result<()> {
            self.perform(LifecycleEvent::DisableKitty)
        }

        fn disable_raw_mode(&self) -> io::Result<()> {
            self.perform(LifecycleEvent::DisableRawMode)
        }

        fn leave_alternate_screen(&self) -> io::Result<()> {
            self.perform(LifecycleEvent::LeaveAlternateScreen)
        }

        fn show_cursor(&self) -> io::Result<()> {
            self.perform(LifecycleEvent::ShowCursor)
        }
    }

    fn settings(kitty: bool, mouse: bool) -> CrosstermSettings {
        #[cfg(not(feature = "mouse"))]
        let _ = mouse;
        CrosstermSettings {
            enable_kitty_protocol: kitty,
            #[cfg(feature = "mouse")]
            enable_mouse_capture: mouse,
        }
    }

    fn full_plan(operations: FakeLifecycleOperations) -> CleanupPlan<FakeLifecycleOperations> {
        CleanupPlan {
            terminal: TerminalCleanup::new(),
            operations,
            kitty: true,
            #[cfg(feature = "mouse")]
            mouse: true,
        }
    }

    fn expected_cleanup_events() -> Vec<LifecycleEvent> {
        vec![
            LifecycleEvent::DisableKitty,
            #[cfg(feature = "mouse")]
            LifecycleEvent::DisableMouse,
            LifecycleEvent::DisableRawMode,
            LifecycleEvent::LeaveAlternateScreen,
            LifecycleEvent::ShowCursor,
        ]
    }

    fn terminal_cleanup_events() -> Vec<LifecycleEvent> {
        vec![
            LifecycleEvent::DisableRawMode,
            LifecycleEvent::LeaveAlternateScreen,
            LifecycleEvent::ShowCursor,
        ]
    }

    #[test]
    fn cleanup_runs_once_in_order() {
        let operations = FakeLifecycleOperations::new(true);
        let plan = full_plan(operations.clone());

        plan.restore().unwrap();
        plan.restore().unwrap();

        assert_eq!(operations.events(), expected_cleanup_events());
    }

    #[test]
    fn cleanup_only_runs_enabled_actions() {
        let operations = FakeLifecycleOperations::new(true);
        let plan = CleanupPlan::new(TerminalCleanup::new(), operations.clone());
        plan.restore().unwrap();
        assert_eq!(operations.events(), terminal_cleanup_events());

        let operations = FakeLifecycleOperations::new(true);
        let plan = CleanupPlan {
            terminal: TerminalCleanup::new(),
            operations: operations.clone(),
            kitty: true,
            #[cfg(feature = "mouse")]
            mouse: false,
        };
        plan.restore().unwrap();
        assert_eq!(
            operations.events(),
            vec![
                LifecycleEvent::DisableKitty,
                LifecycleEvent::DisableRawMode,
                LifecycleEvent::LeaveAlternateScreen,
                LifecycleEvent::ShowCursor,
            ]
        );

        #[cfg(feature = "mouse")]
        {
            let operations = FakeLifecycleOperations::new(true);
            let plan = CleanupPlan {
                terminal: TerminalCleanup::new(),
                operations: operations.clone(),
                kitty: false,
                mouse: true,
            };
            plan.restore().unwrap();
            assert_eq!(
                operations.events(),
                vec![
                    LifecycleEvent::DisableMouse,
                    LifecycleEvent::DisableRawMode,
                    LifecycleEvent::LeaveAlternateScreen,
                    LifecycleEvent::ShowCursor,
                ]
            );
        }
    }

    #[test]
    fn cleanup_attempts_every_action_and_returns_the_first_error() {
        let operations = FakeLifecycleOperations::new(true);
        operations.fail_at(&expected_cleanup_events());
        let plan = full_plan(operations.clone());

        let error = plan.restore().unwrap_err();

        assert_eq!(error.to_string(), "injected DisableKitty error");
        assert_eq!(operations.events(), expected_cleanup_events());
    }

    #[test]
    fn every_cleanup_failure_is_reported_after_the_complete_trace() {
        for failure in expected_cleanup_events() {
            let operations = FakeLifecycleOperations::new(true);
            operations.fail_at(&[failure]);
            let plan = full_plan(operations.clone());

            let error = plan.restore().unwrap_err();

            assert_eq!(error.to_string(), format!("injected {failure:?} error"));
            assert_eq!(
                operations.events(),
                expected_cleanup_events(),
                "failure at {failure:?}"
            );
        }
    }

    #[test]
    fn cleanup_drop_never_panics_when_every_restoration_fails() {
        let operations = FakeLifecycleOperations::new(true);
        operations.fail_at(&expected_cleanup_events());
        let plan = full_plan(operations.clone());

        let result = catch_unwind(AssertUnwindSafe(|| drop(plan)));

        assert!(result.is_ok());
        assert_eq!(operations.events(), expected_cleanup_events());
    }

    #[test]
    fn mode_configuration_covers_disabled_and_unsupported_settings() {
        let operations = FakeLifecycleOperations::new(true);
        let mut plan = CleanupPlan::new(TerminalCleanup::new(), operations.clone());
        configure_modes(settings(false, false), &mut plan).unwrap();
        drop(plan);
        assert_eq!(operations.events(), terminal_cleanup_events());

        let operations = FakeLifecycleOperations::new(false);
        let mut plan = CleanupPlan::new(TerminalCleanup::new(), operations.clone());
        configure_modes(settings(true, false), &mut plan).unwrap();
        drop(plan);
        assert_eq!(
            operations.events(),
            vec![
                LifecycleEvent::QueryKitty,
                LifecycleEvent::DisableRawMode,
                LifecycleEvent::LeaveAlternateScreen,
                LifecycleEvent::ShowCursor,
            ]
        );
    }

    #[test]
    fn kitty_query_errors_are_treated_as_unsupported_without_leaking_terminal() {
        let operations = FakeLifecycleOperations::new(true);
        operations.fail_at(&[LifecycleEvent::QueryKitty]);
        let mut plan = CleanupPlan::new(TerminalCleanup::new(), operations.clone());

        configure_modes(settings(true, true), &mut plan).unwrap();
        drop(plan);

        assert_eq!(
            operations.events(),
            vec![
                #[cfg(feature = "mouse")]
                LifecycleEvent::EnableMouse,
                LifecycleEvent::QueryKitty,
                #[cfg(feature = "mouse")]
                LifecycleEvent::DisableMouse,
                LifecycleEvent::DisableRawMode,
                LifecycleEvent::LeaveAlternateScreen,
                LifecycleEvent::ShowCursor,
            ]
        );
    }

    #[test]
    fn kitty_enable_error_rolls_back_every_possibly_enabled_mode() {
        let operations = FakeLifecycleOperations::new(true);
        operations.fail_at(&[LifecycleEvent::EnableKitty]);
        let mut plan = CleanupPlan::new(TerminalCleanup::new(), operations.clone());

        assert!(configure_modes(settings(true, true), &mut plan).is_err());
        drop(plan);

        assert_eq!(
            operations.events(),
            vec![
                #[cfg(feature = "mouse")]
                LifecycleEvent::EnableMouse,
                LifecycleEvent::QueryKitty,
                LifecycleEvent::EnableKitty,
                LifecycleEvent::DisableKitty,
                #[cfg(feature = "mouse")]
                LifecycleEvent::DisableMouse,
                LifecycleEvent::DisableRawMode,
                LifecycleEvent::LeaveAlternateScreen,
                LifecycleEvent::ShowCursor,
            ]
        );
    }

    #[cfg(feature = "mouse")]
    #[test]
    fn mouse_enable_error_rolls_back_mouse_and_terminal() {
        let operations = FakeLifecycleOperations::new(true);
        operations.fail_at(&[LifecycleEvent::EnableMouse]);
        let mut plan = CleanupPlan::new(TerminalCleanup::new(), operations.clone());

        assert!(configure_modes(settings(true, true), &mut plan).is_err());
        drop(plan);

        assert_eq!(
            operations.events(),
            vec![
                LifecycleEvent::EnableMouse,
                LifecycleEvent::DisableMouse,
                LifecycleEvent::DisableRawMode,
                LifecycleEvent::LeaveAlternateScreen,
                LifecycleEvent::ShowCursor,
            ]
        );
    }

    #[cfg(feature = "mouse")]
    #[test]
    fn enabled_mouse_and_kitty_are_acquired_and_released_in_inverse_order() {
        let operations = FakeLifecycleOperations::new(true);
        let mut plan = CleanupPlan::new(TerminalCleanup::new(), operations.clone());

        configure_modes(settings(true, true), &mut plan).unwrap();
        drop(plan);

        assert_eq!(
            operations.events(),
            vec![
                LifecycleEvent::EnableMouse,
                LifecycleEvent::QueryKitty,
                LifecycleEvent::EnableKitty,
                LifecycleEvent::DisableKitty,
                LifecycleEvent::DisableMouse,
                LifecycleEvent::DisableRawMode,
                LifecycleEvent::LeaveAlternateScreen,
                LifecycleEvent::ShowCursor,
            ]
        );
    }

    #[test]
    fn panic_at_each_optional_mode_boundary_runs_partial_rollback() {
        let cases = [LifecycleEvent::QueryKitty, LifecycleEvent::EnableKitty];

        for panic_at in cases {
            let operations = FakeLifecycleOperations::new(true);
            operations.panic_at(panic_at);
            let result = catch_unwind(AssertUnwindSafe({
                let operations = operations.clone();
                move || {
                    let mut plan = CleanupPlan::new(TerminalCleanup::new(), operations);
                    let _ = configure_modes(settings(true, true), &mut plan);
                }
            }));
            assert!(result.is_err(), "{panic_at:?} did not panic");

            let expected = match panic_at {
                LifecycleEvent::QueryKitty => vec![
                    #[cfg(feature = "mouse")]
                    LifecycleEvent::EnableMouse,
                    LifecycleEvent::QueryKitty,
                    #[cfg(feature = "mouse")]
                    LifecycleEvent::DisableMouse,
                    LifecycleEvent::DisableRawMode,
                    LifecycleEvent::LeaveAlternateScreen,
                    LifecycleEvent::ShowCursor,
                ],
                LifecycleEvent::EnableKitty => vec![
                    #[cfg(feature = "mouse")]
                    LifecycleEvent::EnableMouse,
                    LifecycleEvent::QueryKitty,
                    LifecycleEvent::EnableKitty,
                    LifecycleEvent::DisableKitty,
                    #[cfg(feature = "mouse")]
                    LifecycleEvent::DisableMouse,
                    LifecycleEvent::DisableRawMode,
                    LifecycleEvent::LeaveAlternateScreen,
                    LifecycleEvent::ShowCursor,
                ],
                _ => unreachable!(),
            };
            assert_eq!(operations.events(), expected, "panic at {panic_at:?}");
        }
    }

    #[cfg(feature = "mouse")]
    #[test]
    fn panic_while_enabling_mouse_runs_partial_rollback() {
        let operations = FakeLifecycleOperations::new(true);
        operations.panic_at(LifecycleEvent::EnableMouse);

        let result = catch_unwind(AssertUnwindSafe({
            let operations = operations.clone();
            move || {
                let mut plan = CleanupPlan::new(TerminalCleanup::new(), operations);
                let _ = configure_modes(settings(true, true), &mut plan);
            }
        }));

        assert!(result.is_err());
        assert_eq!(
            operations.events(),
            vec![
                LifecycleEvent::EnableMouse,
                LifecycleEvent::DisableMouse,
                LifecycleEvent::DisableRawMode,
                LifecycleEvent::LeaveAlternateScreen,
                LifecycleEvent::ShowCursor,
            ]
        );
    }
}
