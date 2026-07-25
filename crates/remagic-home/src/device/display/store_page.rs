use super::{Display, BLACK, DARK_GRAY, GRAY, WHITE};
use crate::device::{
    store::{CatalogApp, CatalogStatus, OperationProgress, SystemUpdateInfo},
    Action, Button,
};
use ab_glyph::FontArc;
use remagic_core::AppId;
use remagic_protocol::AppView;
use std::io;

const STORE_APPS: [(&str, &str, &str); 3] = [
    ("magicpaper", "MagicPaper", "手写 AI 魔法纸"),
    ("koreader", "KOReader", "阅读与管理电子书"),
    ("upload", "文件传输", "上传书籍、壁纸并同步设备"),
];
const STORE_TITLE: &str = "应用商店";
const STORE_SUBTITLE: &str = "ReMagic 第一方应用";
const SYSTEM_UPDATE_TITLE: &str = "系统更新";

impl Display {
    pub(crate) fn render_store(
        &mut self,
        font: &FontArc,
        apps: &[AppView],
        catalog: &[CatalogApp],
        progress: Option<&OperationProgress>,
        error: Option<&str>,
    ) -> io::Result<Vec<Button>> {
        self.use_mono_ui()?;
        self.fill(WHITE);
        let mut buttons = Vec::new();
        let y = self.render_store_header(font, &mut buttons, error.is_some());
        let entries = store_entries(apps, catalog);
        self.render_store_grid(font, &mut buttons, y, &entries, progress);
        self.client.update_all()?;
        Ok(buttons)
    }

    pub(crate) fn render_system_update(
        &mut self,
        font: &FontArc,
        system_update: &SystemUpdateInfo,
        progress: Option<&OperationProgress>,
        error: Option<&str>,
    ) -> io::Result<Vec<Button>> {
        self.use_mono_ui()?;
        self.fill(WHITE);
        let mut buttons = Vec::new();
        self.text(font, SYSTEM_UPDATE_TITLE, 54.0, 48, 82, BLACK);
        self.text(font, "ReMagic 系统版本与安装", 25.0, 50, 128, DARK_GRAY);
        self.small_store_button(
            font,
            "返回",
            Button {
                x: self.width - 194,
                y: 38,
                width: 152,
                height: 72,
                action: Action::BackSettings,
            },
            &mut buttons,
        );
        self.render_system_update_card(font, &mut buttons, 202, system_update, progress);
        if let Some(error) = error {
            self.text(font, "上次操作失败", 27.0, 52, 420, BLACK);
            self.text(
                font,
                &super::truncate_to_width(font, error, 22.0, self.width - 104),
                22.0,
                52,
                458,
                DARK_GRAY,
            );
        }
        self.small_store_button(
            font,
            "重新检查",
            Button {
                x: 38,
                y: self.height - 150,
                width: self.width - 76,
                height: 104,
                action: if progress.is_some() {
                    Action::Unavailable
                } else {
                    Action::RefreshSystemUpdate
                },
            },
            &mut buttons,
        );
        self.client.update_all()?;
        Ok(buttons)
    }

    fn render_store_header(
        &mut self,
        font: &FontArc,
        buttons: &mut Vec<Button>,
        has_error: bool,
    ) -> i32 {
        self.text(font, STORE_TITLE, 54.0, 48, 82, BLACK);
        self.text(font, STORE_SUBTITLE, 25.0, 50, 128, DARK_GRAY);
        self.small_store_button(
            font,
            "返回",
            Button {
                x: self.width - 190,
                y: 38,
                width: 142,
                height: 72,
                action: Action::BackManager,
            },
            buttons,
        );

        let mut y = 188;
        if has_error {
            self.text(font, "上次操作失败，请重试", 24.0, 50, 166, DARK_GRAY);
            y += 28;
        }
        y
    }

