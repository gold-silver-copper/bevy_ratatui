//! Mouse support.
use std::io::stdout;

use bevy::prelude::*;
use ratatui::crossterm::{
    ExecutableCommand,
    event::{DisableMouseCapture, EnableMouseCapture},
};

use crate::ratatui_plugin::context_setup;

use super::{
    cleanup::{CleanupHandle, report_cleanup_error},
    error::error_setup,
};

/// Plugin responsible for enabling mouse capture.
pub struct MousePlugin;

impl Plugin for MousePlugin {
    fn build(&self, app: &mut bevy::prelude::App) {
        app.init_resource::<CleanupHandle>()
            .add_systems(Startup, mouse_setup.after(context_setup).after(error_setup));
    }
}

/// Resource indicating that mouse capture was successfully enabled in the current terminal buffer.
#[derive(Resource, Default)]
pub struct MouseEnabled(CleanupHandle);

pub(crate) fn mouse_setup(mut commands: Commands, cleanup: Res<CleanupHandle>) -> Result {
    let result = cleanup.enable_mouse(|| {
        stdout().execute(EnableMouseCapture)?;
        Ok(())
    });
    if let Err(err) = result {
        report_cleanup_error("roll back mouse capture", cleanup.disable_mouse());
        return Err(err.into());
    }

    commands.insert_resource(MouseEnabled(CleanupHandle::clone(&cleanup)));
    Ok(())
}

impl Drop for MouseEnabled {
    fn drop(&mut self) {
        report_cleanup_error("disable mouse capture", self.0.disable_mouse());
    }
}

/// Disables mouse capture.
///
/// See [MousePlugin].
pub(crate) fn disable_mouse_capture() -> std::io::Result<()> {
    stdout().execute(DisableMouseCapture)?;
    Ok(())
}
