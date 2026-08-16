use bevy::{
    app::{Plugin, PluginGroup, PluginGroupBuilder},
    prelude::{Commands, Result},
};

#[cfg(all(feature = "crossterm", not(feature = "windowed")))]
use bevy::app::MainScheduleOrder;
#[cfg(feature = "windowed")]
use bevy::app::Startup;
#[cfg(all(feature = "crossterm", not(feature = "windowed")))]
use bevy::ecs::schedule::ScheduleLabel;
#[cfg(all(feature = "crossterm", not(feature = "windowed")))]
use bevy::prelude::Res;

#[cfg(feature = "windowed")]
use crate::RatatuiContext;
use crate::context::DefaultContext;

use crate::context::TerminalContext;

#[cfg(all(feature = "crossterm", not(feature = "windowed")))]
use crate::crossterm_context::{
    context::CrosstermSettings,
    lifecycle::{TerminalStartup, setup},
};

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

/// The plugin responsible for adding the `RatatuiContext` resource and owning its lifecycle.
///
/// With the Crossterm backend, dropping the app restores the terminal and reinstates the previous
/// panic hook. A panic restores the terminal before invoking that hook. Continuing to run the app
/// after catching a panic and concurrent panics are unsupported. Install other panic hooks before
/// running the app; hooks installed later must chain to the existing hook.
pub struct ContextPlugin;

impl Plugin for ContextPlugin {
    fn build(&self, app: &mut bevy::prelude::App) {
        #[cfg(all(feature = "crossterm", not(feature = "windowed")))]
        app.init_resource::<CrosstermSettings>()
            .add_systems(TerminalStartup, context_setup);

        #[cfg(all(feature = "crossterm", not(feature = "windowed")))]
        {
            app.init_resource::<MainScheduleOrder>();
            let mut order = app.world_mut().resource_mut::<MainScheduleOrder>();
            order.startup_labels.insert(0, TerminalStartup.intern());
        }

        #[cfg(feature = "windowed")]
        app.add_systems(Startup, context_setup);
    }
}

/// A startup system that sets up the terminal context.
#[cfg(all(feature = "crossterm", not(feature = "windowed")))]
pub fn context_setup(commands: Commands, settings: Res<CrosstermSettings>) -> Result {
    setup(commands, settings)
}

/// A startup system that sets up the terminal context.
#[cfg(feature = "windowed")]
pub fn context_setup(mut commands: Commands) -> Result {
    let terminal = RatatuiContext::init()?;
    commands.insert_resource(terminal);

    Ok(())
}

#[cfg(all(test, feature = "crossterm", not(feature = "windowed")))]
mod tests {
    use bevy::ecs::schedule::ScheduleLabel;
    use bevy::prelude::App;

    use super::*;

    #[test]
    fn terminal_setup_precedes_user_startup() {
        let mut app = App::new();
        ContextPlugin.build(&mut app);
        let order = app.world().resource::<MainScheduleOrder>();

        assert_eq!(order.startup_labels[0], TerminalStartup.intern());
    }
}
