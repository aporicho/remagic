use super::{Display, BLACK, DARK_GRAY, GRAY, WHITE};
use crate::device::settings::{HomeSettings, WallpaperOption};
use crate::device::{Action, Button};
use ab_glyph::FontArc;
use std::io;

impl Display {
    pub(crate) fn render_settings(
        &mut self,
        font: &FontArc,
        settings: &HomeSettings,
        wallpapers: &[WallpaperOption],
    ) -> io::Result<Vec<Button>> {
        self.use_mono_ui()?;
        self.fill(WHITE);
        let mut buttons = Vec::new();
        self.text(font, "设置", 54.0, 48, 82, BLACK);
        self.text(font, "休眠、电源与锁屏外观", 25.0, 50, 128, DARK_GRAY);
        self.small_button(
            font,
            "返回",
            Button {
                x: self.width - 194,
                y: 38,
                width: 152,
                height: 72,
                action: Action::BackManager,
            },
            &mut buttons,
        );

        self.text(font, "锁屏", 36.0, 50, 194, BLACK);
        let wallpaper = settings.wallpaper(wallpapers);
        self.setting_row(
            font,
            "壁纸",
            &wallpaper.label,
            224,
            Action::OpenWallpaperBrowser,
            &mut buttons,
        );
        self.setting_row(
            font,
            "壁纸布局",
            settings.lock.fit.label(),
            374,
            Action::ToggleWallpaperFit,
            &mut buttons,
        );
        self.setting_row(
            font,
            "自动休眠",
            idle_suspend_label(settings.idle_suspend_secs),
            524,
            Action::CycleAutoSleep,
            &mut buttons,
        );

        let preview_y = self.height - 260;
        self.rect(38, preview_y, self.width - 76, 104, BLACK);
        self.centered_text(font, "预览锁屏", 36.0, preview_y + 67, WHITE);
        buttons.push(Button {
            x: 38,
            y: preview_y,
            width: self.width - 76,
            height: 104,
            action: Action::PreviewLock,
        });
        self.centered_text(
            font,
            "将 PNG 放入 /home/root/.local/share/remagic/wallpapers/",
            20.0,
            self.height - 92,
            DARK_GRAY,
        );
        self.client.update_all()?;
        Ok(buttons)
    }

    fn setting_row(
        &mut self,
        font: &FontArc,
        title: &str,
        value: &str,
        y: i32,
        action: Action,
        buttons: &mut Vec<Button>,
    ) {
        let width = self.width - 76;
        self.rect(38, y, width, 126, GRAY);
        self.text(font, title, 34.0, 72, y + 53, BLACK);
        self.text(font, value, 24.0, 72, y + 94, DARK_GRAY);
        self.text(font, "›", 42.0, self.width - 104, y + 76, BLACK);
        buttons.push(Button {
            x: 38,
            y,
            width,
            height: 126,
            action,
        });
    }

    fn small_button(
        &mut self,
        font: &FontArc,
        text: &str,
        button: Button,
        buttons: &mut Vec<Button>,
    ) {
        self.rect(button.x, button.y, button.width, button.height, GRAY);
        let baseline = button.y + button.height / 2 + 12;
        let text_width = super::text_width(font, text, 28.0);
        self.text(
            font,
            text,
            28.0,
            button.x + ((button.width as f32 - text_width) / 2.0).round() as i32,
            baseline,
            BLACK,
        );
        buttons.push(button);
    }
}

fn idle_suspend_label(seconds: u64) -> &'static str {
    match seconds {
        0 => "永不自动休眠",
        60 => "无操作 1 分钟",
        120 => "无操作 2 分钟",
        300 => "无操作 5 分钟",
        600 => "无操作 10 分钟",
        1_800 => "无操作 30 分钟",
        _ => "使用系统配置",
    }
}

#[cfg(test)]
mod tests {
    use super::idle_suspend_label;

    #[test]
    fn idle_suspend_labels_explain_the_policy() {
        assert_eq!(idle_suspend_label(0), "永不自动休眠");
        assert_eq!(idle_suspend_label(120), "无操作 2 分钟");
    }
}
