//! Panic handling for the app.
//!
//! This module provides a plugin that sets up panic handling for the app. It installs a hook for
//! panic handling that restores the terminal before printing the panic. This ensures that the error
//! message is not messed up by the terminal state.
use std::panic;

use bevy::prelude::*;

use crate::ratatui_plugin::context_setup;

use super::cleanup::{CleanupHandle, report_cleanup_error};

/// A plugin that sets up panic handling.
///
/// This plugin installs a hook for panic handling that restores the terminal before printing the
/// panic message. This ensures that the panic message is not messed up by the terminal
/// state.
pub struct ErrorPlugin;

impl Plugin for ErrorPlugin {
    fn build(&self, app: &mut bevy::prelude::App) {
        app.init_resource::<CleanupHandle>()
            .add_systems(Startup, error_setup.before(context_setup));
    }
}

/// Installs a hook for panic handling.
///
/// Makes the app resilient to panics by restoring the terminal before printing the panic. This
/// prevents error messages from being messed up by the terminal state.
pub(crate) fn error_setup(cleanup: Res<CleanupHandle>) -> Result {
    let cleanup = CleanupHandle::clone(&cleanup);
    let panic_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        report_cleanup_error("restore terminal", cleanup.run());
        panic_hook(panic_info);
    }));

    Ok(())
}
