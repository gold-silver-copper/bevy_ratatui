use std::{
    io::{self, Stdout, stdout},
    sync::atomic::{AtomicBool, Ordering},
};

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
pub struct CrosstermContext {
    #[deref]
    terminal: Terminal<CrosstermBackend<Stdout>>,
    cleanup: Option<TerminalCleanup>,
}

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
    pub(crate) fn take_cleanup(&mut self) -> TerminalCleanup {
        self.cleanup
            .take()
            .expect("terminal cleanup ownership is available")
    }
}

impl Drop for CrosstermContext {
    fn drop(&mut self) {
        // Dropping the token restores a directly owned terminal; a plugin-owned context has
        // already moved the token into its `TerminalSession`.
        drop(self.cleanup.take());
    }
}

/// The unique right to restore an initialized terminal.
///
/// The app session shares this token with its panic hook, so the atomic transition is what makes
/// cleanup exactly once across those two process-wide paths. Moving the token out of a direct
/// context transfers that right to the session without a second owner.
#[derive(Debug)]
pub(crate) struct TerminalCleanup {
    active: AtomicBool,
}

impl TerminalCleanup {
    pub(crate) fn new() -> Self {
        Self {
            active: AtomicBool::new(true),
        }
    }

    pub(crate) fn restore_with(&self, restore: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        if !self.active.swap(false, Ordering::AcqRel) {
            return Ok(());
        }

        restore()
    }

    fn restore(&self) -> io::Result<()> {
        self.restore_with(CrosstermContext::restore_terminal)
    }
}

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = self.restore();
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
        Ok(Self {
            terminal,
            cleanup: Some(TerminalCleanup::new()),
        })
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    #[test]
    fn terminal_cleanup_token_allows_one_restore() {
        let cleanup = TerminalCleanup::new();
        let calls = AtomicUsize::new(0);

        cleanup
            .restore_with(|| {
                calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            })
            .unwrap();
        cleanup
            .restore_with(|| {
                calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            })
            .unwrap();

        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
}
