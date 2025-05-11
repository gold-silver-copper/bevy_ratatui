use std::io::{Stdout, stdout};

use bevy::prelude::*;

use crossterm::{
    ExecutableCommand, cursor,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::Terminal;

use ratatui::backend::CrosstermBackend;

use crate::{RatatuiPlugins, TerminalContext};

use super::{
    cleanup::CleanupPlugin, error::ErrorPlugin, events::EventPlugin, kitty::KittyPlugin,
    mouse::MousePlugin, translation::TranslationPlugin,
};

#[derive(Deref, DerefMut, Debug)]
pub struct CrosstermContext(Terminal<CrosstermBackend<Stdout>>);

impl TerminalContext<CrosstermBackend<Stdout>> for CrosstermContext {
    fn init() -> Result<Self> {
        let mut stdout = stdout();
        stdout.execute(EnterAlternateScreen)?;
        enable_raw_mode()?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self(terminal))
    }

    fn restore() -> Result<()> {
        let mut stdout = stdout();
        stdout
            .execute(LeaveAlternateScreen)?
            .execute(cursor::Show)?;
        disable_raw_mode()?;
        Ok(())
    }

    fn configure_plugin_group(
        group: &RatatuiPlugins,
        mut builder: bevy::app::PluginGroupBuilder,
    ) -> bevy::app::PluginGroupBuilder {
        builder = builder
            .add(CleanupPlugin)
            .add(ErrorPlugin)
            .add(EventPlugin::default())
            .add(KittyPlugin)
            .add(MousePlugin)
            .add(TranslationPlugin);

        if !group.enable_kitty_protocol {
            builder = builder.disable::<KittyPlugin>();
        }
        if !group.enable_mouse_capture {
            builder = builder.disable::<MousePlugin>();
        }
        if !group.enable_input_forwarding {
            builder = builder.disable::<TranslationPlugin>();
        }

        builder
    }
}
