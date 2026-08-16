use std::{
    io::{self, Write},
    panic,
    sync::Arc,
    thread,
};

use bevy::ecs::schedule::ScheduleLabel;
use bevy::prelude::*;

use crate::RatatuiContext;

use super::{
    context::{CrosstermContext, CrosstermSettings},
    kitty::{KittyEnabled, disable_kitty_protocol, enable_kitty_protocol, supports_kitty_protocol},
};

#[cfg(feature = "mouse")]
use super::mouse::{MouseEnabled, disable_mouse_capture, enable_mouse_capture};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CleanupPlan {
    kitty: bool,
    #[cfg(feature = "mouse")]
    mouse: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, ScheduleLabel)]
pub(crate) struct TerminalStartup;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CleanupAction {
    Kitty,
    #[cfg(feature = "mouse")]
    Mouse,
    Terminal,
}

impl CleanupPlan {
    fn run(self, mut cleanup: impl FnMut(CleanupAction) -> io::Result<()>) -> io::Result<()> {
        let mut first_error = None;

        if self.kitty {
            record_error(&mut first_error, cleanup(CleanupAction::Kitty));
        }
        #[cfg(feature = "mouse")]
        if self.mouse {
            record_error(&mut first_error, cleanup(CleanupAction::Mouse));
        }
        record_error(&mut first_error, cleanup(CleanupAction::Terminal));

        first_error.map_or(Ok(()), Err)
    }
}

/// Rolls back each setup step that may have taken effect if initialization returns early.
struct SetupRollback(Option<CleanupPlan>);

impl SetupRollback {
    fn new() -> Self {
        Self(Some(CleanupPlan::default()))
    }

    fn plan(&self) -> CleanupPlan {
        self.0.expect("setup guard is active")
    }

    fn plan_mut(&mut self) -> &mut CleanupPlan {
        self.0.as_mut().expect("setup guard is active")
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for SetupRollback {
    fn drop(&mut self) {
        if let Some(plan) = self.0 {
            report_cleanup_error("roll back terminal initialization", restore_terminal(plan));
        }
    }
}

type PanicHook = dyn Fn(&panic::PanicHookInfo<'_>) + Send + Sync + 'static;

struct PreviousPanicHook {
    hook: Box<PanicHook>,
}

/// Owns the hook that was installed before the terminal session started.
struct PanicHookGuard(Option<Arc<PreviousPanicHook>>);

impl PanicHookGuard {
    fn restore_previous_hook(&mut self) {
        let Some(previous) = self.0.take() else {
            return;
        };

        drop(panic::take_hook());
        match Arc::try_unwrap(previous) {
            Ok(previous) => panic::set_hook(previous.hook),
            Err(previous) => panic::set_hook(Box::new(move |panic_info| {
                (previous.hook)(panic_info);
            })),
        }
    }
}

/// Owns cleanup after terminal setup has completed successfully.
///
/// Normal app destruction restores the terminal here. During unwinding, the installed panic hook
/// has already restored it before invoking the caller's hook, so `Drop` must not repeat cleanup.
#[derive(Resource)]
struct TerminalSession {
    plan: CleanupPlan,
    panic_hook: PanicHookGuard,
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if thread::panicking() {
            return;
        }

        report_cleanup_error("restore terminal", restore_terminal(self.plan));
        self.panic_hook.restore_previous_hook();
    }
}

pub(crate) fn setup(mut commands: Commands, settings: Res<CrosstermSettings>) -> Result {
    let context = RatatuiContext::init()?;
    let mut setup_rollback = SetupRollback::new();

    #[cfg(feature = "mouse")]
    if settings.enable_mouse_capture {
        setup_rollback.plan_mut().mouse = true;
        enable_mouse_capture()?;
    }

    if settings.enable_kitty_protocol && supports_kitty_protocol().unwrap_or(false) {
        setup_rollback.plan_mut().kitty = true;
        enable_kitty_protocol()?;
    }

    let plan = setup_rollback.plan();
    let panic_hook = install_panic_hook(plan);
    setup_rollback.disarm();

    commands.insert_resource(context);
    commands.insert_resource(TerminalSession { plan, panic_hook });
    if plan.kitty {
        commands.insert_resource(KittyEnabled);
    }
    #[cfg(feature = "mouse")]
    if plan.mouse {
        commands.insert_resource(MouseEnabled);
    }

    Ok(())
}

fn install_panic_hook(plan: CleanupPlan) -> PanicHookGuard {
    let previous = Arc::new(PreviousPanicHook {
        hook: panic::take_hook(),
    });
    let panic_hook = Arc::clone(&previous);
    panic::set_hook(Box::new(move |panic_info| {
        report_cleanup_error("restore terminal", restore_terminal(plan));
        (panic_hook.hook)(panic_info);
    }));

    PanicHookGuard(Some(previous))
}

fn restore_terminal(plan: CleanupPlan) -> io::Result<()> {
    plan.run(|action| match action {
        CleanupAction::Kitty => disable_kitty_protocol(),
        #[cfg(feature = "mouse")]
        CleanupAction::Mouse => disable_mouse_capture(),
        CleanupAction::Terminal => CrosstermContext::restore_terminal(),
    })
}

fn record_error(first_error: &mut Option<io::Error>, result: io::Result<()>) {
    if let Err(err) = result
        && first_error.is_none()
    {
        *first_error = Some(err);
    }
}

fn report_cleanup_error(action: &str, result: io::Result<()>) {
    if let Err(err) = result {
        let _ = writeln!(io::stderr(), "Failed to {action}: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_runs_in_order() {
        let plan = CleanupPlan {
            kitty: true,
            #[cfg(feature = "mouse")]
            mouse: true,
        };
        let mut actions = Vec::new();

        plan.run(|action| {
            actions.push(action);
            Ok(())
        })
        .unwrap();

        let expected = vec![
            CleanupAction::Kitty,
            #[cfg(feature = "mouse")]
            CleanupAction::Mouse,
            CleanupAction::Terminal,
        ];
        assert_eq!(actions, expected);
    }

    #[test]
    fn cleanup_attempts_every_action_and_returns_the_first_error() {
        let plan = CleanupPlan {
            kitty: true,
            #[cfg(feature = "mouse")]
            mouse: true,
        };
        let mut actions = Vec::new();

        let error = plan
            .run(|action| {
                actions.push(action);
                Err(io::Error::other(format!("{action:?}")))
            })
            .unwrap_err();

        assert_eq!(error.to_string(), "Kitty");
        assert_eq!(
            actions,
            vec![
                CleanupAction::Kitty,
                #[cfg(feature = "mouse")]
                CleanupAction::Mouse,
                CleanupAction::Terminal,
            ]
        );
    }
}
