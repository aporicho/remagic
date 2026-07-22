use super::{Display, BLACK, DARK_GRAY, WHITE};
use crate::device::settings::{HomeSettings, WallpaperOption};
use crate::device::{Action, Button};
use ab_glyph::FontArc;
use remagic_protocol::{LOCK_UNLOCK_HEIGHT, LOCK_UNLOCK_WIDTH, LOCK_UNLOCK_X, LOCK_UNLOCK_Y};
use std::io;

impl Display {
    pub(crate) fn render_locked(
        &mut self,
        font: &FontArc,
        settings: &HomeSettings,
        wallpapers: &[WallpaperOption],
        preview: bool,
    ) -> io::Result<Vec<Button>> {
        super::wallpaper::draw(self, settings, settings.wallpaper(wallpapers));
        if settings.lock.show_clock {
            self.centered_text(font, &current_time(), 76.0, 230, BLACK);
        }
        self.round_rect(58, 548, self.width - 116, 232, 0xFFF2_F1EE);
        self.centered_text(font, "Remagic 已锁定", 50.0, 642, BLACK);
        if settings.lock.show_hint {
            self.centered_text(font, "按电源键唤醒后自动返回管理器", 27.0, 710, DARK_GRAY);
        }
        if preview {
            self.text(font, "锁屏预览", 28.0, 42, 66, BLACK);
        }

        let x = LOCK_UNLOCK_X;
        let y = LOCK_UNLOCK_Y;
        let width = LOCK_UNLOCK_WIDTH.min(self.width - x);
        let height = LOCK_UNLOCK_HEIGHT;
        self.round_rect(x, y, width, height, BLACK);
        self.centered_text(
            font,
            if preview {
                "返回设置"
            } else {
                "立即返回"
            },
            40.0,
            y + 79,
            WHITE,
        );
        self.client.update_all()?;
        Ok(vec![Button {
            x,
            y,
            width,
            height,
            action: if preview {
                Action::BackSettings
            } else {
                Action::Wake
            },
        }])
    }
}

fn current_time() -> String {
    let mut timestamp: libc::time_t = 0;
    let mut local = std::mem::MaybeUninit::<libc::tm>::uninit();
    unsafe {
        libc::time(&mut timestamp);
        if libc::localtime_r(&timestamp, local.as_mut_ptr()).is_null() {
            return "--:--".into();
        }
        let local = local.assume_init();
        format!("{:02}:{:02}", local.tm_hour, local.tm_min)
    }
}
