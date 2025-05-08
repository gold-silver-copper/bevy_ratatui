//! This module contains the terminal plugin and the RatatuiContext resource.
//!
//! [`TerminalPlugin`] initializes the terminal, entering the alternate screen and enabling raw
//! mode. It also restores the terminal when the app is dropped.
//!
//! [`RatatuiContext`] is a wrapper [`Resource`] around ratatui::Terminal that automatically enters
//! and leaves the alternate screen.
use std::io::{self, Stdout, stdout};

use bevy::{app::AppExit, prelude::*};
#[cfg(not(feature = "soft"))]
use crossterm::{
    ExecutableCommand, cursor,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::Terminal;
#[cfg(not(feature = "soft"))]
use ratatui::backend::CrosstermBackend;
#[cfg(feature = "soft")]
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
    let terminal = RatatuiContext::init()?;
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

/// Trait for terminal context lifecycle.
pub trait TerminalContext: Sized {
    /// Initializes the terminal and enters raw mode.
    fn init() -> io::Result<Self>;

    /// Restores the terminal to its normal state.
    fn restore() -> io::Result<()>;
}

/// Concrete terminal wrapper using Crossterm and Ratatui.
#[derive(Resource, Deref, DerefMut)]
#[cfg(not(feature = "soft"))]
pub struct RatatuiContext(Terminal<CrosstermBackend<Stdout>>);

#[cfg(not(feature = "soft"))]
impl TerminalContext for RatatuiContext {
    fn init() -> io::Result<Self> {
        let mut stdout = stdout();
        stdout.execute(EnterAlternateScreen)?;
        enable_raw_mode()?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(RatatuiContext(terminal))
    }

    fn restore() -> io::Result<()> {
        let mut stdout = stdout();
        stdout
            .execute(LeaveAlternateScreen)?
            .execute(cursor::Show)?;
        disable_raw_mode()?;
        Ok(())
    }
}
#[cfg(not(feature = "soft"))]
impl Drop for RatatuiContext {
    fn drop(&mut self) {
        if let Err(err) = Self::restore() {
            eprintln!("Failed to restore terminal: {}", err);
        }
    }
}

/// Concrete terminal wrapper using Crossterm and Ratatui.
#[derive(Resource, Deref, DerefMut)]
#[cfg(feature = "soft")]
pub struct RatatuiContext(Terminal<SoftBackend>);

#[cfg(feature = "soft")]
impl TerminalContext for RatatuiContext {
    fn init() -> io::Result<Self> {
        let backend = SoftBackend::new_with_system_fonts(15, 15, 16);
        let terminal = Terminal::new(backend)?;
        Ok(RatatuiContext(terminal))
    }

    fn restore() -> io::Result<()> {
        Ok(())
    }
}
#[cfg(feature = "soft")]
impl Drop for RatatuiContext {
    fn drop(&mut self) {
        if let Err(err) = Self::restore() {
            eprintln!("Failed to restore terminal: {}", err);
        }
    }
}
