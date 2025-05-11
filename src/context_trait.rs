use std::ops::Deref;

use bevy::{app::PluginGroupBuilder, prelude::Result};
use ratatui::{Terminal, prelude::Backend};

use crate::RatatuiPlugins;

/// Trait for types that implement lifecycle functions for initializing a terminal context and
/// restoring the terminal state after exiting.
pub trait TerminalContext<T: Backend + 'static>:
    Sized + Send + Sync + Deref<Target = Terminal<T>> + 'static
{
    /// Initialize the terminal context.
    fn init() -> Result<Self>;

    /// Restore the terminal to its normal state after exiting.
    fn restore() -> Result<()>;

    fn configure_plugin_group(
        group: &RatatuiPlugins,
        builder: PluginGroupBuilder,
    ) -> PluginGroupBuilder;
}
