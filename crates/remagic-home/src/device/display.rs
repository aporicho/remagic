use super::{Action, Button};
use ab_glyph::{point, Font, FontArc, Glyph, PxScale, ScaleFont};
use remagic_protocol::AppView;
use std::fs;
use std::io;
use std::os::fd::RawFd;

const WHITE: u32 = 0xFFFF_FFFF;
const BLACK: u32 = 0xFF18_1818;
const GRAY: u32 = 0xFFE8_E6E1;
const DARK_GRAY: u32 = 0xFF73_716C;

mod lock_page;
mod settings_page;
mod store_page;
mod wallpaper;
mod wallpaper_browser;
mod welcome_page;

pub(super) fn prepare_wallpaper_thumbnails(options: &[super::WallpaperOption]) {
    wallpaper::prepare_thumbnails(options)
}

pub(super) fn load_font() -> Result<FontArc, Box<dyn std::error::Error>> {
    let paths = [
        "/home/root/apps/remagic/fonts/UIFont.ttf",
        "/usr/share/fonts/ttf/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ];
    for path in paths {
        if let Ok(bytes) = fs::read(path) {
            if let Ok(font) = FontArc::try_from_vec(bytes) {
                return Ok(font);
            }
        }
    }
    Err("ReMagic UI font is missing".into())
}

pub(super) struct Display {
    width: i32,
    height: i32,
    stride: usize,
    client: crate::qtfb::Client,
}

impl Display {
    pub(super) fn open() -> io::Result<Self> {
        let client = crate::qtfb::Client::connect()?;
        Ok(Self {
            width: client.width,
            height: client.height,
            stride: client.stride,
            client,
        })
    }

    pub(super) fn render(&mut self, font: &FontArc, apps: &[AppView]) -> io::Result<Vec<Button>> {
        self.fill(WHITE);
        let mut buttons = Vec::new();
        self.render_header(font, &mut buttons);
        let y = self.render_system_card(font, &mut buttons);
        let y = self.render_store_card(font, &mut buttons, y);
        self.render_app_cards(font, apps, &mut buttons, y);
        self.render_sleep_button(font, &mut buttons);
        self.client.update_all()?;
        Ok(buttons)
    }

    fn render_header(&mut self, font: &FontArc, buttons: &mut Vec<Button>) {
        self.text(font, "应用与任务", 54.0, 48, 82, BLACK);
        self.text(
            font,
            "单按返回上一应用 · 三按返回原版系统",
            25.0,
            50,
            128,
            DARK_GRAY,
        );
        let x = self.width - 190;
        self.rect(x, 38, 142, 72, GRAY);
        self.text(font, "设置", 28.0, x + 42, 84, BLACK);
        buttons.push(Button {
            x,
            y: 38,
            width: 142,
            height: 72,
            action: Action::OpenSettings,
        });
    }

    fn render_system_card(&mut self, font: &FontArc, buttons: &mut Vec<Button>) -> i32 {
        let y = 178;
        let card_h = 132;
        self.card(38, y, self.width - 76, card_h);
        self.text(font, "原版系统", 40.0, 72, y + 57, BLACK);
        self.text(font, "reMarkable + 镇纸", 24.0, 72, y + 96, DARK_GRAY);
        buttons.push(Button {
            x: 38,
            y,
            width: self.width - 76,
            height: card_h,
            action: Action::System,
        });
        y + card_h + 22
    }

    fn render_app_cards(
        &mut self,
        font: &FontArc,
        apps: &[AppView],
        buttons: &mut Vec<Button>,
        mut y: i32,
    ) -> i32 {
        let card_h = 132;
        for app in apps
            .iter()
            .filter(|app| app.id.as_str() != "remagic-store")
            .take(7)
        {
            if y + card_h > self.height - 210 {
                break;
            }
            self.card(38, y, self.width - 76, card_h);
            self.text(font, &app.name, 39.0, 72, y + 54, BLACK);
            self.text(font, &app_status(app), 24.0, 72, y + 96, DARK_GRAY);
            self.render_close_button(font, app, buttons, y);
            buttons.push(Button {
                x: 38,
                y,
                width: self.width - 76,
                height: card_h,
                action: app_action(app),
            });
            y += card_h + 22;
        }
        y
    }

    fn render_close_button(
        &mut self,
        font: &FontArc,
        app: &AppView,
        buttons: &mut Vec<Button>,
        y: i32,
    ) {
        if app.session.is_none() && !app.background_active {
            return;
        }
        self.rect(self.width - 190, y + 28, 112, 70, 0xFFD0_CECB);
        self.text(font, "关闭", 25.0, self.width - 164, y + 72, BLACK);
        buttons.push(Button {
            x: self.width - 190,
            y: y + 28,
            width: 112,
            height: 70,
            action: Action::Close(app.id.clone()),
        });
    }

    fn render_sleep_button(&mut self, font: &FontArc, buttons: &mut Vec<Button>) {
        let y = self.height - 150;
        self.rect(38, y, self.width - 76, 104, BLACK);
        self.text(font, "休眠", 38.0, self.width / 2 - 46, y + 67, WHITE);
        buttons.push(Button {
            x: 38,
            y,
            width: self.width - 76,
            height: 104,
            action: Action::Sleep,
        });
    }

    fn card(&mut self, x: i32, y: i32, width: i32, height: i32) {
        self.rect(x, y, width, height, GRAY);
        self.hline(x + 18, x + width - 18, y + height - 1, 0xFFD2_D0CB);
    }

    pub(super) fn press(&mut self, button: &Button) -> io::Result<Vec<u32>> {
        let mut saved = Vec::with_capacity((button.width * button.height) as usize);
        for y in button.y..button.y + button.height {
            for x in button.x..button.x + button.width {
                let color = self.read_pixel(x, y);
                saved.push(color);
                self.pixel(x, y, (color & 0xFF00_0000) | ((!color) & 0x00FF_FFFF));
            }
        }
        self.client
            .update(button.x, button.y, button.width, button.height)?;
        Ok(saved)
    }

    pub(super) fn release(&mut self, button: &Button, saved: Vec<u32>) -> io::Result<()> {
        let mut index = 0;
        for y in button.y..button.y + button.height {
            for x in button.x..button.x + button.width {
                self.pixel(x, y, saved[index]);
                index += 1;
            }
        }
        self.client
            .update(button.x, button.y, button.width, button.height)
    }

    pub(super) fn poll_touch_events(&mut self) -> io::Result<Vec<crate::qtfb::TouchEvent>> {
        self.client.poll_touch_events()
    }

    pub(super) fn input_fd(&self) -> RawFd {
        self.client.raw_fd()
    }

    pub(super) fn commit_sequence(&self) -> u64 {
        self.client.commit_sequence()
    }

    fn fill(&mut self, color: u32) {
        for y in 0..self.height {
            for x in 0..self.width {
                self.pixel(x, y, color)
            }
        }
    }

    fn rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: u32) {
        for py in y..y + height {
            for px in x..x + width {
                self.pixel(px, py, color);
            }
        }
    }

    fn hline(&mut self, x0: i32, x1: i32, y: i32, color: u32) {
        for x in x0..x1 {
            self.pixel(x, y, color)
        }
    }

    fn text(&mut self, font: &FontArc, text: &str, size: f32, x: i32, baseline: i32, color: u32) {
        let scaled = font.as_scaled(PxScale::from(size));
        let mut caret = x as f32;
        let mut previous = None;
        for ch in text.chars() {
            let id = scaled.glyph_id(ch);
            if let Some(previous) = previous {
                caret += scaled.kern(previous, id)
            }
            let glyph: Glyph = id.with_scale_and_position(size, point(caret, baseline as f32));
            if let Some(outline) = font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, coverage| {
                    if coverage > 0.25 {
                        self.pixel(
                            bounds.min.x as i32 + gx as i32,
                            bounds.min.y as i32 + gy as i32,
                            color,
                        );
                    }
                });
            }
            caret += scaled.h_advance(id);
            previous = Some(id);
        }
    }

    fn centered_text(&mut self, font: &FontArc, text: &str, size: f32, baseline: i32, color: u32) {
        let width = text_width(font, text, size);
        let x = ((self.width as f32 - width) / 2.0).round() as i32;
        self.text(font, text, size, x.max(0), baseline, color);
    }

    fn pixel(&mut self, x: i32, y: i32, color: u32) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let red = ((color >> 16) & 0xff) as u16;
        let green = ((color >> 8) & 0xff) as u16;
        let blue = (color & 0xff) as u16;
        let rgb565 = ((red & 0xf8) << 8) | ((green & 0xfc) << 3) | (blue >> 3);
        let offset = y as usize * self.stride + x as usize * 2;
        self.client.pixels_mut()[offset..offset + 2].copy_from_slice(&rgb565.to_le_bytes());
    }

    fn read_pixel(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return WHITE;
        }
        let offset = y as usize * self.stride + x as usize * 2;
        let bytes = &self.client.pixels()[offset..offset + 2];
        let value = u16::from_le_bytes([bytes[0], bytes[1]]);
        let red = ((value >> 11) & 0x1f) as u32 * 255 / 31;
        let green = ((value >> 5) & 0x3f) as u32 * 255 / 63;
        let blue = (value & 0x1f) as u32 * 255 / 31;
        0xff00_0000 | (red << 16) | (green << 8) | blue
    }
}

