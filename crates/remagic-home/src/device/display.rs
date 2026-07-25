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

struct TileSpec<'a> {
    title: &'a str,
    status: &'a str,
    icon: &'a str,
    action: Action,
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
        self.use_mono_ui()?;
        self.fill(WHITE);
        let mut buttons = Vec::new();
        self.render_header(font, &mut buttons);
        let y = 178;
        self.render_app_cards(font, apps, &mut buttons, y);
        self.client.update_all()?;
        Ok(buttons)
    }

    fn use_mono_ui(&self) -> io::Result<()> {
        self.client.set_refresh_mode(crate::qtfb::REFRESH_MODE_UI)
    }

    fn use_color_content(&self) -> io::Result<()> {
        self.client
            .set_refresh_mode(crate::qtfb::REFRESH_MODE_CONTENT)
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
        let button_w = 142;
        let button_h = 72;
        let gap = 18;
        let settings_x = self.width - 48 - button_w;
        let sleep_x = settings_x - gap - button_w;
        self.header_button(
            font,
            "休眠",
            Button {
                x: sleep_x,
                y: 38,
                width: button_w,
                height: button_h,
                action: Action::Sleep,
            },
            buttons,
        );
        self.header_button(
            font,
            "设置",
            Button {
                x: settings_x,
                y: 38,
                width: button_w,
                height: button_h,
                action: Action::OpenSettings,
            },
            buttons,
        );
    }

    fn render_app_cards(
        &mut self,
        font: &FontArc,
        apps: &[AppView],
        buttons: &mut Vec<Button>,
        y: i32,
    ) -> i32 {
        let tile = 224;
        let gap = 24;
        let columns = ((self.width - 76 + gap) / (tile + gap)).max(1);
        let grid_width = columns * tile + (columns - 1) * gap;
        let start_x = (self.width - grid_width) / 2;
        let mut index = 0;
        self.render_action_tile(
            font,
            TileSpec {
                title: "应用商店",
                status: "安装和管理应用",
                icon: "店",
                action: Action::OpenStore,
            },
            buttons,
            start_x,
            y,
            tile,
        );
        index += 1;
        for app in apps
            .iter()
            .filter(|app| app.id.as_str() != "remagic-store")
            .take(12)
        {
            let column = index % columns;
            let row = index / columns;
            let x = start_x + column * (tile + gap);
            let tile_y = y + row * (tile + gap);
            if tile_y + tile > self.height - 180 {
                break;
            }
            self.render_app_tile(font, app, buttons, x, tile_y, tile);
            buttons.push(Button {
                x,
                y: tile_y,
                width: tile,
                height: tile,
                action: app_action(app),
            });
            index += 1;
        }
        y + ((index + columns - 1) / columns) * (tile + gap)
    }

    fn render_app_tile(
        &mut self,
        font: &FontArc,
        app: &AppView,
        buttons: &mut Vec<Button>,
        x: i32,
        y: i32,
        size: i32,
    ) {
        self.card(x, y, size, size);
        let icon = 94;
        let icon_x = x + (size - icon) / 2;
        self.rect(icon_x, y + 24, icon, icon, BLACK);
        self.centered_text_in_rect(font, &app_icon_text(app), 43.0, icon_x, icon, y + 84, WHITE);
        self.centered_text_in_rect(
            font,
            &truncate_to_width(font, &app.name, 26.0, size - 28),
            26.0,
            x,
            size,
            y + 152,
            BLACK,
        );
        self.centered_text_in_rect(
            font,
            &truncate_to_width(font, &app_status(app), 18.0, size - 24),
            18.0,
            x,
            size,
            y + 186,
            DARK_GRAY,
        );
        self.render_close_button(font, app, buttons, x, y);
    }

    fn render_action_tile(
        &mut self,
        font: &FontArc,
        spec: TileSpec<'_>,
        buttons: &mut Vec<Button>,
        x: i32,
        y: i32,
        size: i32,
    ) {
        self.card(x, y, size, size);
        let icon = 94;
        let icon_x = x + (size - icon) / 2;
        self.rect(icon_x, y + 24, icon, icon, BLACK);
        self.centered_text_in_rect(font, spec.icon, 38.0, icon_x, icon, y + 84, WHITE);
        self.centered_text_in_rect(
            font,
            &truncate_to_width(font, spec.title, 26.0, size - 28),
            26.0,
            x,
            size,
            y + 152,
            BLACK,
        );
        self.centered_text_in_rect(
            font,
            &truncate_to_width(font, spec.status, 18.0, size - 24),
            18.0,
            x,
            size,
            y + 186,
            DARK_GRAY,
        );
        buttons.push(Button {
            x,
            y,
            width: size,
            height: size,
            action: spec.action,
        });
    }

    fn render_close_button(
        &mut self,
        font: &FontArc,
        app: &AppView,
        buttons: &mut Vec<Button>,
        tile_x: i32,
        y: i32,
    ) {
        if app.session.is_none() && !app.background_active {
            return;
        }
        let button = Button {
            x: tile_x + 150,
            y: y + 18,
            width: 54,
            height: 42,
            action: Action::Close(app.id.clone()),
        };
        self.rect(button.x, button.y, button.width, button.height, 0xFFD0_CECB);
        self.centered_text_in_rect(
            font,
            "关",
            22.0,
            button.x,
            button.width,
            button.y + 29,
            BLACK,
        );
        buttons.push(Button {
            x: button.x,
            y: button.y,
            width: button.width,
            height: button.height,
            action: button.action,
        });
    }

    fn card(&mut self, x: i32, y: i32, width: i32, height: i32) {
        self.rect(x, y, width, height, GRAY);
        self.hline(x + 18, x + width - 18, y + height - 1, 0xFFD2_D0CB);
    }

    fn header_button(
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
            28.0,
            button.x,
            button.width,
            button.y + 47,
            BLACK,
        );
        buttons.push(button);
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

    fn centered_text_in_rect(
        &mut self,
        font: &FontArc,
        text: &str,
        size: f32,
        x: i32,
        width: i32,
        baseline: i32,
        color: u32,
    ) {
        let text_width = text_width(font, text, size);
        let text_x = x + ((width as f32 - text_width) / 2.0).round() as i32;
        self.text(font, text, size, text_x.max(x), baseline, color);
    }

    fn progress_bar(&mut self, x: i32, y: i32, width: i32, height: i32, fraction: Option<f32>) {
        self.rect(x, y, width, height, 0xFFD0_CECB);
        let inner = ((width - 8) as f32 * fraction.unwrap_or(0.36).clamp(0.0, 1.0)).round() as i32;
        if inner > 0 {
            self.rect(x + 4, y + 4, inner, height - 8, BLACK);
        }
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

fn truncate_to_width(font: &FontArc, text: &str, size: f32, max_width: i32) -> String {
    if text_width(font, text, size) <= max_width as f32 {
        return text.to_owned();
    }
    let suffix = "...";
    let suffix_width = text_width(font, suffix, size);
    let mut output = String::new();
    for ch in text.chars() {
        let candidate = format!("{output}{ch}");
        if text_width(font, &candidate, size) + suffix_width > max_width as f32 {
            break;
        }
        output.push(ch);
    }
    if output.is_empty() {
        suffix.to_owned()
    } else {
        output.push_str(suffix);
        output
    }
}

fn app_icon_text(app: &AppView) -> String {
    match app.id.as_str() {
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
