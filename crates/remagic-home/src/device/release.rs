use super::{
    button_contains, domain_state, execute_action, first_run, reconcile_mode, refresh_apps,
    request, retain_lock_after_failed_sleep, settings_ui, store, unlock, Action, Button, Display,
    HomeSettings, UiMode, WallpaperOption,
};
use ab_glyph::FontArc;
use remagic_protocol::{AppView, Request};
use std::time::Duration;

pub(super) struct Context<'a> {
    pub(super) display: &'a mut Display,
    pub(super) font: &'a FontArc,
    pub(super) apps: &'a mut Vec<AppView>,
    pub(super) buttons: &'a mut Vec<Button>,
    pub(super) mode: &'a mut UiMode,
    pub(super) settings: &'a mut HomeSettings,
    pub(super) wallpapers: &'a mut Vec<WallpaperOption>,
    pub(super) store_error: &'a mut Option<String>,
}

pub(super) async fn handle(
    mut context: Context<'_>,
    pressed: &mut Option<(Button, Vec<u32>)>,
    x: i32,
    y: i32,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some((button, saved)) = pressed.take() else {
        return Ok(());
    };
    context.display.release(&button, saved)?;
    if !button_contains(&button, x, y) {
        return Ok(());
    }
    if handle_welcome(&mut context, &button.action).await?
        || handle_store(&mut context, &button.action).await?
        || settings_ui::handle(
            &button.action,
            context.display,
            context.font,
            context.apps,
            context.buttons,
            context.mode,
            context.settings,
            context.wallpapers,
        )
        .await?
    {
        return Ok(());
    }
    handle_manager(&mut context, button.action).await
}

async fn handle_welcome(
    context: &mut Context<'_>,
    action: &Action,
) -> Result<bool, Box<dyn std::error::Error>> {
    if *context.mode != UiMode::Welcome {
        return Ok(false);
    }
    match action {
        Action::OpenStore => {
            first_run::complete()?;
            refresh_apps(context.apps).await;
            *context.buttons = context.display.render_store(
                context.font,
                context.apps,
                None,
                context.store_error.as_deref(),
            )?;
            *context.mode = UiMode::Store;
        }
        Action::System => {
            first_run::complete()?;
            if let Err(error) = request(Request::ReturnSystem).await {
                eprintln!("remagic-home: first-run return failed: {error}");
                *context.buttons = context.display.render(context.font, context.apps)?;
                *context.mode = UiMode::Manager;
            }
        }
        _ => return Ok(false),
    }
    Ok(true)
}

async fn handle_store(
    context: &mut Context<'_>,
    action: &Action,
) -> Result<bool, Box<dyn std::error::Error>> {
    match (*context.mode, action) {
        (UiMode::Manager, Action::OpenStore) => enter_store(context).await?,
        (UiMode::Store, Action::BackManager) => leave_store(context).await?,
        (UiMode::Store, Action::StoreInstall(app_id)) => {
            install_from_store(context, app_id).await?
        }
        (UiMode::Store, Action::Launch(app_id)) => launch_from_store(context, app_id).await?,
        _ => return Ok(false),
    }
    Ok(true)
}

async fn enter_store(context: &mut Context<'_>) -> Result<(), Box<dyn std::error::Error>> {
    refresh_apps(context.apps).await;
    *context.buttons = context.display.render_store(
        context.font,
        context.apps,
        None,
        context.store_error.as_deref(),
    )?;
    *context.mode = UiMode::Store;
    Ok(())
}

async fn leave_store(context: &mut Context<'_>) -> Result<(), Box<dyn std::error::Error>> {
    refresh_apps(context.apps).await;
    *context.buttons = context.display.render(context.font, context.apps)?;
    *context.mode = UiMode::Manager;
    Ok(())
}

async fn install_from_store(
    context: &mut Context<'_>,
    app_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    *context.store_error = None;
    *context.buttons =
        context
            .display
            .render_store(context.font, context.apps, Some(app_id), None)?;
    if let Err(error) = store::install(app_id).await {
        eprintln!("remagic-home: Store install failed: {error}");
        *context.store_error = Some(error.to_string());
    }
    redraw_store(context).await
}

async fn launch_from_store(
    context: &mut Context<'_>,
    app_id: &remagic_core::AppId,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Err(error) = request(Request::Launch {
        app_id: app_id.clone(),
        open_path: None,
    })
    .await
    {
        eprintln!("remagic-home: Store launch failed: {error}");
        *context.store_error = Some(error.to_string());
    }
    redraw_store(context).await
}

async fn redraw_store(context: &mut Context<'_>) -> Result<(), Box<dyn std::error::Error>> {
    refresh_apps(context.apps).await;
    *context.buttons = context.display.render_store(
        context.font,
        context.apps,
        None,
        context.store_error.as_deref(),
    )?;
    Ok(())
}

async fn handle_manager(
    context: &mut Context<'_>,
    action: Action,
) -> Result<(), Box<dyn std::error::Error>> {
    match (*context.mode, action) {
        (UiMode::Manager, Action::Sleep) => sleep(context).await?,
        (UiMode::Locked, Action::Wake) => {
            unlock(
                context.display,
                context.font,
                context.apps,
                context.buttons,
                context.mode,
                context.settings,
                context.wallpapers,
            )
            .await?;
        }
        (UiMode::Manager, action) => execute_manager_action(context, action).await?,
        _ => {}
    }
    Ok(())
}

async fn sleep(context: &mut Context<'_>) -> Result<(), Box<dyn std::error::Error>> {
    // Publish authoritative lock pixels before asking remagicd to fence the
    // surface and release the wakelock.
    *context.buttons =
        context
            .display
            .render_locked(context.font, context.settings, context.wallpapers, false)?;
    *context.mode = UiMode::Locked;
    let result = request(Request::Sleep {
        lock_surface_sequence: context.display.commit_sequence(),
    })
    .await;
    if result.is_ok() {
        // Sleep returns after resume; the power key is the unlock action.
        return unlock(
            context.display,
            context.font,
            context.apps,
            context.buttons,
            context.mode,
            context.settings,
            context.wallpapers,
        )
        .await;
    }
    let error = result.expect_err("sleep result was checked above");
    eprintln!("remagic-home: sleep failed: {error}");
    let domain = domain_state().await.ok();
    if !retain_lock_after_failed_sleep(domain.as_ref()) {
        reconcile_mode(
            context.display,
            context.font,
            context.apps,
            context.buttons,
            context.mode,
            context.settings,
            context.wallpapers,
        )
        .await?;
    }
    Ok(())
}

async fn execute_manager_action(
    context: &mut Context<'_>,
    action: Action,
) -> Result<(), Box<dyn std::error::Error>> {
    let is_close = matches!(action, Action::Close(_));
    if let Err(error) = execute_action(action, context.apps).await {
        // Keep Home alive so the daemon can roll back a failed launch.
        eprintln!("remagic-home: action failed: {error}");
    }
    if !is_close {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    // Always reload after an acknowledged close or daemon recovery so the
    // close button cannot retain a stale pre-request snapshot.
    refresh_apps(context.apps).await;
    *context.buttons = context.display.render(context.font, context.apps)?;
    Ok(())
}
