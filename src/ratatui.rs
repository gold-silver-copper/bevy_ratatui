use bevy::{app::PluginGroupBuilder, prelude::*};

use crate::{error, kitty, mouse, terminal};
#[cfg(not(feature = "soft"))]
use crate::{event, input_forwarding};

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
impl PluginGroup for RatatuiPlugins {
    fn build(self) -> PluginGroupBuilder {
        let mut builder = PluginGroupBuilder::start::<Self>()
            .add(error::ErrorPlugin)
            .add(terminal::TerminalPlugin);

        #[cfg(not(feature = "soft"))]
        {
            builder = builder.add(event::EventPlugin::default());
        }
        #[cfg(feature = "soft")]
        {
            builder = builder.add(terminal::SoftRender);
        }

        if self.enable_kitty_protocol {
            builder = builder.add(kitty::KittyPlugin);
        }
        if self.enable_mouse_capture {
            builder = builder.add(mouse::MousePlugin);
        }
        #[cfg(not(feature = "soft"))]
        if self.enable_input_forwarding {
            builder = builder.add(input_forwarding::KeyboardPlugin);
        }
        builder
    }
}
