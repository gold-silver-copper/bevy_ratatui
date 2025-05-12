use bevy::{app::AppExit, prelude::*};
#[cfg(not(feature = "windowed"))]
use bevy_ratatui::KeyEvent;
use bevy_ratatui::{RatatuiContext, RatatuiPlugins};
#[cfg(not(feature = "windowed"))]
use crossterm::event::KeyCode;
use ratatui::text::Text;

fn main() {
    #[cfg(not(feature = "windowed"))]
    App::new()
        .add_plugins((
            MinimalPlugins.set(bevy::app::ScheduleRunnerPlugin::run_loop(
                std::time::Duration::from_secs_f32(1. / 60.),
            )),
            RatatuiPlugins::default(),
        ))
        .add_systems(PreUpdate, input_system)
        .add_systems(Update, draw_system)
        .run();
    #[cfg(feature = "windowed")]
    App::new()
        .add_plugins((
            DefaultPlugins.set(ImagePlugin::default_nearest()),
            RatatuiPlugins::default(),
        ))
        .add_systems(PreUpdate, input_system_windowed)
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
#[cfg(not(feature = "windowed"))]
fn input_system(mut events: EventReader<KeyEvent>, mut exit: EventWriter<AppExit>) {
    for event in events.read() {
        if let KeyCode::Char('q') = event.code {
            exit.write_default();
        }
    }
}

#[cfg(feature = "windowed")]
fn input_system_windowed(keys: Res<ButtonInput<KeyCode>>, mut app_exit: EventWriter<AppExit>) {
    if keys.just_pressed(KeyCode::KeyQ) {
        app_exit.write_default();
    }
}
