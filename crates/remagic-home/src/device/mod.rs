mod display;
mod first_run;
mod home_events;
mod release;
mod settings;
mod settings_ui;
mod store;

use crate::{domain_state, list_apps, request};
use display::Display;
use remagic_core::{AppId, DomainState};
use remagic_protocol::{AppView, Request};
use settings::{HomeSettings, WallpaperOption};
use std::time::Duration;

#[derive(Clone)]
pub(super) enum Action {
    Launch(AppId),
    Close(AppId),
    OpenStore,
    StoreInstall(String),
    StoreUpgrade(String),
    StoreUninstall(String),
    SystemUpdate,
    Unavailable,
    System,
    Sleep,
    OpenSettings,
    BackManager,
    BackSettings,
    OpenWallpaperBrowser,
    SelectWallpaper(String),
    WallpaperPage(i32),
    ToggleWallpaperFit,
    CycleAutoSleep,
    PreviewLock,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UiMode {
    Welcome,
    Manager,
    Settings,
    WallpaperBrowser,
    Store,
    LockPreview,
    Locked,
}

#[derive(Clone)]
pub(super) struct Button {
    pub(super) x: i32,
    pub(super) y: i32,
    pub(super) width: i32,
    pub(super) height: i32,
    pub(super) action: Action,
}

pub(super) async fn run(mut apps: Vec<AppView>) -> Result<(), Box<dyn std::error::Error>> {
    settings::ensure_wallpaper_dir();
    let mut settings = HomeSettings::load();
    let mut wallpapers = settings::wallpapers();
    let mut display = Display::open()?;
    let home_events = home_events::Receiver::bind()?;
    let font = display::load_font()?;
    let (mut mode, mut buttons) =
        initial_page(&mut display, &font, &apps, &settings, &wallpapers).await?;
    let mut pressed: Option<(Button, Vec<u32>)> = None;
    let mut store_error: Option<String> = None;
    let mut store_catalog = Vec::new();
    let mut system_update = store::SystemUpdateInfo::default();
    let mut wallpaper_page = 0_usize;
    loop {
        for event in home_events.drain()? {
            match event {
                home_events::Event::AutoSleep if mode != UiMode::Locked => {
                    pressed = None;
                    let mut context = release::Context {
                        display: &mut display,
                        font: &font,
                        apps: &mut apps,
                        buttons: &mut buttons,
                        mode: &mut mode,
                        settings: &mut settings,
                        wallpapers: &mut wallpapers,
                        store_error: &mut store_error,
                        store_catalog: &mut store_catalog,
                        system_update: &mut system_update,
                        wallpaper_page: &mut wallpaper_page,
                    };
                    release::sleep(&mut context).await?;
                }
                home_events::Event::AutoSleep => {}
                home_events::Event::ResumeUnlock if mode == UiMode::Locked => {
                    pressed = None;
                    unlock(
                        &mut display,
                        &font,
                        &mut apps,
                        &mut buttons,
                        &mut mode,
                        &settings,
                        &wallpapers,
                    )
                    .await?;
                }
                home_events::Event::ResumeUnlock => {}
                home_events::Event::WallpapersChanged => settings_ui::wallpapers_changed(
                    &mut display,
                    &font,
                    &mut buttons,
                    mode,
                    &settings,
                    &mut wallpapers,
                    &mut wallpaper_page,
                )?,
            }
        }
        for event in display.poll_touch_events()? {
            match event {
                crate::qtfb::TouchEvent::Press { x, y } => {
                    handle_press(&mut display, &buttons, &mut pressed, x, y)?;
                }
                crate::qtfb::TouchEvent::Release { x, y } => {
                    release::handle(
                        release::Context {
                            display: &mut display,
                            font: &font,
                            apps: &mut apps,
                            buttons: &mut buttons,
                            mode: &mut mode,
                            settings: &mut settings,
                            wallpapers: &mut wallpapers,
                            store_error: &mut store_error,
                            store_catalog: &mut store_catalog,
                            system_update: &mut system_update,
                            wallpaper_page: &mut wallpaper_page,
                        },
                        &mut pressed,
                        x,
                        y,
                    )
                    .await?;
                }
            }
        }
        home_events.wait_with_input(display.input_fd(), None)?;
    }
}

async fn initial_page(
    display: &mut Display,
    font: &ab_glyph::FontArc,
    apps: &[AppView],
    settings: &HomeSettings,
    wallpapers: &[WallpaperOption],
) -> Result<(UiMode, Vec<Button>), Box<dyn std::error::Error>> {
    let domain = domain_state().await?;
    let mode = if matches!(&domain, DomainState::Sleeping) {
        UiMode::Locked
    } else if first_run::pending() {
        UiMode::Welcome
    } else {
        UiMode::Manager
    };
    let buttons = match mode {
        UiMode::Welcome => {
            let device = remagic_device::DeviceProfile::detect()?;
            let name = match device.product {
                remagic_device::DeviceProduct::PaperPro => "reMarkable Paper Pro",
                remagic_device::DeviceProduct::PaperProMove => "reMarkable Paper Pro Move",
            };
            display.render_welcome(font, name)?
        }
        UiMode::Manager => display.render(font, apps)?,
        UiMode::Locked => display.render_locked(font, settings, wallpapers, false)?,
        UiMode::Settings | UiMode::WallpaperBrowser | UiMode::Store | UiMode::LockPreview => {
            unreachable!()
        }
    };
    // A restarted Home commits a new surface and then requests one idempotent
    // foreground transaction; no periodic supervisor polling is required.
    if matches!(&domain, DomainState::Manager) {
        request(Request::OpenManager).await?;
    }
    Ok((mode, buttons))
}

fn persist_settings(settings: &HomeSettings) {
    if let Err(error) = settings.save() {
        eprintln!("remagic-home: cannot save settings: {error}");
    }
}

async fn unlock(
    display: &mut Display,
    font: &ab_glyph::FontArc,
    apps: &mut Vec<AppView>,
    buttons: &mut Vec<Button>,
    mode: &mut UiMode,
    settings: &HomeSettings,
    wallpapers: &[WallpaperOption],
) -> Result<(), Box<dyn std::error::Error>> {
    // Render the manager page behind the frozen lock. Unlock presents this
    // exact sequence and releases input as one panel transaction.
    refresh_apps(apps).await;
    let manager_buttons = display.render(font, apps)?;
    let manager_surface_sequence = display.commit_sequence();
    match request(Request::Wake {
        manager_surface_sequence,
    })
    .await
    {
        Ok(()) => {
            *buttons = manager_buttons;
            *mode = UiMode::Manager;
        }
        Err(error) => {
            eprintln!("remagic-home: unlock acknowledgement failed: {error}");
            let authoritative_domain = domain_state().await.ok();
            if unlock_committed_after_lost_ack(authoritative_domain.as_ref()) {
                // The display transaction committed but its final reply was
                // lost. Do not flash the obsolete lock page back on screen.
                *buttons = manager_buttons;
                *mode = UiMode::Manager;
            } else {
                *buttons = display.render_locked(font, settings, wallpapers, false)?;
                *mode = UiMode::Locked;
            }
        }
    }
    Ok(())
}

fn unlock_committed_after_lost_ack(domain: Option<&DomainState>) -> bool {
    matches!(domain, Some(DomainState::Manager))
}

fn handle_press(
    display: &mut Display,
    buttons: &[Button],
    pressed: &mut Option<(Button, Vec<u32>)>,
    x: i32,
    y: i32,
) -> std::io::Result<()> {
    if pressed.is_none() {
        if let Some(button) = button_at(buttons, x, y).cloned() {
            *pressed = Some((button.clone(), display.press(&button)?));
        }
    }
    Ok(())
}

async fn execute_action(
    action: Action,
    apps: &mut Vec<AppView>,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        Action::Launch(app_id) => {
            request(Request::Launch {
                app_id,
                open_path: None,
            })
            .await
        }
        Action::Close(app_id) => {
            request(Request::Close {
                app_id: app_id.clone(),
                complete: true,
            })
            .await?;
            wait_until_closed(&app_id, apps).await
        }
        Action::Unavailable => Ok(()),
        Action::System => request(Request::ReturnSystem).await,
        Action::Sleep => Err("sleep must render and fence the lock page first".into()),
        Action::OpenSettings
        | Action::BackManager
        | Action::BackSettings
        | Action::OpenWallpaperBrowser
        | Action::SelectWallpaper(_)
        | Action::WallpaperPage(_)
        | Action::OpenStore
        | Action::StoreInstall(_)
        | Action::StoreUpgrade(_)
        | Action::StoreUninstall(_)
        | Action::SystemUpdate
        | Action::ToggleWallpaperFit
        | Action::CycleAutoSleep
        | Action::PreviewLock => Err("settings actions must stay inside Home".into()),
    }
}

