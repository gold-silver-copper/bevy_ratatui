use bevy::{
    app::{Plugin, PluginGroup, PluginGroupBuilder},
    prelude::{IntoScheduleConfigs, Result, SystemSet},
};

#[cfg(feature = "windowed")]
use bevy::prelude::Commands;

#[cfg(feature = "windowed")]
use crate::RatatuiContext;
use crate::context::DefaultContext;

use crate::context::TerminalContext;

#[cfg(all(feature = "crossterm", not(feature = "windowed")))]
use crate::crossterm_context::{cleanup::setup, context::CrosstermSettings};

/// A plugin group that includes all the plugins in the Ratatui crate.
///
/// # Example
///
/// ```rust
/// use bevy::prelude::*;
/// use bevy_ratatui::RatatuiPlugins;
///
/// App::new().add_plugins(RatatuiPlugins::default());
/// ```
pub struct RatatuiPlugins {
    /// Use kitty protocol if available and enabled.
    pub enable_kitty_protocol: bool,
    /// Capture mouse if enabled.
    pub enable_mouse_capture: bool,
    /// Forwards terminal input events to the bevy input system if enabled.
    pub enable_input_forwarding: bool,
}

impl Default for RatatuiPlugins {
    fn default() -> Self {
        Self {
            enable_kitty_protocol: true,
            enable_mouse_capture: false,
            enable_input_forwarding: false,
        }
    }
}

impl PluginGroup for RatatuiPlugins {
    fn build(self) -> PluginGroupBuilder {
        let mut builder = PluginGroupBuilder::start::<Self>();

        builder = builder.add(ContextPlugin);

        builder = DefaultContext::configure_plugin_group(&self, builder);

        builder
    }
}

/// The plugin responsible for owning the terminal session and adding the `RatatuiContext` resource.
///
/// With Crossterm, the complete session is acquired before its process-wide panic hook is installed.
/// Normal teardown restores terminal modes in order and reinstates the previous hook; panic cleanup
/// runs before that hook. Replacing the hook while the app is active, continuing after catching a
/// panic, panics on a thread that does not terminate the app, and concurrent panics are not
/// supported. Destructors cannot restore the terminal after `process::exit`, `SIGKILL`, or another
/// hard termination, and this crate does not install signal handlers for graceful signal recovery.
pub struct ContextPlugin;

/// Ordering set for work that must run after the terminal context has been acquired.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, SystemSet)]
pub struct ContextSetup;

impl Plugin for ContextPlugin {
    fn build(&self, app: &mut bevy::prelude::App) {
        #[cfg(all(feature = "crossterm", not(feature = "windowed")))]
        app.init_resource::<CrosstermSettings>()
            .add_systems(bevy::app::PreStartup, context_setup.in_set(ContextSetup));

        #[cfg(feature = "windowed")]
        app.add_systems(bevy::app::Startup, context_setup.in_set(ContextSetup));
    }
}

/// A startup system that sets up the terminal context.
#[cfg(all(feature = "crossterm", not(feature = "windowed")))]
pub fn context_setup(world: &mut bevy::prelude::World) -> Result {
    setup(world)
}

/// A startup system that sets up the terminal context.
#[cfg(feature = "windowed")]
pub fn context_setup(mut commands: Commands) -> Result {
    let terminal = RatatuiContext::init()?;
    commands.insert_resource(terminal);

    Ok(())
}
