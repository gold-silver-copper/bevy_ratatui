use std::io::{self, Stdout, stdout};

use bevy::prelude::*;

use ratatui::Terminal;
use ratatui::crossterm::{
    ExecutableCommand, cursor,
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        is_raw_mode_enabled,
    },
};

use ratatui::backend::CrosstermBackend;

use crate::{RatatuiPlugins, context::TerminalContext};

use super::{event::EventPlugin, kitty::KittyPlugin};

#[cfg(feature = "mouse")]
use super::mouse::MousePlugin;
#[cfg(feature = "keyboard")]
use super::translation::TranslationPlugin;

/// Ratatui context that will draw to the terminal buffer using crossterm.
#[derive(Deref, DerefMut, Debug)]
pub struct CrosstermContext(#[deref] Terminal<CrosstermBackend<Stdout>>, bool);

#[derive(Clone, Copy, Default, Resource)]
pub(crate) struct CrosstermSettings {
    pub(crate) enable_kitty_protocol: bool,
    #[cfg(feature = "mouse")]
    pub(crate) enable_mouse_capture: bool,
}

#[derive(Default)]
struct InitializationGuard {
    raw_mode: bool,
    alternate_screen: bool,
}

impl InitializationGuard {
    fn disarm(&mut self) {
        self.raw_mode = false;
        self.alternate_screen = false;
    }
}

impl Drop for InitializationGuard {
    fn drop(&mut self) {
        if self.raw_mode {
            let _ = disable_raw_mode();
        }
        if self.alternate_screen {
            let _ = stdout().execute(LeaveAlternateScreen);
        }
    }
}

impl CrosstermContext {
    pub(crate) fn restore_terminal() -> io::Result<()> {
        let mut stdout = stdout();
        let raw_mode = disable_raw_mode();
        let alternate_screen = stdout.execute(LeaveAlternateScreen).map(|_| ());
        let cursor = stdout.execute(cursor::Show).map(|_| ());

        raw_mode.and(alternate_screen).and(cursor)
    }

    #[cfg(not(feature = "windowed"))]
    pub(crate) fn relinquish_cleanup(&mut self) {
        self.1 = false;
    }
}

impl Drop for CrosstermContext {
    fn drop(&mut self) {
        if self.1 {
            let _ = Self::restore_terminal();
        }
    }
}

impl TerminalContext<CrosstermBackend<Stdout>> for CrosstermContext {
    fn init() -> Result<Self> {
        if is_raw_mode_enabled()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the terminal is already in raw mode",
            )
            .into());
        }

        let mut rollback = InitializationGuard {
            raw_mode: true,
            alternate_screen: false,
        };
        let mut stdout = stdout();
        enable_raw_mode()?;
        rollback.alternate_screen = true;
        stdout.execute(EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;

        rollback.disarm();
        Ok(Self(terminal, true))
    }

    fn restore() -> Result<()> {
        Self::restore_terminal()?;
        Ok(())
    }

    fn configure_plugin_group(
        group: &RatatuiPlugins,
        mut builder: bevy::app::PluginGroupBuilder,
    ) -> bevy::app::PluginGroupBuilder {
        builder = builder.add(EventPlugin::default()).add(KittyPlugin);

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
