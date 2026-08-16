//! Mouse support.
#[cfg(not(feature = "windowed"))]
use std::io::stdout;

use bevy::prelude::*;
#[cfg(not(feature = "windowed"))]
use ratatui::crossterm::{
    ExecutableCommand,
    event::{DisableMouseCapture, EnableMouseCapture},
};

use super::context::CrosstermSettings;

/// Configures [`ContextPlugin`](crate::context::ContextPlugin) to enable mouse capture.
///
/// This plugin must be used together with `ContextPlugin`, which owns the terminal session.
pub struct MousePlugin;

impl Plugin for MousePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CrosstermSettings>();
        app.world_mut()
            .resource_mut::<CrosstermSettings>()
            .enable_mouse_capture = true;
    }
}

/// A resource inserted when mouse capture was successfully enabled by `ContextPlugin`.
#[derive(Resource, Default)]
pub struct MouseEnabled;

#[cfg(not(feature = "windowed"))]
pub(crate) fn enable_mouse_capture() -> std::io::Result<()> {
    stdout().execute(EnableMouseCapture)?;
    Ok(())
}

#[cfg(not(feature = "windowed"))]
pub(crate) fn disable_mouse_capture() -> std::io::Result<()> {
    stdout().execute(DisableMouseCapture)?;
    Ok(())
}