fn text_width(font: &FontArc, text: &str, size: f32) -> f32 {
    let scaled = font.as_scaled(PxScale::from(size));
    let mut width = 0.0;
    let mut previous = None;
    for ch in text.chars() {
        let id = scaled.glyph_id(ch);
        if let Some(previous) = previous {
            width += scaled.kern(previous, id);
        }
        width += scaled.h_advance(id);
        previous = Some(id);
    }
    width
}

fn app_status(app: &AppView) -> String {
    if app.id.as_str() == "magicpaper" && queued_magicpaper_result() {
        "有新的定时任务结果".into()
    } else if let Some(session) = &app.session {
        if session.subtitle.is_empty() {
            "已暂停，可继续".into()
        } else {
            session.subtitle.clone()
        }
    } else if app.installed && app.background_active {
        "已安装 · 后台服务运行中".into()
    } else if app.installed {
        "已安装".into()
    } else {
        "未安装".into()
    }
}

fn queued_magicpaper_result() -> bool {
    fs::metadata("/home/root/.local/share/magicpaper/agent/pending.tsv")
        .is_ok_and(|metadata| metadata.len() > 0)
}

fn app_action(app: &AppView) -> Action {
    if app.installed {
        Action::Launch(app.id.clone())
    } else {
        Action::Unavailable
    }
}
