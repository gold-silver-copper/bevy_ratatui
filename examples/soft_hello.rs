use std::time::Duration;

use bevy::{app::AppExit, prelude::*};
use bevy_ratatui::{RatatuiPlugins, terminal::RatatuiContext};

use ratatui::text::Text;

fn main() {
    App::new()
        .add_plugins((
            DefaultPlugins,
            RatatuiPlugins {
                enable_input_forwarding: true,
                ..default()
            },
        ))
        .add_systems(PreUpdate, input_system)
        .add_systems(Update, draw_system)
        .run();
}

fn draw_system(mut context: ResMut<RatatuiContext>) -> Result {
    context.draw(|frame| {
        let text = Text::raw("hello world\npress 'q' to quit");
        frame.render_widget(text, frame.area());
    })?;

    Ok(())
}

fn input_system(keys: Res<ButtonInput<KeyCode>>) {
    if keys.just_pressed(KeyCode::KeyQ) {
        panic!("good bye")
    }
}
