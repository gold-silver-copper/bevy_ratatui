use std::fmt::Debug;

use bevy::prelude::*;

use ratatui::Terminal;

use soft_ratatui::SoftBackend;

use crate::TerminalContext;

use super::other::SoftRenderPlugin;

#[derive(Deref, DerefMut)]
pub struct WindowedContext(Terminal<SoftBackend>);

impl Debug for WindowedContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WindowedContext()")
    }
}

impl TerminalContext<SoftBackend> for WindowedContext {
    fn init() -> Result<Self> {
        let backend = SoftBackend::new_with_system_fonts(15, 15, 16);
        let terminal = Terminal::new(backend)?;
        Ok(Self(terminal))
    }

    fn restore() -> Result<()> {
        Ok(())
    }

    fn configure_plugin_group(
        _group: &crate::RatatuiPlugins,
        mut builder: bevy::app::PluginGroupBuilder,
    ) -> bevy::app::PluginGroupBuilder {
        builder = builder.add(SoftRenderPlugin);

        builder
    }
}
