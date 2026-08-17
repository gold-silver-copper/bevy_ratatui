use bevy::{
    app::{Plugin, PluginGroup, PluginGroupBuilder},
    prelude::*,
};

use crate::RatatuiContext;
#[cfg(all(feature = "crossterm", not(feature = "windowed")))]
use crate::context::{CrosstermSession, SessionOptions};

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

        #[cfg(all(feature = "crossterm", not(feature = "windowed")))]
        {
            use crate::crossterm_context::event::EventPlugin;

            let options = SessionOptions {
                #[cfg(feature = "mouse")]
                mouse_capture: self.enable_mouse_capture,
                kitty_keyboard: self.enable_kitty_protocol,
                ..SessionOptions::default()
            };
            builder = builder
                .add(ContextPlugin { options })
                .add(EventPlugin::default());

            #[cfg(feature = "keyboard")]
            {
                use crate::crossterm_context::translation::TranslationPlugin;

                builder = builder.add(TranslationPlugin);
                if !self.enable_input_forwarding {
                    builder = builder.disable::<TranslationPlugin>();
                }
            }
        }

        #[cfg(feature = "windowed")]
        {
            use crate::windowed_context::plugin::WindowedPlugin;

            builder = builder.add(ContextPlugin::default()).add(WindowedPlugin);
        }

        builder
    }
}

/// The plugin that creates the [`RatatuiContext`] resource.
///
/// With the Crossterm backend it acquires the one process-wide terminal session in `PreStartup`,
/// before user startup systems, and inserts it as [`RatatuiContext`] together with the
/// `KittyEnabled` and `MouseEnabled` markers for the optional modes that were actually enabled.
/// The session restores the terminal exactly once when the resource is dropped or when a panic
/// hook fires; see `context::CrosstermSession`.
#[derive(Clone, Debug, Default)]
pub struct ContextPlugin {
    /// Terminal session options.
    #[cfg(all(feature = "crossterm", not(feature = "windowed")))]
    pub options: SessionOptions,
}

/// The system set in which [`ContextPlugin`] creates [`RatatuiContext`]. Order startup systems
/// that need the context `.after(ContextSetup)`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, SystemSet)]
pub struct ContextSetup;

impl Plugin for ContextPlugin {
    fn build(&self, app: &mut App) {
        #[cfg(all(feature = "crossterm", not(feature = "windowed")))]
        {
            let options = self.options;
            app.add_systems(
                PreStartup,
                (move |world: &mut World| -> Result { acquire_session(world, options) })
                    .in_set(ContextSetup),
            );
        }

        #[cfg(feature = "windowed")]
        app.add_systems(Startup, windowed_setup.in_set(ContextSetup));
    }
}

#[cfg(all(feature = "crossterm", not(feature = "windowed")))]
fn acquire_session(world: &mut World, options: SessionOptions) -> Result {
    let session = CrosstermSession::acquire(options)?;
    let acquired = session.acquired();
    if acquired.kitty_keyboard {
        world.insert_resource(KittyEnabled);
    }
    #[cfg(feature = "mouse")]
    if acquired.mouse_capture {
        world.insert_resource(MouseEnabled);
    }
    world.insert_resource(RatatuiContext(session));
    Ok(())
}

#[cfg(feature = "windowed")]
fn windowed_setup(mut commands: Commands) -> Result {
    commands.insert_resource(RatatuiContext::init()?);
    Ok(())
}

/// Marker resource: the kitty keyboard enhancement flags are pushed for the current session.
///
/// Absent when the protocol was disabled in [`RatatuiPlugins`] or unsupported by the terminal.
/// An enabled protocol is not a guarantee that every feature is supported; keep fallbacks until you
/// observe the event kinds you rely on. The flags are popped by the session, not by this resource.
///
/// See the [kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/).
#[cfg(feature = "crossterm")]
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct KittyEnabled;

/// Marker resource: mouse capture is enabled for the current session.
///
/// Mouse capture is released by the session, not by this resource.
#[cfg(all(feature = "crossterm", feature = "mouse"))]
#[derive(Resource, Clone, Copy, Debug, Default)]
pub struct MouseEnabled;
