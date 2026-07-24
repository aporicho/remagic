use super::{Display, BLACK, DARK_GRAY, GRAY, WHITE};
use crate::device::settings::{HomeSettings, WallpaperOption};
use crate::device::{Action, Button};
use ab_glyph::FontArc;
use std::io;

pub(super) const WALLPAPERS_PER_PAGE: usize = 8;

#[derive(Clone, Copy)]
struct BrowserGrid {
    margin: i32,
    gap: i32,
    columns: i32,
    card_width: i32,
    card_height: i32,
    start_y: i32,
}

impl BrowserGrid {
    fn new(width: i32, height: i32) -> Self {
        let margin = 38;
        let gap = 24;
        let columns = if width >= 1_300 { 4 } else { 2 };
        let card_width = (width - margin * 2 - gap * (columns - 1)) / columns;
        let rows = WALLPAPERS_PER_PAGE as i32 / columns;
        let start_y = 174;
        let card_height = (height - start_y - 190 - gap * (rows - 1)) / rows;
        Self {
            margin,
            gap,
            columns,
            card_width,
            card_height,
            start_y,
        }
    }

    fn origin(self, slot: usize) -> (i32, i32) {
        let column = slot as i32 % self.columns;
        let row = slot as i32 / self.columns;
        (
            self.margin + column * (self.card_width + self.gap),
            self.start_y + row * (self.card_height + self.gap),
        )
    }
}

impl Display {
    pub(crate) fn render_wallpaper_browser(
        &mut self,
        font: &FontArc,
        settings: &HomeSettings,
        wallpapers: &[WallpaperOption],
        requested_page: usize,
    ) -> io::Result<(Vec<Button>, usize)> {
        self.use_color_content()?;
        self.fill(WHITE);
        let pages = wallpapers.len().max(1).div_ceil(WALLPAPERS_PER_PAGE);
        let page = requested_page.min(pages - 1);
        let mut buttons = Vec::new();
        self.render_browser_header(font, settings, page, pages, &mut buttons);
        self.render_browser_cards(
            font,
            settings,
            wallpapers,
            page,
            BrowserGrid::new(self.width, self.height),
            &mut buttons,
        );
        self.render_browser_footer(font, settings, page, pages, &mut buttons);
        self.client.update_all()?;
        Ok((buttons, page))
    }

    fn render_browser_header(
        &mut self,
        font: &FontArc,
        settings: &HomeSettings,
        page: usize,
        pages: usize,
        buttons: &mut Vec<Button>,
    ) {
        self.text(font, "壁纸", 54.0, 48, 82, BLACK);
        self.text(
            font,
            &format!(
                "点击图片选择 · {} · {}/{}",
                settings.lock.fit.label(),
                page + 1,
                pages
            ),
            25.0,
            50,
            128,
            DARK_GRAY,
        );
        self.browser_button(
            font,
            "返回",
            self.width - 190,
            38,
            152,
            Action::BackSettings,
            buttons,
        );
    }

    fn render_browser_cards(
        &mut self,
        font: &FontArc,
        settings: &HomeSettings,
        wallpapers: &[WallpaperOption],
        page: usize,
        grid: BrowserGrid,
        buttons: &mut Vec<Button>,
    ) {
        let label_height = 52;
        let start = page * WALLPAPERS_PER_PAGE;
        for (slot, option) in wallpapers
            .iter()
            .skip(start)
            .take(WALLPAPERS_PER_PAGE)
            .enumerate()
        {
            let (x, y) = grid.origin(slot);
            let selected = option.id == settings.lock.wallpaper;
            self.rect(
                x,
                y,
                grid.card_width,
                grid.card_height,
                if selected { BLACK } else { GRAY },
            );
            super::wallpaper::draw_thumbnail(
                self,
                option,
                x + 5,
                y + 5,
                grid.card_width - 10,
                grid.card_height - label_height - 10,
            );
            let label = if selected {
                format!("✓ {}", option.label)
            } else {
                option.label.clone()
            };
            self.centered_text(
                font,
                &label,
                23.0,
                y + grid.card_height - 17,
                if selected { WHITE } else { BLACK },
            );
            buttons.push(Button {
                x,
                y,
                width: grid.card_width,
                height: grid.card_height,
                action: Action::SelectWallpaper(option.id.clone()),
            });
        }
    }

    fn render_browser_footer(
        &mut self,
        font: &FontArc,
        settings: &HomeSettings,
        page: usize,
        pages: usize,
        buttons: &mut Vec<Button>,
    ) {
        let footer_y = self.height - 154;
        self.browser_button(
            font,
            settings.lock.fit.label(),
            38,
            footer_y,
            250,
            Action::ToggleWallpaperFit,
            buttons,
        );
        if page > 0 {
            self.browser_button(
                font,
                "上一页",
                self.width - 370,
                footer_y,
                150,
                Action::WallpaperPage(-1),
                buttons,
            );
        }
        if page + 1 < pages {
            self.browser_button(
                font,
                "下一页",
                self.width - 200,
                footer_y,
                150,
                Action::WallpaperPage(1),
                buttons,
            );
        }
    }

    fn browser_button(
        &mut self,
        font: &FontArc,
        label: &str,
        x: i32,
        y: i32,
        width: i32,
        action: Action,
        buttons: &mut Vec<Button>,
    ) {
        self.rect(x, y, width, 72, GRAY);
        let text_width = super::text_width(font, label, 27.0);
        self.text(
            font,
            label,
            27.0,
            x + ((width as f32 - text_width) / 2.0).round() as i32,
            y + 47,
            BLACK,
        );
        buttons.push(Button {
            x,
            y,
            width,
            height: 72,
            action,
        });
    }
}
