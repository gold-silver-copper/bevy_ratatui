//! This module contains the terminal plugin and the RatatuiContext resource.
//!
//! [`TerminalPlugin`] initializes the terminal, entering the alternate screen and enabling raw
//! mode. It also restores the terminal when the app is dropped.
//!
//! [`RatatuiContext`] is a wrapper [`Resource`] around ratatui::Terminal that automatically enters
//! and leaves the alternate screen.
use std::io::{self, Stdout, stdout};

use bevy::{app::AppExit, prelude::*};
use crossterm::{
    ExecutableCommand, cursor,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, prelude::Backend};
use soft_ratatui::SoftBackend;

use crate::{kitty::KittyEnabled, mouse::MouseCaptureEnabled};

/// A plugin that sets up the terminal.
///
/// This plugin initializes the terminal, entering the alternate screen and enabling raw mode. It
/// also restores the terminal when the app is dropped.
pub struct TerminalPlugin;

impl Plugin for TerminalPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup)
            .add_systems(PostUpdate, cleanup_system);
    }
}

/// A startup system that sets up the terminal.
pub fn setup(mut commands: Commands) -> Result {
    let terminal = RatatuiContext::default_init()?;
    commands.insert_resource(terminal);
    Ok(())
}

/// A cleanup system that ensures terminal enhancements are cleaned up in the correct order.
pub fn cleanup_system(mut commands: Commands, mut exit_reader: EventReader<AppExit>) {
    for _ in exit_reader.read() {
        commands.remove_resource::<KittyEnabled>();
        commands.remove_resource::<MouseCaptureEnabled>();
        commands.remove_resource::<RatatuiContext>();
    }
}

/// A wrapper around ratatui::Terminal that automatically enters and leaves the alternate screen.
///
/// This resource is used to draw to the terminal. It automatically enters the alternate screen when
/// it is initialized, and leaves the alternate screen when it is dropped.
///
/// # Example
///
/// ```rust
/// use bevy::prelude::*;
/// use bevy_ratatui::terminal::RatatuiContext;
///
/// fn draw_system(mut context: ResMut<RatatuiContext>) {
///     context.draw(|frame| {
///         // Draw widgets etc. to the terminal
///     });
/// }
/// ```

/// Trait for backend lifecycle management (init/restore).
pub trait TerminalBackendStrategy {
    type Backend: Backend;

    fn init_terminal() -> io::Result<Terminal<Self::Backend>>;
    fn restore_terminal() -> io::Result<()>;
}

/// The default Crossterm terminal backend strategy.
pub struct CrosstermStrategy;

impl TerminalBackendStrategy for CrosstermStrategy {
    type Backend = CrosstermBackend<Stdout>;

    fn init_terminal() -> io::Result<Terminal<Self::Backend>> {
        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;
        enable_raw_mode()?;
        let backend = CrosstermBackend::new(stdout);
        Terminal::new(backend)
    }

    fn restore_terminal() -> io::Result<()> {
        let mut stdout = io::stdout();
        stdout.execute(LeaveAlternateScreen)?;
        disable_raw_mode()
    }
}

/// Generic terminal context used in Bevy systems.
///
/// Defaults to `CrosstermStrategy`, so users can write just `RatatuiContext`.
#[derive(Resource, Deref, DerefMut)]
pub struct RatatuiContext<S: TerminalBackendStrategy = CrosstermStrategy> {
    terminal: Terminal<S::Backend>,
}

impl<S: TerminalBackendStrategy> RatatuiContext<S> {
    /// Initializes the terminal using the selected strategy.
    pub fn init() -> io::Result<Self> {
        let terminal = S::init_terminal()?;
        Ok(Self { terminal })
    }

    /// Restores the terminal using the selected strategy.
    pub fn restore() -> io::Result<()> {
        S::restore_terminal()
    }
}

impl<S: TerminalBackendStrategy> Drop for RatatuiContext<S> {
    fn drop(&mut self) {
        if let Err(err) = Self::restore() {
            eprintln!("Failed to restore terminal: {}", err);
        }
    }
}

impl RatatuiContext {
    pub fn default_init() -> io::Result<Self> {
        Self::init()
    }
    pub fn default_restore() -> io::Result<()> {
        Self::restore()
    }
}

pub struct WindowedStrategy;

impl TerminalBackendStrategy for WindowedStrategy {
    type Backend = SoftBackend;

    fn init_terminal() -> io::Result<Terminal<Self::Backend>> {
        let backend = SoftBackend::new_with_system_fonts(10, 10, 16);
        Terminal::new(backend)
    }

    fn restore_terminal() -> io::Result<()> {
        Ok(())
    }
}
