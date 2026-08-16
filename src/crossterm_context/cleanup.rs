use std::{
    io::{self, Write},
    thread,
};

use bevy::prelude::*;

use crate::RatatuiContext;

use super::{
    context::{CrosstermContext, CrosstermSettings},
    error::PanicHookGuard,
    kitty::{KittyEnabled, disable_kitty_protocol, enable_kitty_protocol, supports_kitty_protocol},
};

#[cfg(feature = "mouse")]
use super::mouse::{MouseEnabled, disable_mouse_capture, enable_mouse_capture};

#[derive(Clone, Copy, Default)]
struct CleanupPlan {
    kitty: bool,
    #[cfg(feature = "mouse")]
    mouse: bool,
}

impl CleanupPlan {
    fn restore(self) -> io::Result<()> {
        let mut first_error = None;

        if self.kitty {
            record_error(&mut first_error, disable_kitty_protocol());
        }
        #[cfg(feature = "mouse")]
        if self.mouse {
            record_error(&mut first_error, disable_mouse_capture());
        }
        record_error(&mut first_error, CrosstermContext::restore_terminal());

        first_error.map_or(Ok(()), Err)
    }
}

#[derive(Resource)]
struct TerminalSession {
    cleanup: CleanupPlan,
    hook: PanicHookGuard,
}

impl TerminalSession {
    fn new(context: &mut CrosstermContext) -> Self {
        context.relinquish_cleanup();
        Self {
            cleanup: CleanupPlan::default(),
            hook: PanicHookGuard::default(),
        }
    }

    fn install_hook(&mut self) {
        let cleanup = self.cleanup;
        self.hook = PanicHookGuard::install(move || report_cleanup_error(cleanup.restore()));
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if thread::panicking() && self.hook.is_installed() {
            return;
        }

        report_cleanup_error(self.cleanup.restore());
        self.hook.restore();
    }
}

pub(crate) fn setup(world: &mut World) -> Result {
    let settings = *world.resource::<CrosstermSettings>();
    let mut context = RatatuiContext::init()?;
    let mut session = TerminalSession::new(&mut context.0);

    #[cfg(feature = "mouse")]
    if settings.enable_mouse_capture {
        session.cleanup.mouse = true;
        enable_mouse_capture()?;
    }

    if settings.enable_kitty_protocol && supports_kitty_protocol().unwrap_or(false) {
        session.cleanup.kitty = true;
        enable_kitty_protocol()?;
    }

    session.install_hook();
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
