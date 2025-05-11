//! Mouse support.
use std::io::stdout;

use bevy::prelude::*;
use crossterm::{
    ExecutableCommand,
    event::{DisableMouseCapture, EnableMouseCapture},
};

pub struct MousePlugin;

impl Plugin for MousePlugin {
    fn build(&self, app: &mut bevy::prelude::App) {
        app.add_systems(Startup, mouse_setup);
    }
}

#[derive(Resource, Default)]
pub struct MouseEnabled;

fn mouse_setup(mut commands: Commands) -> Result {
    stdout().execute(EnableMouseCapture)?;
    commands.insert_resource(MouseEnabled);
    Ok(())
}

impl Drop for MouseEnabled {
    fn drop(&mut self) {
        let _ = stdout().execute(DisableMouseCapture);
    }
}