async fn reconcile_mode(
    display: &mut Display,
    font: &ab_glyph::FontArc,
    apps: &mut Vec<AppView>,
    buttons: &mut Vec<Button>,
    mode: &mut UiMode,
    settings: &HomeSettings,
    wallpapers: &[WallpaperOption],
) -> Result<(), Box<dyn std::error::Error>> {
    let domain = match domain_state().await {
        Ok(domain) => domain,
        Err(error) => {
            eprintln!("remagic-home: status refresh failed: {error}");
            return Ok(());
        }
    };
    match domain {
        DomainState::Sleeping => {
            if *mode != UiMode::Locked {
                *buttons = display.render_locked(font, settings, wallpapers, false)?;
                *mode = UiMode::Locked;
            }
        }
        DomainState::Manager if *mode != UiMode::Manager => {
            refresh_apps(apps).await;
            *buttons = display.render(font, apps)?;
            *mode = UiMode::Manager;
        }
        _ => {}
    }
    Ok(())
}

fn retain_lock_after_failed_sleep(domain: Option<&DomainState>) -> bool {
    matches!(domain, Some(DomainState::Sleeping))
}

async fn refresh_apps(apps: &mut Vec<AppView>) {
    match list_apps().await {
        Ok(latest) => *apps = latest,
        Err(error) => {
            // Preserve the last coherent snapshot across a transient daemon
            // rollback instead of clearing the task page.
            eprintln!("remagic-home: app refresh failed: {error}");
        }
    }
}

