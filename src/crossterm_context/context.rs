use std::{
    io::{self, Stdout, stdout},
    ops::{Deref, DerefMut},
};

use bevy::prelude::*;

use ratatui::Terminal;
use ratatui::crossterm::{
    ExecutableCommand,
    terminal::{EnterAlternateScreen, enable_raw_mode},
};

use ratatui::backend::CrosstermBackend;

use crate::{RatatuiPlugins, context::TerminalContext};

use super::{
    cleanup::{CleanupHandle, CleanupPlugin, report_cleanup_error},
    error::ErrorPlugin,
    event::EventPlugin,
    kitty::KittyPlugin,
};

#[cfg(feature = "mouse")]
use super::mouse::MousePlugin;
#[cfg(feature = "keyboard")]
use super::translation::TranslationPlugin;

/// Ratatui context that will draw to the terminal buffer using crossterm.
#[derive(Debug)]
pub struct CrosstermContext {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    cleanup: CleanupHandle,
}

impl CrosstermContext {
    pub(crate) fn init_with_cleanup(cleanup: CleanupHandle) -> Result<Self> {
        let terminal = cleanup.setup(|| Self::initialize_terminal(&cleanup));

        match terminal {
            Ok(terminal) => Ok(Self { terminal, cleanup }),
            Err(err) => {
                report_cleanup_error("roll back terminal initialization", cleanup.run());
                Err(err.into())
            }
        }
    }

    fn initialize_terminal(
        cleanup: &CleanupHandle,
    ) -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
        let mut stdout = stdout();
        cleanup.enter_alternate_screen(|| {
            stdout.execute(EnterAlternateScreen)?;
            Ok(())
        })?;

        cleanup.enable_raw_mode(enable_raw_mode)?;
        cleanup.mark_cursor_may_be_hidden()?;

        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(terminal)
    }
}

impl Deref for CrosstermContext {
    type Target = Terminal<CrosstermBackend<Stdout>>;

    fn deref(&self) -> &Self::Target {
        &self.terminal
    }
}

impl DerefMut for CrosstermContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.terminal
    }
}

impl Drop for CrosstermContext {
    fn drop(&mut self) {
        report_cleanup_error("restore terminal", self.cleanup.run());
    }
}

impl TerminalContext<CrosstermBackend<Stdout>> for CrosstermContext {
    fn init() -> Result<Self> {
        Self::init_with_cleanup(CleanupHandle::default())
    }

    fn restore(&self) -> Result<()> {
        self.cleanup.run()?;
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
            .add(KittyPlugin);

        #[cfg(feature = "mouse")]
        let builder = builder.add(MousePlugin);
        #[cfg(feature = "keyboard")]
        let builder = builder.add(TranslationPlugin);

        let mut builder = builder;
        if !group.enable_kitty_protocol {
            builder = builder.disable::<KittyPlugin>();
        }

        #[cfg(feature = "mouse")]
        if !group.enable_mouse_capture {
            builder = builder.disable::<MousePlugin>();
        }

        #[cfg(feature = "keyboard")]
        if !group.enable_input_forwarding {
            builder = builder.disable::<TranslationPlugin>();
        }

        builder
    }
}
