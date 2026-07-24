use super::{Display, BLACK, GRAY};
use crate::device::settings::{HomeSettings, WallpaperOption};
use crate::device::{Action, Button};
use ab_glyph::FontArc;
use std::io;

impl Display {
    pub(crate) fn render_locked(
        &mut self,
        font: &FontArc,
        settings: &HomeSettings,
        wallpapers: &[WallpaperOption],
        preview: bool,
    ) -> io::Result<Vec<Button>> {
        self.use_color_content()?;
        super::wallpaper::draw(self, settings, settings.wallpaper(wallpapers));
        let mut buttons = Vec::new();
        if preview {
            self.text(font, "锁屏预览", 28.0, 42, 66, BLACK);
            let x = self.width - 190;
            self.rect(x, 38, 152, 72, GRAY);
            self.text(font, "返回", 28.0, x + 48, 84, BLACK);
            buttons.push(Button {
                x,
                y: 38,
                width: 152,
                height: 72,
                action: Action::BackSettings,
            });
        }
        self.client.update_all()?;
        Ok(buttons)
    }
}
