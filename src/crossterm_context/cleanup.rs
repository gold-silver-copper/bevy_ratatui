use bevy::prelude::*;

use crate::RatatuiContext;

use super::{kitty::KittyEnabled, mouse::MouseEnabled};

pub struct CleanupPlugin;

impl Plugin for CleanupPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(cleanup);
    }
}

fn cleanup(_trigger: Trigger<AppExit>, mut commands: Commands) {
    commands.remove_resource::<KittyEnabled>();
    commands.remove_resource::<MouseEnabled>();
    commands.remove_resource::<RatatuiContext>();
}
