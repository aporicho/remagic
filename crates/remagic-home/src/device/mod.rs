mod display;
mod settings;
mod settings_ui;

use crate::{domain_state, list_apps, request};
use display::Display;
use remagic_core::{AppId, DomainState};
use remagic_protocol::{AppView, PackageOperation, Request};
use settings::{HomeSettings, WallpaperOption};
use std::fs;
use std::time::Duration;

#[derive(Clone)]
pub(super) enum Action {
    Launch(AppId),
    Close(AppId),
    Package(PackageOperation),
    Unavailable,
    System,
    Sleep,
    Wake,
    OpenSettings,
    BackManager,
    BackSettings,
    CycleWallpaper,
    ToggleWallpaperFit,
    ToggleLockClock,
    ToggleLockHint,
    PreviewLock,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UiMode {
    Manager,
    Settings,
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
    let font = display::load_font()?;
    let mut mode = if matches!(domain_state().await?, DomainState::Sleeping) {
        UiMode::Locked
    } else {
        UiMode::Manager
    };
    let mut buttons = match mode {
        UiMode::Manager => display.render(&font, &apps)?,
        UiMode::Locked => display.render_locked(&font, &settings, &wallpapers, false)?,
        UiMode::Settings | UiMode::LockPreview => unreachable!(),
    };
    let mut pressed: Option<(Button, Vec<u32>)> = None;
    let mut last_lock_poll = tokio::time::Instant::now();
    loop {
        for event in display.poll_touch_events()? {
            match event {
                crate::qtfb::TouchEvent::Press { x, y } => {
                    handle_press(&mut display, &buttons, &mut pressed, x, y)?;
                }
                crate::qtfb::TouchEvent::Release { x, y } => {
                    handle_release(
                        &mut display,
                        &font,
                        &mut apps,
                        &mut buttons,
                        &mut pressed,
                        &mut mode,
                        &mut settings,
                        &mut wallpapers,
                        x,
                        y,
                    )
                    .await?;
                }
            }
        }
        if mode == UiMode::Locked && last_lock_poll.elapsed() >= Duration::from_millis(300) {
            let previous_mode = mode;
            reconcile_mode(
                &mut display,
                &font,
                &mut apps,
                &mut buttons,
                &mut mode,
                &settings,
                &wallpapers,
            )
            .await?;
            if mode != previous_mode {
                pressed = None;
            }
            last_lock_poll = tokio::time::Instant::now();
        }
        let wait = if mode == UiMode::Locked {
            Duration::from_millis(300).saturating_sub(last_lock_poll.elapsed())
        } else {
            Duration::from_millis(500)
        };
        display.wait_for_input(wait.max(Duration::from_millis(1)))?;
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_release(
    display: &mut Display,
    font: &ab_glyph::FontArc,
    apps: &mut Vec<AppView>,
    buttons: &mut Vec<Button>,
    pressed: &mut Option<(Button, Vec<u32>)>,
    mode: &mut UiMode,
    settings: &mut HomeSettings,
    wallpapers: &mut Vec<WallpaperOption>,
    x: i32,
    y: i32,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some((button, saved)) = pressed.take() else {
        return Ok(());
    };
    display.release(&button, saved)?;
    if !button_contains(&button, x, y) {
        return Ok(());
    }
    if settings_ui::handle(
        &button.action,
        display,
        font,
        apps,
        buttons,
        mode,
        settings,
        wallpapers,
    )
    .await?
    {
        return Ok(());
    }
    match (*mode, button.action.clone()) {
        (UiMode::Manager, Action::Sleep) => {
            // Publish authoritative lock pixels before asking remagicd to
            // fence the surface and release the wakelock.
            *buttons = display.render_locked(font, settings, wallpapers, false)?;
            *mode = UiMode::Locked;
            let sleep_result = request(Request::Sleep {
                lock_surface_sequence: display.commit_sequence(),
            })
            .await;
            match sleep_result {
                Ok(()) => {
                    // The sleep request returns only after the kernel has
                    // resumed. Replace the frozen lock with a prepared Home
                    // frame immediately; the power key is the unlock action.
                    unlock(display, font, apps, buttons, mode, settings, wallpapers).await?;
                }
                Err(error) => {
                    eprintln!("remagic-home: sleep failed: {error}");
                    let domain = domain_state().await.ok();
                    if !retain_lock_after_failed_sleep(domain.as_ref()) {
                        reconcile_mode(display, font, apps, buttons, mode, settings, wallpapers)
                            .await?;
                    }
                }
            }
        }
        (UiMode::Locked, Action::Wake) => {
            unlock(display, font, apps, buttons, mode, settings, wallpapers).await?;
        }
        (UiMode::Manager, action) => {
            let is_close = matches!(action, Action::Close(_));
            if let Err(error) = execute_action(action, apps).await {
                // Keep Home alive so the daemon can roll back a failed launch.
                eprintln!("remagic-home: action failed: {error}");
            }
            if !is_close {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            // Close normally refreshes while waiting for the exact app to
            // disappear.  Still reload here: a failed close is acknowledged
            // only after daemon recovery, and retaining the pre-request
            // snapshot made a stale Close button look as if it did nothing.
            refresh_apps(apps).await;
            *buttons = display.render(font, apps)?;
        }
        (UiMode::Locked | UiMode::Settings | UiMode::LockPreview, _) => {}
    }
    Ok(())
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
        Action::Package(operation) => request(Request::Package { operation }).await,
        Action::Unavailable => Ok(()),
        Action::System => request(Request::ReturnSystem).await,
        Action::Sleep => Err("sleep must render and fence the lock page first".into()),
        Action::Wake => Err("wake must prepare and fence the manager page first".into()),
        Action::OpenSettings
        | Action::BackManager
        | Action::BackSettings
        | Action::CycleWallpaper
        | Action::ToggleWallpaperFit
        | Action::ToggleLockClock
        | Action::ToggleLockHint
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

pub(super) fn queued_magicpaper_result() -> bool {
    fs::metadata("/home/root/riddle-data/agent/pending.tsv")
        .is_ok_and(|metadata| metadata.len() > 0)
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
