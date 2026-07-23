use super::{Display, BLACK, DARK_GRAY, GRAY, WHITE};
use crate::device::{
    store::{CatalogApp, CatalogStatus, SystemUpdateInfo},
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

impl Display {
    pub(super) fn render_store_card(
        &mut self,
        font: &FontArc,
        buttons: &mut Vec<Button>,
        y: i32,
    ) -> i32 {
        let height = 112;
        self.card(38, y, self.width - 76, height);
        self.text(font, "更新中心", 35.0, 72, y + 48, BLACK);
        self.text(
            font,
            "系统更新 · 应用安装与更新",
            22.0,
            72,
            y + 86,
            DARK_GRAY,
        );
        buttons.push(Button {
            x: 38,
            y,
            width: self.width - 76,
            height,
            action: Action::OpenStore,
        });
        y + height + 22
    }

    pub(crate) fn render_store(
        &mut self,
        font: &FontArc,
        apps: &[AppView],
        catalog: &[CatalogApp],
        system_update: &SystemUpdateInfo,
        busy: Option<&str>,
        error: Option<&str>,
    ) -> io::Result<Vec<Button>> {
        self.fill(WHITE);
        let mut buttons = Vec::new();
        self.text(font, "更新中心", 54.0, 48, 82, BLACK);
        self.text(
            font,
            "系统更新 · ReMagic 第一方应用",
            25.0,
            50,
            128,
            DARK_GRAY,
        );
        let back = Button {
            x: self.width - 190,
            y: 38,
            width: 142,
            height: 72,
            action: Action::BackManager,
        };
        self.round_rect(back.x, back.y, back.width, back.height, GRAY);
        self.text(font, "返回", 28.0, back.x + 42, 84, BLACK);
        buttons.push(back);

        let mut y = 188;
        if error.is_some() {
            self.text(font, "上次操作失败，请重试", 24.0, 50, 166, DARK_GRAY);
            y += 28;
        }
        let fallback = STORE_APPS
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
            .collect::<Vec<_>>();
        let entries = if catalog.is_empty() {
            &fallback
        } else {
            catalog
        };
        let system_busy = busy == Some("__system__");
        self.card(38, y, self.width - 76, 142);
        self.text(font, "ReMagic 系统", 38.0, 72, y + 52, BLACK);
        let system_status = if system_busy {
            "正在下载、验证并安装…".to_owned()
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
        buttons.push(Button {
            x: 38,
            y,
            width: self.width - 76,
            height: 142,
            action: if system_busy
                || (!system_update.update_available && !system_update.available_version.is_empty())
            {
                Action::Unavailable
            } else {
                Action::SystemUpdate
            },
        });
        y += 166;

        for entry in entries {
            let is_busy = busy == Some(entry.id.as_str());
            let height = 176;
            self.card(38, y, self.width - 76, height);
            self.text(font, &entry.name, 41.0, 72, y + 58, BLACK);
            self.text(
                font,
                &format!("{} · v{}", entry.summary, entry.version),
                22.0,
                72,
                y + 99,
                DARK_GRAY,
            );
            let (status, action) = if is_busy {
                ("正在下载、验证并安装…", Action::Unavailable)
            } else {
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
            };
            self.text(font, status, 24.0, 72, y + 142, BLACK);
            buttons.push(Button {
                x: 38,
                y,
                width: if matches!(
                    entry.status,
                    CatalogStatus::Installed
                        | CatalogStatus::NeedsConfiguration
                        | CatalogStatus::UpdateAvailable
                ) && !is_busy
                {
                    self.width - 244
                } else {
                    self.width - 76
                },
                height,
                action,
            });
            if matches!(
                entry.status,
                CatalogStatus::Installed
                    | CatalogStatus::NeedsConfiguration
                    | CatalogStatus::UpdateAvailable
            ) && !is_busy
            {
                let remove = Button {
                    x: self.width - 206,
                    y: y + 111,
                    width: 132,
                    height: 48,
                    action: Action::StoreUninstall(entry.id.clone()),
                };
                self.round_rect(remove.x, remove.y, remove.width, remove.height, GRAY);
                self.text(font, "卸载", 22.0, remove.x + 40, remove.y + 33, BLACK);
                buttons.push(remove);
            }
            y += height + 26;
        }
        self.client.update_all()?;
        Ok(buttons)
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
}
