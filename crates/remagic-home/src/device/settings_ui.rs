use super::{
    refresh_apps, settings, Action, Button, Display, HomeSettings, UiMode, WallpaperOption,
};
use ab_glyph::FontArc;
use remagic_protocol::AppView;

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle(
    action: &Action,
    display: &mut Display,
    font: &FontArc,
    apps: &mut Vec<AppView>,
    buttons: &mut Vec<Button>,
    mode: &mut UiMode,
    home_settings: &mut HomeSettings,
    wallpapers: &mut Vec<WallpaperOption>,
) -> Result<bool, Box<dyn std::error::Error>> {
    match (*mode, action) {
        (UiMode::Manager, Action::OpenSettings) => {
            *wallpapers = settings::wallpapers();
            *buttons = display.render_settings(font, home_settings, wallpapers)?;
            *mode = UiMode::Settings;
        }
        (UiMode::Settings, Action::BackManager) => {
            refresh_apps(apps).await;
            *buttons = display.render(font, apps)?;
            *mode = UiMode::Manager;
        }
        (UiMode::Settings, Action::CycleWallpaper) => {
            *wallpapers = settings::wallpapers();
            home_settings.cycle_wallpaper(wallpapers);
            super::persist_settings(home_settings);
            *buttons = display.render_settings(font, home_settings, wallpapers)?;
        }
        (UiMode::Settings, Action::ToggleWallpaperFit) => {
            home_settings.lock.fit.toggle();
            super::persist_settings(home_settings);
            *buttons = display.render_settings(font, home_settings, wallpapers)?;
        }
        (UiMode::Settings, Action::ToggleLockClock) => {
            home_settings.lock.show_clock = !home_settings.lock.show_clock;
            super::persist_settings(home_settings);
            *buttons = display.render_settings(font, home_settings, wallpapers)?;
        }
        (UiMode::Settings, Action::ToggleLockHint) => {
            home_settings.lock.show_hint = !home_settings.lock.show_hint;
            super::persist_settings(home_settings);
            *buttons = display.render_settings(font, home_settings, wallpapers)?;
        }
        (UiMode::Settings, Action::PreviewLock) => {
            *buttons = display.render_locked(font, home_settings, wallpapers, true)?;
            *mode = UiMode::LockPreview;
        }
        (UiMode::LockPreview, Action::BackSettings) => {
            *buttons = display.render_settings(font, home_settings, wallpapers)?;
            *mode = UiMode::Settings;
        }
        _ => return Ok(false),
    }
    Ok(true)
}
