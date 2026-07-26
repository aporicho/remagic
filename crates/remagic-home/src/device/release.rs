use super::{
    button_contains, domain_state, execute_action, first_run, reconcile_mode, refresh_apps,
    request, retain_lock_after_failed_sleep, settings_ui, store, unlock, Action, Button, Display,
    HomeSettings, ResumeTarget, UiMode, WallpaperOption,
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
    pub(super) store_catalog: &'a mut Vec<super::store::CatalogApp>,
    pub(super) store_progress: &'a mut Option<super::store::OperationProgress>,
    pub(super) system_update_error: &'a mut Option<String>,
    pub(super) system_update: &'a mut super::store::SystemUpdateInfo,
    pub(super) system_update_progress: &'a mut Option<super::store::OperationProgress>,
    pub(super) wallpaper_page: &'a mut usize,
    pub(super) task_tx: &'a tokio::sync::mpsc::UnboundedSender<super::store::TaskResult>,
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
        || handle_system_update(&mut context, &button.action).await?
        || settings_ui::handle(
            &button.action,
            context.display,
            context.font,
            context.apps,
            context.buttons,
            context.mode,
            context.settings,
            context.wallpapers,
            context.wallpaper_page,
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
            if let Ok(catalog) = super::store::catalog().await {
                *context.store_catalog = catalog;
            }
            *context.buttons = context.display.render_store(
                context.font,
                context.apps,
                context.store_catalog,
                context.store_progress.as_ref(),
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
        (UiMode::Store, Action::StoreUpgrade(app_id)) => {
            upgrade_from_store(context, app_id).await?
        }
        (UiMode::Store, Action::StoreUninstall(app_id)) => {
            uninstall_from_store(context, app_id).await?
        }
        (UiMode::Store, Action::Launch(app_id)) => launch_from_store(context, app_id).await?,
        _ => return Ok(false),
    }
    Ok(true)
}

async fn enter_store(context: &mut Context<'_>) -> Result<(), Box<dyn std::error::Error>> {
    refresh_apps(context.apps).await;
    match super::store::catalog().await {
        Ok(catalog) => *context.store_catalog = catalog,
        Err(error) => {
            eprintln!("remagic-home: Store catalog refresh failed: {error}");
            *context.store_error = Some(error.to_string());
        }
    }
    *context.buttons = context.display.render_store(
        context.font,
        context.apps,
        context.store_catalog,
        context.store_progress.as_ref(),
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
    if context.store_progress.is_some() {
        return Ok(());
    }
    *context.store_error = None;
    *context.store_progress = Some(store::OperationProgress::indeterminate(
        app_id,
        "正在下载、验证并安装",
    ));
    *context.buttons = context.display.render_store(
        context.font,
        context.apps,
        context.store_catalog,
        context.store_progress.as_ref(),
        None,
    )?;
    let app_id = app_id.to_owned();
    let task_tx = context.task_tx.clone();
    tokio::spawn(async move {
        let result = store::install(&app_id)
            .await
            .map_err(|error| error.to_string());
        let _ = task_tx.send(store::TaskResult::Store { app_id, result });
    });
    Ok(())
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

async fn upgrade_from_store(
    context: &mut Context<'_>,
    app_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if context.store_progress.is_some() {
        return Ok(());
    }
    *context.store_error = None;
    *context.store_progress = Some(store::OperationProgress::indeterminate(
        app_id,
        "正在下载、验证并安装更新",
    ));
    *context.buttons = context.display.render_store(
        context.font,
        context.apps,
        context.store_catalog,
        context.store_progress.as_ref(),
        None,
    )?;
    let app_id = app_id.to_owned();
    let task_tx = context.task_tx.clone();
    tokio::spawn(async move {
        let result = store::upgrade(&app_id)
            .await
            .map_err(|error| error.to_string());
        let _ = task_tx.send(store::TaskResult::Store { app_id, result });
    });
    Ok(())
}

async fn uninstall_from_store(
    context: &mut Context<'_>,
    app_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if context.store_progress.is_some() {
        return Ok(());
    }
    *context.store_error = None;
    *context.store_progress = Some(store::OperationProgress::indeterminate(app_id, "正在卸载"));
    *context.buttons = context.display.render_store(
        context.font,
        context.apps,
        context.store_catalog,
        context.store_progress.as_ref(),
        None,
    )?;
    let app_id = app_id.to_owned();
    let task_tx = context.task_tx.clone();
    tokio::spawn(async move {
        let result = store::uninstall(&app_id)
            .await
            .map_err(|error| error.to_string());
        let _ = task_tx.send(store::TaskResult::Store { app_id, result });
    });
    Ok(())
}

async fn redraw_store(context: &mut Context<'_>) -> Result<(), Box<dyn std::error::Error>> {
    refresh_apps(context.apps).await;
    *context.buttons = context.display.render_store(
        context.font,
        context.apps,
        context.store_catalog,
        context.store_progress.as_ref(),
        context.store_error.as_deref(),
    )?;
    Ok(())
}

async fn handle_system_update(
    context: &mut Context<'_>,
    action: &Action,
) -> Result<bool, Box<dyn std::error::Error>> {
    match (*context.mode, action) {
        (UiMode::Settings, Action::OpenSystemUpdate) => enter_system_update(context).await?,
        (UiMode::SystemUpdate, Action::BackSettings) => {
            *context.buttons = context.display.render_settings(
                context.font,
                context.settings,
                context.wallpapers,
            )?;
            *context.mode = UiMode::Settings;
        }
        (UiMode::SystemUpdate, Action::RefreshSystemUpdate) => {
            refresh_system_update_page(context).await?
        }
        (UiMode::SystemUpdate, Action::SystemUpdate) => install_system_update(context).await?,
        _ => return Ok(false),
    }
    Ok(true)
}

async fn enter_system_update(context: &mut Context<'_>) -> Result<(), Box<dyn std::error::Error>> {
    *context.system_update_error = None;
    refresh_system_update_page(context).await?;
    *context.mode = UiMode::SystemUpdate;
    Ok(())
}

async fn refresh_system_update_page(
    context: &mut Context<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    *context.system_update_error = None;
    *context.system_update_progress = Some(store::OperationProgress::indeterminate(
        "__system__",
        "正在检查更新",
    ));
    redraw_system_update(context)?;
    match store::system_update_info().await {
        Ok(update) => *context.system_update = update,
        Err(error) => {
            eprintln!("remagic-home: system update check failed: {error}");
            *context.system_update_error = Some(error.to_string());
        }
    }
    *context.system_update_progress = None;
    redraw_system_update(context)
}

async fn install_system_update(
    context: &mut Context<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    if context.system_update_progress.is_some() {
        return Ok(());
    }
    *context.system_update_error = None;
    *context.system_update_progress = Some(store::OperationProgress::indeterminate(
        "__system__",
        "正在下载、验证并安装系统更新",
    ));
    redraw_system_update(context)?;
    let task_tx = context.task_tx.clone();
    tokio::spawn(async move {
        let result = store::install_system_update()
            .await
            .map_err(|error| error.to_string());
        let _ = task_tx.send(store::TaskResult::SystemInstall { result });
    });
    Ok(())
}

fn redraw_system_update(context: &mut Context<'_>) -> Result<(), Box<dyn std::error::Error>> {
    *context.buttons = context.display.render_system_update(
        context.font,
        context.system_update,
        context.system_update_progress.as_ref(),
        context.system_update_error.as_deref(),
    )?;
    Ok(())
}

async fn handle_manager(
    context: &mut Context<'_>,
    action: Action,
) -> Result<(), Box<dyn std::error::Error>> {
    match (*context.mode, action) {
        (UiMode::Manager, Action::Sleep) => sleep(context).await?,
        (UiMode::Manager, action) => execute_manager_action(context, action).await?,
        _ => {}
    }
    Ok(())
}

pub(super) async fn sleep(context: &mut Context<'_>) -> Result<(), Box<dyn std::error::Error>> {
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

pub(super) async fn cover_sleep(
    context: &mut Context<'_>,
    resume_target: &mut Option<ResumeTarget>,
    target: ResumeTarget,
    deferred_cover_sleep: &mut bool,
) -> Result<(), Box<dyn std::error::Error>> {
    *resume_target = Some(target);
    if *context.mode != UiMode::Locked {
        *context.buttons = context.display.render_locked(
            context.font,
            context.settings,
            context.wallpapers,
            false,
        )?;
        *context.mode = UiMode::Locked;
    }
    if maintenance_active(context) {
        *deferred_cover_sleep = true;
        return Ok(());
    }
    *deferred_cover_sleep = false;
    request_cover_sleep(context, resume_target).await
}

async fn request_cover_sleep(
    context: &mut Context<'_>,
    resume_target: &mut Option<ResumeTarget>,
) -> Result<(), Box<dyn std::error::Error>> {
    let result = request(Request::Sleep {
        lock_surface_sequence: context.display.commit_sequence(),
    })
    .await;
    if result.is_ok() {
        return resume_unlock(context, resume_target.take()).await;
    }
    let error = result.expect_err("sleep result was checked above");
    eprintln!("remagic-home: cover sleep failed: {error}");
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

pub(super) async fn handle_task_result(
    context: &mut Context<'_>,
    result: store::TaskResult,
    resume_target: &mut Option<ResumeTarget>,
    deferred_cover_sleep: &mut bool,
) -> Result<(), Box<dyn std::error::Error>> {
    match result {
        store::TaskResult::Store { app_id, result } => {
            *context.store_progress = None;
            if let Err(error) = result {
                eprintln!("remagic-home: Store operation failed for {app_id}: {error}");
                *context.store_error = Some(error);
            }
            if *context.mode == UiMode::Store {
                match store::catalog().await {
                    Ok(catalog) => *context.store_catalog = catalog,
                    Err(error) => {
                        eprintln!("remagic-home: Store catalog refresh failed: {error}");
                        *context.store_error = Some(error.to_string());
                    }
                }
                redraw_store(context).await?;
            } else {
                refresh_apps(context.apps).await;
            }
        }
        store::TaskResult::SystemInstall { result } => {
            match result {
                Ok(()) => {
                    *context.system_update_error = None;
                    *context.system_update_progress = Some(store::OperationProgress::complete(
                        "__system__",
                        "系统更新已开始，请保持设备连接",
                    ));
                }
                Err(error) => {
                    eprintln!("remagic-home: system update failed: {error}");
                    *context.system_update_error = Some(error);
                    *context.system_update_progress = None;
                }
            }
            if *context.mode == UiMode::SystemUpdate {
                redraw_system_update(context)?;
            }
        }
    }

    if *deferred_cover_sleep && !maintenance_active(context) && *context.mode == UiMode::Locked {
        *deferred_cover_sleep = false;
        request_cover_sleep(context, resume_target).await?;
    }
    Ok(())
}

pub(super) async fn launch_resume_app(
    context: &mut Context<'_>,
    app_id: remagic_core::AppId,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Err(error) = request(Request::Launch {
        app_id,
        open_path: None,
    })
    .await
    {
        eprintln!("remagic-home: cover resume launch failed: {error}");
        refresh_apps(context.apps).await;
        *context.buttons = context.display.render(context.font, context.apps)?;
        *context.mode = UiMode::Manager;
    }
    Ok(())
}

fn maintenance_active(context: &Context<'_>) -> bool {
    context.store_progress.is_some() || context.system_update_progress.is_some()
}

pub(super) async fn resume_unlock(
    context: &mut Context<'_>,
    target: Option<ResumeTarget>,
) -> Result<(), Box<dyn std::error::Error>> {
    let target = target.unwrap_or(ResumeTarget::Ui(UiMode::Manager));
    let launch_after = render_resume_target(context, &target).await?;
    let manager_surface_sequence = context.display.commit_sequence();
    match request(Request::Wake {
        manager_surface_sequence,
    })
    .await
    {
        Ok(()) => {
            if let Some(app_id) = launch_after {
                if let Err(error) = request(Request::Launch {
                    app_id,
                    open_path: None,
                })
                .await
                {
                    eprintln!("remagic-home: cover resume launch failed: {error}");
                    refresh_apps(context.apps).await;
                    *context.buttons = context.display.render(context.font, context.apps)?;
                    *context.mode = UiMode::Manager;
                }
            }
        }
        Err(error) => {
            eprintln!("remagic-home: cover unlock acknowledgement failed: {error}");
            let authoritative_domain = domain_state().await.ok();
            if !super::unlock_committed_after_lost_ack(authoritative_domain.as_ref()) {
                *context.buttons = context.display.render_locked(
                    context.font,
                    context.settings,
                    context.wallpapers,
                    false,
                )?;
                *context.mode = UiMode::Locked;
            }
        }
    }
    Ok(())
}

async fn render_resume_target(
    context: &mut Context<'_>,
    target: &ResumeTarget,
) -> Result<Option<remagic_core::AppId>, Box<dyn std::error::Error>> {
    match target {
        ResumeTarget::App(app_id) => {
            refresh_apps(context.apps).await;
            *context.buttons = context.display.render(context.font, context.apps)?;
            *context.mode = UiMode::Manager;
            Ok(Some(app_id.clone()))
        }
        ResumeTarget::Ui(UiMode::Welcome) => {
            let device = remagic_device::DeviceProfile::detect()?;
            let name = match device.product {
                remagic_device::DeviceProduct::PaperPro => "reMarkable Paper Pro",
                remagic_device::DeviceProduct::PaperProMove => "reMarkable Paper Pro Move",
            };
            *context.buttons = context.display.render_welcome(context.font, name)?;
            *context.mode = UiMode::Welcome;
            Ok(None)
        }
        ResumeTarget::Ui(UiMode::Manager) | ResumeTarget::Ui(UiMode::Locked) => {
            refresh_apps(context.apps).await;
            *context.buttons = context.display.render(context.font, context.apps)?;
            *context.mode = UiMode::Manager;
            Ok(None)
        }
        ResumeTarget::Ui(UiMode::Settings) | ResumeTarget::Ui(UiMode::LockPreview) => {
            *context.buttons = context.display.render_settings(
                context.font,
                context.settings,
                context.wallpapers,
            )?;
            *context.mode = UiMode::Settings;
            Ok(None)
        }
        ResumeTarget::Ui(UiMode::Backlight) => {
            settings_ui::refresh_backlight_settings(context.settings).await;
            *context.buttons = context
                .display
                .render_backlight_settings(context.font, context.settings.backlight.as_ref())?;
            *context.mode = UiMode::Backlight;
            Ok(None)
        }
        ResumeTarget::Ui(UiMode::WallpaperBrowser) => {
            let (buttons, page) = context.display.render_wallpaper_browser(
                context.font,
                context.settings,
                context.wallpapers,
                *context.wallpaper_page,
            )?;
            *context.buttons = buttons;
            *context.wallpaper_page = page;
            *context.mode = UiMode::WallpaperBrowser;
            Ok(None)
        }
        ResumeTarget::Ui(UiMode::Store) => {
            refresh_apps(context.apps).await;
            match store::catalog().await {
                Ok(catalog) => *context.store_catalog = catalog,
                Err(error) => {
                    eprintln!("remagic-home: Store catalog refresh failed: {error}");
                    *context.store_error = Some(error.to_string());
                }
            }
            *context.buttons = context.display.render_store(
                context.font,
                context.apps,
                context.store_catalog,
                context.store_progress.as_ref(),
                context.store_error.as_deref(),
            )?;
            *context.mode = UiMode::Store;
            Ok(None)
        }
        ResumeTarget::Ui(UiMode::SystemUpdate) => {
            *context.buttons = context.display.render_system_update(
                context.font,
                context.system_update,
                context.system_update_progress.as_ref(),
                context.system_update_error.as_deref(),
            )?;
            *context.mode = UiMode::SystemUpdate;
            Ok(None)
        }
    }
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
