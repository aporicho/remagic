use super::{
    refresh_apps, settings, Action, Button, Display, HomeSettings, UiMode, WallpaperOption,
};
use ab_glyph::FontArc;
use remagic_protocol::{AppView, Request};

#[allow(clippy::too_many_arguments)]
pub(super) fn wallpapers_changed(
    display: &mut Display,
    font: &FontArc,
    buttons: &mut Vec<Button>,
    mode: UiMode,
    settings: &HomeSettings,
    wallpapers: &mut Vec<WallpaperOption>,
    page: &mut usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if mode == UiMode::WallpaperBrowser {
        *wallpapers = super::settings::wallpapers();
        redraw_wallpapers(display, font, buttons, settings, wallpapers, page)?;
    }
    Ok(())
}

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
    wallpaper_page: &mut usize,
) -> Result<bool, Box<dyn std::error::Error>> {
    if handle_wallpapers(
        action,
        display,
        font,
        buttons,
        mode,
        home_settings,
        wallpapers,
        wallpaper_page,
    )? {
        return Ok(true);
    }
    match (*mode, action) {
        (UiMode::Manager, Action::OpenSettings) => {
            *wallpapers = settings::wallpapers();
            refresh_system_settings(home_settings).await;
            *buttons = display.render_settings(font, home_settings, wallpapers)?;
            *mode = UiMode::Settings;
        }
        (UiMode::Settings, Action::BackManager) => {
            refresh_apps(apps).await;
            *buttons = display.render(font, apps)?;
            *mode = UiMode::Manager;
        }
        (UiMode::Settings, Action::ToggleWallpaperFit) => {
            home_settings.lock.fit.toggle();
            super::persist_settings(home_settings);
            *buttons = display.render_settings(font, home_settings, wallpapers)?;
        }
        (UiMode::Settings, Action::OpenBacklight) => {
            refresh_backlight_settings(home_settings).await;
            *buttons = display.render_backlight_settings(font, home_settings.backlight.as_ref())?;
            *mode = UiMode::Backlight;
        }
        (UiMode::Backlight, Action::BackSettings) => {
            *buttons = display.render_settings(font, home_settings, wallpapers)?;
            *mode = UiMode::Settings;
        }
        (UiMode::Backlight, Action::SetBacklight(percent)) => {
            set_backlight(*percent, home_settings).await;
            *buttons = display.render_backlight_settings(font, home_settings.backlight.as_ref())?;
        }
        (UiMode::Backlight, Action::AdjustBacklight(delta)) => {
            let current = home_settings
                .backlight
                .as_ref()
                .and_then(|snapshot| snapshot.percent)
                .unwrap_or(0) as i16;
            let percent = (current + *delta as i16).clamp(0, 100) as u8;
            set_backlight(percent, home_settings).await;
            *buttons = display.render_backlight_settings(font, home_settings.backlight.as_ref())?;
        }
        (UiMode::Settings, Action::CycleAutoSleep) => {
            let seconds = next_idle_suspend(home_settings.idle_suspend_secs);
            match crate::set_idle_suspend(seconds).await {
                Ok(snapshot) => home_settings.idle_suspend_secs = snapshot.idle_suspend_secs,
                Err(error) => eprintln!("remagic-home: idle suspend update failed: {error}"),
            }
            *buttons = display.render_settings(font, home_settings, wallpapers)?;
        }
        (UiMode::Settings, Action::System) => {
            if let Err(error) = super::request(Request::ReturnSystem).await {
                eprintln!("remagic-home: settings return failed: {error}");
            }
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

#[allow(clippy::too_many_arguments)]
fn handle_wallpapers(
    action: &Action,
    display: &mut Display,
    font: &FontArc,
    buttons: &mut Vec<Button>,
    mode: &mut UiMode,
    settings: &mut HomeSettings,
    wallpapers: &mut Vec<WallpaperOption>,
    page: &mut usize,
) -> Result<bool, Box<dyn std::error::Error>> {
    match (*mode, action) {
        (UiMode::Settings, Action::OpenWallpaperBrowser) => {
            *wallpapers = super::settings::wallpapers();
            super::display::prepare_wallpaper_thumbnails(wallpapers);
            let selected = wallpapers
                .iter()
                .position(|option| option.id == settings.lock.wallpaper)
                .unwrap_or(0);
            *page = selected / 8;
            redraw_wallpapers(display, font, buttons, settings, wallpapers, page)?;
            *mode = UiMode::WallpaperBrowser;
        }
        (UiMode::WallpaperBrowser, Action::BackSettings) => {
            *buttons = display.render_settings(font, settings, wallpapers)?;
            *mode = UiMode::Settings;
        }
        (UiMode::WallpaperBrowser, Action::SelectWallpaper(id)) => {
            if settings.select_wallpaper(id, wallpapers) {
                super::persist_settings(settings);
            }
            redraw_wallpapers(display, font, buttons, settings, wallpapers, page)?;
        }
        (UiMode::WallpaperBrowser, Action::WallpaperPage(delta)) => {
            *page = if *delta < 0 {
                (*page).saturating_sub(delta.unsigned_abs() as usize)
            } else {
                (*page).saturating_add(*delta as usize)
            };
            redraw_wallpapers(display, font, buttons, settings, wallpapers, page)?;
        }
        (UiMode::WallpaperBrowser, Action::ToggleWallpaperFit) => {
            settings.lock.fit.toggle();
            super::persist_settings(settings);
            redraw_wallpapers(display, font, buttons, settings, wallpapers, page)?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn redraw_wallpapers(
    display: &mut Display,
    font: &FontArc,
    buttons: &mut Vec<Button>,
    settings: &HomeSettings,
    wallpapers: &[WallpaperOption],
    page: &mut usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let (rendered, actual_page) =
        display.render_wallpaper_browser(font, settings, wallpapers, *page)?;
    *buttons = rendered;
    *page = actual_page;
    Ok(())
}

async fn refresh_system_settings(settings: &mut HomeSettings) {
    refresh_power_settings(settings).await;
    refresh_backlight_settings(settings).await;
}

async fn refresh_power_settings(settings: &mut HomeSettings) {
    match crate::power_status().await {
        Ok(snapshot) => settings.idle_suspend_secs = snapshot.idle_suspend_secs,
        Err(error) => eprintln!("remagic-home: power settings refresh failed: {error}"),
    }
}

pub(super) async fn refresh_backlight_settings(settings: &mut HomeSettings) {
    match crate::backlight_status().await {
        Ok(snapshot) => settings.backlight = Some(snapshot),
        Err(error) => {
            eprintln!("remagic-home: backlight settings refresh failed: {error}");
            settings.backlight = Some(remagic_core::BacklightSnapshot::unsupported(
                error.to_string(),
            ));
        }
    }
}

async fn set_backlight(percent: u8, settings: &mut HomeSettings) {
    match crate::set_backlight(percent).await {
        Ok(snapshot) => settings.backlight = Some(snapshot),
        Err(error) => {
            eprintln!("remagic-home: backlight update failed: {error}");
            refresh_backlight_settings(settings).await;
            if let Some(snapshot) = &mut settings.backlight {
                snapshot.error = Some(error.to_string());
            }
        }
    }
}

fn next_idle_suspend(current: u64) -> u64 {
    const OPTIONS: [u64; 6] = [60, 120, 300, 600, 1_800, 0];
    OPTIONS
        .iter()
        .position(|seconds| *seconds == current)
        .map_or(OPTIONS[0], |index| OPTIONS[(index + 1) % OPTIONS.len()])
}

#[cfg(test)]
mod tests {
    use super::next_idle_suspend;

    #[test]
    fn idle_suspend_options_form_a_stable_cycle() {
        assert_eq!(next_idle_suspend(60), 120);
        assert_eq!(next_idle_suspend(1_800), 0);
        assert_eq!(next_idle_suspend(0), 60);
        assert_eq!(next_idle_suspend(999), 60);
    }
}
