use std::{
    io::{self, Write},
    sync::Arc,
};

use bevy::prelude::*;

use crate::RatatuiContext;

use super::{
    context::{CrosstermContext, CrosstermSettings, TerminalCleanup},
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
    Terminal,
}

struct CleanupPlan {
    terminal: TerminalCleanup,
    kitty: bool,
    #[cfg(feature = "mouse")]
    mouse: bool,
}

impl CleanupPlan {
    fn new(terminal: TerminalCleanup) -> Self {
        Self {
            terminal,
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
            record_error(&mut first_error, cleanup(CleanupAction::Terminal));

            first_error.map_or(Ok(()), Err)
        })
    }

    fn restore(&self) -> io::Result<()> {
        self.run(|action| match action {
            CleanupAction::Kitty => disable_kitty_protocol(),
            #[cfg(feature = "mouse")]
            CleanupAction::Mouse => disable_mouse_capture(),
            CleanupAction::Terminal => CrosstermContext::restore_terminal(),
        })
    }
}

impl Drop for CleanupPlan {
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
    let mut cleanup = CleanupPlan::new(context.0.take_cleanup());

    #[cfg(feature = "mouse")]
    if settings.enable_mouse_capture {
        cleanup.mouse = true;
        enable_mouse_capture()?;
    }

    if settings.enable_kitty_protocol && supports_kitty_protocol().unwrap_or(false) {
        cleanup.kitty = true;
        enable_kitty_protocol()?;
    }

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
    use super::*;

    fn full_plan() -> CleanupPlan {
        CleanupPlan {
            terminal: TerminalCleanup::new(),
            kitty: true,
            #[cfg(feature = "mouse")]
            mouse: true,
        }
    }

    fn expected_actions() -> Vec<CleanupAction> {
        vec![
            CleanupAction::Kitty,
            #[cfg(feature = "mouse")]
            CleanupAction::Mouse,
            CleanupAction::Terminal,
        ]
    }

    #[test]
    fn cleanup_runs_once_in_order() {
        let plan = full_plan();
        let mut actions = Vec::new();

        plan.run(|action| {
            actions.push(action);
            Ok(())
        })
        .unwrap();
        plan.run(|action| {
            actions.push(action);
            Ok(())
        })
        .unwrap();

        assert_eq!(actions, expected_actions());
    }

    #[test]
    fn cleanup_only_runs_enabled_actions() {
        let plan = CleanupPlan::new(TerminalCleanup::new());
        let mut actions = Vec::new();
        plan.run(|action| {
            actions.push(action);
            Ok(())
        })
        .unwrap();
        assert_eq!(actions, vec![CleanupAction::Terminal]);

        let plan = CleanupPlan {
            terminal: TerminalCleanup::new(),
            kitty: true,
            #[cfg(feature = "mouse")]
            mouse: false,
        };
        let mut actions = Vec::new();
        plan.run(|action| {
            actions.push(action);
            Ok(())
        })
        .unwrap();
        assert_eq!(actions, vec![CleanupAction::Kitty, CleanupAction::Terminal]);

        #[cfg(feature = "mouse")]
        {
            let plan = CleanupPlan {
                terminal: TerminalCleanup::new(),
                kitty: false,
                mouse: true,
            };
            let mut actions = Vec::new();
            plan.run(|action| {
                actions.push(action);
                Ok(())
            })
            .unwrap();
            assert_eq!(actions, vec![CleanupAction::Mouse, CleanupAction::Terminal]);
        }
    }

    #[test]
    fn cleanup_attempts_every_action_and_returns_the_first_error() {
        let plan = full_plan();
        let mut actions = Vec::new();

        let error = plan
            .run(|action| {
                actions.push(action);
                Err(io::Error::other(format!("{action:?}")))
            })
            .unwrap_err();

        assert_eq!(error.to_string(), "Kitty");
        assert_eq!(actions, expected_actions());
    }
}
