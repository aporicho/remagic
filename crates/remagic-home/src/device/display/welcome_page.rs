use super::{Display, BLACK, DARK_GRAY, GRAY, WHITE};
use crate::device::{Action, Button};
use ab_glyph::FontArc;
use std::io;

impl Display {
    pub(crate) fn render_welcome(
        &mut self,
        font: &FontArc,
        device_name: &str,
    ) -> io::Result<Vec<Button>> {
        self.use_mono_ui()?;
        self.fill(WHITE);
        self.centered_text(font, "ReMagic 已就绪", 58.0, self.height / 4, BLACK);
        self.centered_text(font, device_name, 27.0, self.height / 4 + 54, DARK_GRAY);
        self.centered_text(
            font,
            "原版系统与镇纸保持不变",
            27.0,
            self.height / 4 + 112,
            DARK_GRAY,
        );

        let width = (self.width - 96).min(900);
        let x = (self.width - width) / 2;
        let height = 118;
        let store_y = self.height / 2 - 42;
        self.rect(x, store_y, width, height, BLACK);
        self.centered_text(font, "打开应用商店", 38.0, store_y + 75, WHITE);

        let system_y = store_y + height + 34;
        self.rect(x, system_y, width, height, GRAY);
        self.centered_text(font, "返回原版系统", 36.0, system_y + 74, BLACK);

        self.centered_text(
            font,
            "以后可三按电源键再次进入 ReMagic",
            24.0,
            system_y + height + 72,
            DARK_GRAY,
        );
        self.client.update_all()?;
        Ok(vec![
            Button {
                x,
                y: store_y,
                width,
                height,
                action: Action::OpenStore,
            },
            Button {
                x,
                y: system_y,
                width,
                height,
                action: Action::System,
            },
        ])
    }
}
