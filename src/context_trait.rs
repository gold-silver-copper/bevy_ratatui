use std::ops::Deref;

use bevy::{app::PluginGroupBuilder, prelude::Result};
use ratatui::{Terminal, prelude::Backend};

use crate::RatatuiPlugins;

/// Trait for types that initialize a terminal context and configure its supporting plugins.
///
/// Implementors own any cleanup needed by the initialized context and must release it when that
/// context is dropped. They must also use `configure_plugin_group()` to add any systems, resources,
/// events, or other functionality needed by the associated Ratatui backend.
pub trait TerminalContext<T: Backend + 'static>:
    Sized + Send + Sync + Deref<Target = Terminal<T>> + 'static
{
    /// Initialize the terminal context.
    fn init() -> Result<Self>;

    /// Configure the plugin group to add the plugins necessary for this particular backend's
    /// functionality.
    fn configure_plugin_group(
        group: &RatatuiPlugins,
        builder: PluginGroupBuilder,
    ) -> PluginGroupBuilder;
}
