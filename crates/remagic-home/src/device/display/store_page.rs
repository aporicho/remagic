use super::{Display, BLACK, DARK_GRAY, GRAY, WHITE};
use crate::device::{store::CatalogApp, Action, Button};
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
            })
            .collect::<Vec<_>>();
        let entries = if catalog.is_empty() {
            &fallback
        } else {
            catalog
        };
        for entry in entries {
            let installed = apps.iter().find(|app| app.id.as_str() == entry.id);
            let is_busy = busy == Some(entry.id.as_str());
            let height = 176;
            self.card(38, y, self.width - 76, height);
            self.text(font, &entry.name, 41.0, 72, y + 58, BLACK);
            self.text(font, &entry.summary, 24.0, 72, y + 99, DARK_GRAY);
            let (status, action) = if is_busy {
                ("正在下载、验证并安装…", Action::Unavailable)
            } else if installed.is_some_and(|app| app.installed) {
                (
                    "已安装 · 轻点打开",
                    Action::Launch(AppId::new(entry.id.clone()).expect("catalog app id")),
                )
            } else {
                ("未安装 · 轻点安装", Action::StoreInstall(entry.id.clone()))
            };
            self.text(font, status, 24.0, 72, y + 142, BLACK);
            buttons.push(Button {
                x: 38,
                y,
                width: self.width - 76,
                height,
                action,
            });
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
