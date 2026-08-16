//! Enhanced kitty keyboard protocol.
use std::io::{self, stdout};

use bevy::prelude::*;
use ratatui::crossterm::{
    ExecutableCommand,
    event::{KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags},
    terminal::supports_keyboard_enhancement,
};

use crate::ratatui_plugin::context_setup;

use super::{
    cleanup::{CleanupHandle, report_cleanup_error},
    error::error_setup,
};

/// Plugin responsible for enabling the Kitty keyboard protocol in the current buffer.
///
/// Provides additional information involving keyboard events. For example, key release events will
/// be reported.
///
/// Refer to the above link for a list of terminals that support the protocol. An `Ok` result is not
/// a guarantee that all features are supported: you should have fallbacks that you use until you
/// detect the event type you are looking for.
///
/// [kitty keyboard protocol]: https://sw.kovidgoyal.net/kitty/keyboard-protocol/
pub struct KittyPlugin;

impl Plugin for KittyPlugin {
    fn build(&self, app: &mut bevy::prelude::App) {
        app.init_resource::<CleanupHandle>()
            .add_systems(Startup, kitty_setup.after(context_setup).after(error_setup));
    }
}

pub(crate) fn kitty_setup(mut commands: Commands, cleanup: Res<CleanupHandle>) {
    if enable_kitty_protocol_with_cleanup(&cleanup).is_ok() {
        commands.insert_resource(KittyEnabled(CleanupHandle::clone(&cleanup)));
    } else {
        report_cleanup_error(
            "roll back Kitty keyboard enhancements",
            cleanup.disable_kitty(),
        );
    }
}

/// A resource indicating that the Kitty keyboard protocol was successfully enabled in the current
/// buffer.
#[derive(Resource)]
pub struct KittyEnabled(CleanupHandle);

impl Drop for KittyEnabled {
    fn drop(&mut self) {
        report_cleanup_error(
            "disable Kitty keyboard enhancements",
            self.0.disable_kitty(),
        );
    }
}

fn enable_kitty_protocol_with_cleanup(cleanup: &CleanupHandle) -> io::Result<()> {
    if supports_keyboard_enhancement()? {
        return cleanup.enable_kitty(push_kitty_protocol);
    }
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Kitty keyboard protocol is not supported by this terminal.",
    ))
}

fn push_kitty_protocol() -> io::Result<()> {
    stdout().execute(PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::all()))?;
    Ok(())
}

/// Disables the Kitty keyboard protocol, restoring the buffer to normal.
///
/// See [KittyPlugin].
pub fn disable_kitty_protocol() -> io::Result<()> {
    stdout().execute(PopKeyboardEnhancementFlags)?;
    Ok(())
}
