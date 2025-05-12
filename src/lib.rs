//! A collection of plugins for building terminal-based applications with [Bevy] and [Ratatui].
//!
//! # Example
//!
//! ```rust,no_run
//! use std::time::Duration;
//!
//! use bevy::{
//!     app::{AppExit, ScheduleRunnerPlugin},
//!     prelude::*,
//! };
//! use bevy_ratatui::{event::KeyEvent, terminal::RatatuiContext, RatatuiPlugins};
//! use crossterm::event::KeyCode;
//! use ratatui::text::Text;
//!
//! fn main() {
//!     let frame_time = Duration::from_secs_f32(1. / 60.);
//!
//!     App::new()
//!         .add_plugins((
//!             MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(frame_time)),
//!             RatatuiPlugins::default(),
//!         ))
//!         .add_systems(PreUpdate, input_system)
//!         .add_systems(Update, draw_system)
//!         .run();
//! }
//!
//! fn draw_system(mut context: ResMut<RatatuiContext>) -> Result {
//!     context.draw(|frame| {
//!         let text = Text::raw("hello world\npress 'q' to quit");
//!         frame.render_widget(text, frame.area());
//!     })?;
//!
//!     Ok(())
//! }
//!
//! fn input_system(mut events: EventReader<KeyEvent>, mut exit: EventWriter<AppExit>) {
//!     for event in events.read() {
//!         if let KeyCode::Char('q') = event.code {
//!             exit.send_default();
//!         }
//!     }
//! }
//! ```
//!
//! See the [examples] directory for more examples.
//!
//! # Input Forwarding
//!
//! The terminal input can be forwarded to the bevy input system. See the
//! [input_forwarding] module documentation for details.
//!
//! [Bevy]: https://bevyengine.org
//! [Ratatui]: https://ratatui.rs
//! [examples]: https://github.com/cxreiff/bevy_ratatui/tree/main/examples

mod context_trait;
mod crossterm_context;
mod plugins;
mod ratatui_context;
#[cfg(feature = "windowed")]
mod windowed_context;

pub use context_trait::TerminalContext;
pub use crossterm_context::context::CrosstermContext;
pub use crossterm_context::events::KeyEvent;
pub use crossterm_context::events::MouseEvent;
pub use crossterm_context::kitty::KittyEnabled;
pub use crossterm_context::translation::*;
pub use plugins::RatatuiPlugins;
pub use ratatui_context::RatatuiContext;
#[cfg(feature = "windowed")]
pub use windowed_context::context::WindowedContext;
