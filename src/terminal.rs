//! This module contains the terminal plugin and the RatatuiContext resource.
//!
//! [`TerminalPlugin`] initializes the terminal, entering the alternate screen and enabling raw
//! mode. It also restores the terminal when the app is dropped.
//!
//! [`RatatuiContext`] is a wrapper [`Resource`] around ratatui::Terminal that automatically enters
//! and leaves the alternate screen.
use std::io::{self, Stdout, stdout};

use bevy::{app::AppExit, prelude::*};

#[cfg(feature = "windowed")]
use bevy::{
    asset::RenderAssetUsages,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
    window::WindowResized,
};

use crossterm::{
    ExecutableCommand, cursor,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::Terminal;

use ratatui::backend::CrosstermBackend;
#[cfg(feature = "windowed")]
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
#[derive(Resource, Deref, DerefMut)]
#[cfg(not(feature = "windowed"))]
pub struct RatatuiContext(Terminal<CrosstermBackend<Stdout>>);

#[cfg(not(feature = "windowed"))]
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
#[cfg(not(feature = "windowed"))]
impl Drop for RatatuiContext {
    fn drop(&mut self) {
        if let Err(err) = Self::restore() {
            eprintln!("Failed to restore terminal: {}", err);
        }
    }
}
#[cfg(feature = "windowed")]
#[derive(Resource)]
struct TerminalRender(Handle<Image>);

/// Concrete terminal wrapper using Crossterm and Ratatui.
#[derive(Resource, Deref, DerefMut)]
#[cfg(feature = "windowed")]
pub struct RatatuiContext(Terminal<SoftBackend>);

#[cfg(feature = "windowed")]
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
#[cfg(feature = "windowed")]
impl Drop for RatatuiContext {
    fn drop(&mut self) {
        if let Err(err) = Self::restore() {
            eprintln!("Failed to restore terminal: {}", err);
        }
    }
}

#[cfg(feature = "windowed")]
pub struct SoftRender;
#[cfg(feature = "windowed")]
impl Plugin for SoftRender {
    fn build(&self, app: &mut App) {
        app.add_systems(PostStartup, terminal_render_setup)
            .add_systems(PreUpdate, handle_resize_events)
            .add_systems(Update, render_terminal_to_handle);
    }
}

/// A startup system that sets up the terminal.
#[cfg(feature = "windowed")]
pub fn terminal_render_setup(
    mut commands: Commands,
    softatui: ResMut<RatatuiContext>,
    mut images: ResMut<Assets<Image>>,
) -> Result {
    commands.spawn(bevy::core_pipeline::core_2d::Camera2d);
    // Create an image that we are going to draw into
    let width = softatui.backend().get_pixmap_width() as u32;
    let height = softatui.backend().get_pixmap_height() as u32;
    let data = softatui.backend().get_pixmap_data_as_rgba();

    let image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    let handle = images.add(image);
    commands.spawn(Sprite::from_image(handle.clone()));
    commands.insert_resource(TerminalRender(handle));
    Ok(())
}

#[cfg(feature = "windowed")]
fn render_terminal_to_handle(
    softatui: ResMut<RatatuiContext>,
    mut images: ResMut<Assets<Image>>,
    my_handle: Res<TerminalRender>,
) {
    let width = softatui.backend().get_pixmap_width() as u32;
    let height = softatui.backend().get_pixmap_height() as u32;
    let data = softatui.backend().get_pixmap_data_as_rgba();

    let imageik = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    let image = images.get_mut(&my_handle.0).expect("Image not found");
    *image = imageik;
}

/// System that reacts to window resize
#[cfg(feature = "windowed")]
fn handle_resize_events(
    mut resize_reader: EventReader<WindowResized>,
    mut softatui: ResMut<RatatuiContext>,
) {
    for event in resize_reader.read() {
        let cur_pix_width = softatui.backend().char_width;
        let cur_pix_height = softatui.backend().char_height;
        let av_wid = (event.width / cur_pix_width as f32) as u16;
        let av_hei = (event.height / cur_pix_height as f32) as u16;
        softatui.backend_mut().resize(av_wid, av_hei);
    }
}