async fn wait_until_closed(
    app_id: &AppId,
    apps: &mut Vec<AppView>,
) -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..10 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let latest = list_apps().await?;
        let closed = latest
            .iter()
            .find(|app| app.id == *app_id)
            .is_none_or(|app| app.session.is_none() && !app.background_active);
        *apps = latest;
        if closed {
            return Ok(());
        }
    }
    Err(format!("application {app_id} still reports a live session after close").into())
}

fn button_contains(button: &Button, x: i32, y: i32) -> bool {
    x >= button.x && x < button.x + button.width && y >= button.y && y < button.y + button.height
}

fn button_at(buttons: &[Button], x: i32, y: i32) -> Option<&Button> {
    buttons.iter().find(|button| button_contains(button, x, y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lost_unlock_ack_requires_authoritative_manager_state() {
        assert!(unlock_committed_after_lost_ack(Some(&DomainState::Manager)));
        assert!(!unlock_committed_after_lost_ack(Some(
            &DomainState::Sleeping
        )));
        assert!(!unlock_committed_after_lost_ack(Some(&DomainState::System)));
        assert!(!unlock_committed_after_lost_ack(None));
    }

    #[test]
    fn failed_suspend_keeps_a_committed_sleeping_domain_locked() {
        assert!(retain_lock_after_failed_sleep(Some(&DomainState::Sleeping)));
        assert!(!retain_lock_after_failed_sleep(Some(&DomainState::Manager)));
        assert!(!retain_lock_after_failed_sleep(None));
    }
}