    fn render_store_grid(
        &mut self,
        font: &FontArc,
        buttons: &mut Vec<Button>,
        y: i32,
        entries: &[CatalogApp],
        progress: Option<&OperationProgress>,
    ) -> i32 {
        let tile = 260;
        let gap = 26;
        let columns = ((self.width - 76 + gap) / (tile + gap)).max(1);
        let grid_width = columns * tile + (columns - 1) * gap;
        let start_x = (self.width - grid_width) / 2;
        for (index, entry) in entries.iter().enumerate() {
            let column = index as i32 % columns;
            let row = index as i32 / columns;
            let x = start_x + column * (tile + gap);
            let tile_y = y + row * (tile + gap);
            if tile_y + tile > self.height - 52 {
                break;
            }
            self.render_store_app_tile(font, buttons, x, tile_y, tile, entry, progress);
        }
        y + ((entries.len() as i32 + columns - 1) / columns) * (tile + gap)
    }

    fn render_system_update_card(
        &mut self,
        font: &FontArc,
        buttons: &mut Vec<Button>,
        y: i32,
        system_update: &SystemUpdateInfo,
        progress: Option<&OperationProgress>,
    ) -> i32 {
        let system_busy = progress.is_some();
        self.card(38, y, self.width - 76, 176);
        self.text(font, "ReMagic 系统", 38.0, 72, y + 52, BLACK);
        let system_status = if system_busy {
            progress.expect("checked above").label.clone()
        } else if system_update.update_available {
            format!(
                "{} → {} · 轻点更新",
                system_update.current_version, system_update.available_version
            )
        } else if system_update.available_version.is_empty() {
            "轻点联网检查系统更新".to_owned()
        } else {
            format!("已是最新版本 {}", system_update.current_version)
        };
        self.text(font, &system_status, 23.0, 72, y + 103, DARK_GRAY);
        if let Some(progress) = progress {
            self.progress_bar(72, y + 128, self.width - 144, 18, progress.fraction);
        }
        buttons.push(Button {
            x: 38,
            y,
            width: self.width - 76,
            height: 176,
            action: if system_busy {
                Action::Unavailable
            } else if system_update.update_available {
                Action::SystemUpdate
            } else if system_update.available_version.is_empty() {
                Action::RefreshSystemUpdate
            } else {
                Action::Unavailable
            },
        });
        y + 200
    }

    fn render_store_app_tile(
        &mut self,
        font: &FontArc,
        buttons: &mut Vec<Button>,
        x: i32,
        y: i32,
        size: i32,
        entry: &CatalogApp,
        progress: Option<&OperationProgress>,
    ) {
        let is_busy = progress.is_some_and(|value| value.target_id == entry.id);
        let installed = is_installed(entry.status) && !is_busy;
        self.card(x, y, size, size);
        let icon = 102;
        let icon_x = x + (size - icon) / 2;
        self.rect(icon_x, y + 24, icon, icon, BLACK);
        self.centered_text_in_rect(
            font,
            &store_icon_text(&entry.id),
            45.0,
            icon_x,
            icon,
            y + 88,
            WHITE,
        );
        self.centered_text_in_rect(
            font,
            &super::truncate_to_width(font, &entry.name, 28.0, size - 28),
            28.0,
            x,
            size,
            y + 160,
            BLACK,
        );
        self.centered_text_in_rect(
            font,
            &super::truncate_to_width(
                font,
                &format!("{} · v{}", entry.summary, entry.version),
                18.0,
                size - 28,
            ),
            18.0,
            x,
            size,
            y + 192,
            DARK_GRAY,
        );
        let (status, action) = store_action(entry, is_busy);
        self.centered_text_in_rect(
            font,
            &super::truncate_to_width(font, status, 20.0, size - 28),
            20.0,
            x,
            size,
            y + 224,
            BLACK,
        );
        if let Some(progress) = progress.filter(|value| value.target_id == entry.id) {
            self.progress_bar(x + 24, y + 234, size - 48, 14, progress.fraction);
        }
        buttons.push(Button {
            x,
            y,
            width: size,
            height: if installed { size - 58 } else { size },
            action,
        });
        if installed {
            self.render_uninstall_button(font, buttons, x, y, size, &entry.id);
        }
    }

