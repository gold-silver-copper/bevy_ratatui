//! Enhanced kitty keyboard protocol.
#[cfg(not(feature = "windowed"))]
use std::io::{self, stdout};

use bevy::prelude::*;
#[cfg(not(feature = "windowed"))]
use ratatui::crossterm::{
    ExecutableCommand,
    event::{KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags},
    terminal::supports_keyboard_enhancement,
};

use super::context::CrosstermSettings;

/// Configures [`ContextPlugin`](crate::context::ContextPlugin) to enable the Kitty keyboard
/// protocol in the current buffer.
///
/// Provides additional information involving keyboard events. For example, key release events will
/// be reported.
///
/// This plugin does not own a terminal session and must be used together with `ContextPlugin`.
///
/// Refer to the above link for a list of terminals that support the protocol. An `Ok` result is not
/// a guarantee that all features are supported: you should have fallbacks that you use until you
/// detect the event type you are looking for.
///
/// [kitty keyboard protocol]: https://sw.kovidgoyal.net/kitty/keyboard-protocol/
pub struct KittyPlugin;

impl Plugin for KittyPlugin {
    fn build(&self, app: &mut bevy::prelude::App) {
        app.init_resource::<CrosstermSettings>();
        app.world_mut()
            .resource_mut::<CrosstermSettings>()
            .enable_kitty_protocol = true;
    }
}

/// A resource inserted by `ContextPlugin` when the Kitty keyboard protocol was successfully enabled
/// in the current buffer.
#[derive(Resource)]
pub struct KittyEnabled;

#[cfg(not(feature = "windowed"))]
pub(crate) fn supports_kitty_protocol() -> io::Result<bool> {
    supports_keyboard_enhancement()
}

#[cfg(not(feature = "windowed"))]
pub(crate) fn enable_kitty_protocol() -> io::Result<()> {
    stdout().execute(PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::all()))?;
    Ok(())
}

/// Disables the Kitty keyboard protocol, restoring the buffer to normal.
///
/// See [KittyPlugin].
#[cfg(not(feature = "windowed"))]
pub(crate) fn disable_kitty_protocol() -> io::Result<()> {
    stdout().execute(PopKeyboardEnhancementFlags)?;
    Ok(())
}
