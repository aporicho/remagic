mod display;

use crate::{list_apps, request};
use display::Display;
use remagic_core::AppId;
use remagic_protocol::{AppView, PackageOperation, Request};
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
    let mut display = Display::open()?;
    let font = display::load_font()?;
    let mut buttons = display.render(&font, &apps)?;
    let mut pressed: Option<(Button, Vec<u32>)> = None;
    loop {
        for event in display.poll_touch_events()? {
            match event {
                crate::qtfb::TouchEvent::Press { x, y } => {
                    handle_press(&mut display, &buttons, &mut pressed, x, y)?;
                }
                crate::qtfb::TouchEvent::Release { x, y } => {
                    let Some((button, saved)) = pressed.take() else {
                        continue;
                    };
                    display.release(&button, saved)?;
                    if !button_contains(&button, x, y) {
                        continue;
                    }
                    let is_close = matches!(button.action, Action::Close(_));
                    if let Err(error) = execute_action(button.action, &mut apps).await {
                        // A failed application launch is an ordinary Home event, not
                        // a fatal error for the task manager itself.  remagicd has
                        // already rolled the display domain back to this still-live
                        // surface; keep it registered and repaint instead of exiting
                        // and briefly exposing an empty foreground surface.
                        eprintln!("remagic-home: action failed: {error}");
                    }
                    if !is_close {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                        match list_apps().await {
                            Ok(latest) => apps = latest,
                            Err(error) => {
                                // The daemon may be completing a rollback.  Preserve
                                // the last coherent snapshot and keep Home alive so a
                                // transient control error cannot blank the display.
                                eprintln!("remagic-home: app refresh failed: {error}");
                            }
                        }
                    }
                    buttons = display.render(&font, &apps)?;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(12)).await;
    }
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
        Action::Sleep => request(Request::Sleep).await,
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
            break;
        }
    }
    Ok(())
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