    fn render_uninstall_button(
        &mut self,
        font: &FontArc,
        buttons: &mut Vec<Button>,
        x: i32,
        y: i32,
        size: i32,
        app_id: &str,
    ) {
        let remove = Button {
            x: x + 24,
            y: y + size - 52,
            width: size - 48,
            height: 36,
            action: Action::StoreUninstall(app_id.to_owned()),
        };
        self.rect(remove.x, remove.y, remove.width, remove.height, GRAY);
        self.centered_text_in_rect(
            font,
            "卸载",
            18.0,
            remove.x,
            remove.width,
            remove.y + 25,
            BLACK,
        );
        buttons.push(remove);
    }

    fn small_store_button(
        &mut self,
        font: &FontArc,
        text: &str,
        button: Button,
        buttons: &mut Vec<Button>,
    ) {
        self.rect(button.x, button.y, button.width, button.height, GRAY);
        let baseline = button.y + button.height / 2 + 11;
        self.centered_text_in_rect(font, text, 27.0, button.x, button.width, baseline, BLACK);
        buttons.push(button);
    }
}

fn store_entries(apps: &[AppView], catalog: &[CatalogApp]) -> Vec<CatalogApp> {
    if !catalog.is_empty() {
        return catalog.to_vec();
    }
    STORE_APPS
        .iter()
        .map(|(id, name, summary)| CatalogApp {
            id: (*id).into(),
            name: (*name).into(),
            summary: (*summary).into(),
            version: "未知".into(),
            status: if apps
                .iter()
                .any(|app| app.id.as_str() == *id && app.installed)
            {
                CatalogStatus::Installed
            } else {
                CatalogStatus::NotInstalled
            },
        })
        .collect()
}

fn is_installed(status: CatalogStatus) -> bool {
    matches!(
        status,
        CatalogStatus::Installed
            | CatalogStatus::NeedsConfiguration
            | CatalogStatus::UpdateAvailable
    )
}

fn store_action(entry: &CatalogApp, busy: bool) -> (&'static str, Action) {
    if busy {
        return ("进行中", Action::Unavailable);
    }
    match entry.status {
        CatalogStatus::UpdateAvailable => (
            "有新版本 · 轻点更新",
            Action::StoreUpgrade(entry.id.clone()),
        ),
        CatalogStatus::Installed | CatalogStatus::NeedsConfiguration => (
            "已安装 · 轻点打开",
            Action::Launch(AppId::new(entry.id.clone()).expect("catalog app id")),
        ),
        CatalogStatus::Incompatible => ("当前设备或系统不兼容", Action::Unavailable),
        CatalogStatus::NotInstalled => {
            ("未安装 · 轻点安装", Action::StoreInstall(entry.id.clone()))
        }
    }
}

fn store_icon_text(app_id: &str) -> String {
    match app_id {
        "magicpaper" => "M".into(),
        "koreader" => "K".into(),
        "upload" | "remagic-upload" => "U".into(),
        value => value
            .chars()
            .find(|ch| ch.is_ascii_alphanumeric())
            .map(|ch| ch.to_ascii_uppercase().to_string())
            .unwrap_or_else(|| "?".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_names_never_leak_internal_package_identity() {
        assert_eq!(STORE_APPS[1].1, "KOReader");
        assert!(!STORE_APPS
            .iter()
            .any(|entry| entry.1.contains("for ReMagic")));
    }

    #[test]
    fn store_copy_is_not_the_system_update_center() {
        assert_eq!(STORE_TITLE, "应用商店");
        assert_eq!(STORE_SUBTITLE, "ReMagic 第一方应用");
        assert!(!STORE_TITLE.contains("更新中心"));
        assert!(!STORE_SUBTITLE.contains("系统更新"));
        assert_eq!(SYSTEM_UPDATE_TITLE, "系统更新");
    }
}
