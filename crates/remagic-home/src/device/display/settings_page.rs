use super::{Display, BLACK, DARK_GRAY, GRAY, WHITE};
use crate::device::settings::{HomeSettings, WallpaperOption};
use crate::device::{Action, Button};
use ab_glyph::FontArc;
use remagic_core::BacklightSnapshot;
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
        self.text(font, "系统、电源与锁屏外观", 25.0, 50, 128, DARK_GRAY);
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

        self.text(font, "系统", 36.0, 50, 194, BLACK);
        self.setting_row(
            font,
            "系统更新",
            "检查并安装 ReMagic 系统更新",
            224,
            Action::OpenSystemUpdate,
            &mut buttons,
        );
        self.setting_row(
            font,
            "返回原版系统",
            "退出 ReMagic，回到 reMarkable 原版系统",
            374,
            Action::System,
            &mut buttons,
        );

        self.text(font, "电源", 36.0, 50, 524, BLACK);
        self.setting_row(
            font,
            "背光",
            &backlight_label(settings.backlight.as_ref()),
            554,
            Action::OpenBacklight,
            &mut buttons,
        );
        self.setting_row(
            font,
            "自动休眠",
            idle_suspend_label(settings.idle_suspend_secs),
            704,
            Action::CycleAutoSleep,
            &mut buttons,
        );

        self.text(font, "锁屏", 36.0, 50, 884, BLACK);
        let wallpaper = settings.wallpaper(wallpapers);
        self.setting_row(
            font,
            "壁纸",
            &wallpaper.label,
            914,
            Action::OpenWallpaperBrowser,
            &mut buttons,
        );
        self.setting_row(
            font,
            "壁纸布局",
            settings.lock.fit.label(),
            1064,
            Action::ToggleWallpaperFit,
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

    pub(crate) fn render_backlight_settings(
        &mut self,
        font: &FontArc,
        snapshot: Option<&BacklightSnapshot>,
    ) -> io::Result<Vec<Button>> {
        self.use_mono_ui()?;
        self.fill(WHITE);
        let mut buttons = Vec::new();
        self.text(font, "背光", 54.0, 48, 82, BLACK);
        self.text(font, "阅读灯亮度", 25.0, 50, 128, DARK_GRAY);
        self.small_button(
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

        let Some(snapshot) = snapshot else {
            self.text(font, "正在读取背光状态", 34.0, 52, 226, BLACK);
            self.client.update_all()?;
            return Ok(buttons);
        };
        if !snapshot.supported {
            self.render_backlight_unavailable(font, snapshot);
            self.client.update_all()?;
            return Ok(buttons);
        }

        self.render_backlight_status(font, snapshot);
        self.render_backlight_controls(font, &mut buttons);
        self.client.update_all()?;
        Ok(buttons)
    }

    fn render_backlight_unavailable(&mut self, font: &FontArc, snapshot: &BacklightSnapshot) {
        self.text(font, "当前设备未暴露背光控制", 34.0, 52, 226, BLACK);
        if let Some(error) = &snapshot.error {
            self.text(
                font,
                &super::truncate_to_width(font, error, 23.0, self.width - 104),
                23.0,
                52,
                270,
                DARK_GRAY,
            );
        }
    }

    fn render_backlight_status(&mut self, font: &FontArc, snapshot: &BacklightSnapshot) {
        let percent = snapshot.percent.unwrap_or(0).min(100);
        self.centered_text(font, &format!("{percent}%"), 88.0, 262, BLACK);
        self.centered_text(
            font,
            backlight_status_label(snapshot, percent),
            26.0,
            310,
            DARK_GRAY,
        );
        self.progress_bar(72, 352, self.width - 144, 26, Some(percent as f32 / 100.0));
        let native = match (snapshot.brightness, snapshot.max_brightness) {
            (Some(brightness), Some(max)) => format!("原生亮度 {brightness}/{max}"),
            _ => "原生亮度未知".into(),
        };
        self.centered_text(font, &native, 22.0, 420, DARK_GRAY);
        if let Some(error) = &snapshot.error {
            self.centered_text(
                font,
                &super::truncate_to_width(font, error, 21.0, self.width - 104),
                21.0,
                456,
                DARK_GRAY,
            );
        }
    }

    fn render_backlight_controls(&mut self, font: &FontArc, buttons: &mut Vec<Button>) {
        let control_y = 524;
        let gap = 24;
        let button_width = (self.width - 76 - gap) / 2;
        self.large_control_button(
            font,
            "-5%",
            Button {
                x: 38,
                y: control_y,
                width: button_width,
                height: 116,
                action: Action::AdjustBacklight(-5),
            },
            buttons,
        );
        self.large_control_button(
            font,
            "+5%",
            Button {
                x: 38 + button_width + gap,
                y: control_y,
                width: button_width,
                height: 116,
                action: Action::AdjustBacklight(5),
            },
            buttons,
        );

        let presets = [0_u8, 25, 50, 75, 100];
        let preset_gap = 16;
        let preset_width = (self.width - 76 - preset_gap * 4) / 5;
        let preset_y = control_y + 154;
        for (index, preset) in presets.into_iter().enumerate() {
            self.large_control_button(
                font,
                &format!("{preset}%"),
                Button {
                    x: 38 + index as i32 * (preset_width + preset_gap),
                    y: preset_y,
                    width: preset_width,
                    height: 104,
                    action: Action::SetBacklight(preset),
                },
                buttons,
            );
        }
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

    fn large_control_button(
        &mut self,
        font: &FontArc,
        text: &str,
        button: Button,
        buttons: &mut Vec<Button>,
    ) {
        self.rect(button.x, button.y, button.width, button.height, GRAY);
        self.centered_text_in_rect(
            font,
            text,
            30.0,
            button.x,
            button.width,
            button.y + button.height / 2 + 11,
            BLACK,
        );
        buttons.push(button);
    }
}

fn backlight_label(snapshot: Option<&BacklightSnapshot>) -> String {
    match snapshot {
        Some(snapshot) if snapshot.supported && snapshot.forced_off => {
            format!("{}% · 锁屏熄灯", snapshot.percent.unwrap_or(0))
        }
        Some(snapshot) if snapshot.supported => format!("{}%", snapshot.percent.unwrap_or(0)),
        Some(snapshot) if snapshot.error.is_some() => "不可用".into(),
        Some(_) => "不可用".into(),
        None => "读取中".into(),
    }
}

fn backlight_status_label(snapshot: &BacklightSnapshot, percent: u8) -> &'static str {
    if snapshot.forced_off {
        "锁屏期间已临时关闭"
    } else if percent == 0 {
        "阅读灯关闭"
    } else {
        "阅读灯开启"
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
    use super::{backlight_label, idle_suspend_label};
    use remagic_core::BacklightSnapshot;

    #[test]
    fn idle_suspend_labels_explain_the_policy() {
        assert_eq!(idle_suspend_label(0), "永不自动休眠");
        assert_eq!(idle_suspend_label(120), "无操作 2 分钟");
    }

    #[test]
    fn backlight_labels_show_supported_state() {
        let snapshot = BacklightSnapshot {
            supported: true,
            percent: Some(75),
            forced_off: false,
            provider: Some("rm_frontlight".into()),
            brightness: Some(1535),
            max_brightness: Some(2047),
            bl_power: Some(0),
            linear_mapping: Some("no".into()),
            error: None,
        };
        assert_eq!(backlight_label(Some(&snapshot)), "75%");
        assert_eq!(backlight_label(None), "读取中");
    }
}
