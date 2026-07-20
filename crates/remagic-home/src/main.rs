use remagic_protocol::{read_frame, write_frame, AppView, Request, Response};
use tokio::net::UnixStream;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let apps = list_apps().await?;
    #[cfg(feature = "device")]
    return device::run(apps).await;
    #[cfg(not(feature = "device"))]
    {
        println!("Remagic Manager");
        for app in apps {
            println!("- {} ({})", app.name, app.id);
        }
        Ok(())
    }
}

async fn list_apps() -> Result<Vec<AppView>, Box<dyn std::error::Error>> {
    let mut stream = UnixStream::connect(remagic_protocol::DEFAULT_SOCKET).await?;
    write_frame(&mut stream, &Request::ListApps).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Apps { apps } => Ok(apps),
        Response::Error { message } => Err(message.into()),
        _ => Err("unexpected manager response".into()),
    }
}

#[cfg(feature = "device")]
async fn request(request: Request) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = UnixStream::connect(remagic_protocol::DEFAULT_SOCKET).await?;
    write_frame(&mut stream, &request).await?;
    match read_frame::<_, Response>(&mut stream).await? {
        Response::Ok => Ok(()),
        Response::Error { message } => Err(message.into()),
        _ => Err("unexpected manager response".into()),
    }
}

#[cfg(feature = "device")]
mod device {
    use super::*;
    use ab_glyph::{point, Font, FontArc, Glyph, PxScale, ScaleFont};
    use remagic_core::AppId;
    use std::ffi::CString;
    use std::fs;
    use std::io;
    use std::os::fd::RawFd;
    use std::time::Duration;

    const WHITE: u32 = 0xFFFF_FFFF;
    const BLACK: u32 = 0xFF18_1818;
    const GRAY: u32 = 0xFFE8_E6_E1;
    const DARK_GRAY: u32 = 0xFF73_716C;

    #[link(name = "quill")]
    extern "C" {
        fn quill_init() -> i32;
        fn quill_width() -> i32;
        fn quill_height() -> i32;
        fn quill_stride() -> i32;
        fn quill_buffer() -> *mut u8;
        fn quill_swap_ex(
            x: i32,
            y: i32,
            width: i32,
            height: i32,
            mode: i32,
            full: i32,
            content: i32,
        ) -> u64;
        fn quill_process_events();
    }

    #[derive(Clone)]
    enum Action {
        Launch(AppId),
        Close(AppId),
        Package(remagic_protocol::PackageOperation),
        Unavailable,
        System,
        Sleep,
    }

    #[derive(Clone)]
    struct Button {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        action: Action,
    }

    pub async fn run(apps: Vec<AppView>) -> Result<(), Box<dyn std::error::Error>> {
        let mut display = Display::open()?;
        let font = load_font()?;
        let buttons = display.render(&font, &apps);
        let mut touch = Touch::open()?;
        loop {
            if let Some((x, y)) = touch.poll_tap() {
                if let Some(button) = buttons.iter().find(|button| {
                    x >= button.x
                        && x < button.x + button.width
                        && y >= button.y
                        && y < button.y + button.height
                }) {
                    display.flash(button);
                    match &button.action {
                        Action::Launch(id) => {
                            request(Request::Launch {
                                app_id: id.clone(),
                                open_path: None,
                            })
                            .await?;
                        }
                        Action::Close(id) => {
                            request(Request::Close {
                                app_id: id.clone(),
                                complete: true,
                            })
                            .await?;
                        }
                        Action::Package(operation) => {
                            request(Request::Package {
                                operation: operation.clone(),
                            })
                            .await?;
                        }
                        Action::Unavailable => {}
                        Action::System => request(Request::ReturnSystem).await?,
                        Action::Sleep => request(Request::Sleep).await?,
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    // Restore the normal card after the acknowledgement flash.
                    // The daemon will stop this UI when an app is launched.
                    display.render(&font, &apps);
                }
            }
            unsafe { quill_process_events() };
            tokio::time::sleep(Duration::from_millis(12)).await;
        }
    }

    fn load_font() -> Result<FontArc, Box<dyn std::error::Error>> {
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
        Err("Remagic UI font is missing".into())
    }

    struct Display {
        width: i32,
        height: i32,
        stride: usize,
        buffer: *mut u8,
    }

    impl Display {
        fn open() -> io::Result<Self> {
            if unsafe { quill_init() } != 0 {
                return Err(io::Error::other("quill_init failed"));
            }
            let display = Self {
                width: unsafe { quill_width() },
                height: unsafe { quill_height() },
                stride: unsafe { quill_stride() } as usize,
                buffer: unsafe { quill_buffer() },
            };
            if display.width <= 0 || display.height <= 0 || display.buffer.is_null() {
                return Err(io::Error::other("invalid Quill framebuffer"));
            }
            Ok(display)
        }

        fn render(&mut self, font: &FontArc, apps: &[AppView]) -> Vec<Button> {
            self.fill(WHITE);
            self.text(font, "应用与任务", 54.0, 48, 82, BLACK);
            self.text(
                font,
                "单按返回上一应用 · 三按返回原版系统",
                25.0,
                50,
                128,
                DARK_GRAY,
            );

            let mut buttons = Vec::new();
            let mut y = 178;
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
            y += card_h + 22;

            for app in apps.iter().take(7) {
                if y + card_h > self.height - 210 {
                    break;
                }
                self.card(38, y, self.width - 76, card_h);
                self.text(font, &app.name, 39.0, 72, y + 54, BLACK);
                let queued_result = app.id.as_str() == "magicpaper"
                    && fs::metadata("/home/root/riddle-data/agent/pending.tsv")
                        .is_ok_and(|metadata| metadata.len() > 0);
                let status = if queued_result {
                    "有新的定时任务结果".to_string()
                } else if let Some(session) = &app.session {
                    if session.subtitle.is_empty() {
                        "已暂停，可继续".to_string()
                    } else {
                        session.subtitle.clone()
                    }
                } else if app.installed {
                    if app.background_active {
                        "已安装 · 后台服务运行中".into()
                    } else {
                        "已安装".into()
                    }
                } else {
                    "未安装".into()
                };
                self.text(font, &status, 24.0, 72, y + 96, DARK_GRAY);
                if app.session.is_some() || app.background_active {
                    self.round_rect(self.width - 190, y + 28, 112, 70, 0xFFD0_CECB);
                    self.text(font, "关闭", 25.0, self.width - 164, y + 72, BLACK);
                    buttons.push(Button {
                        x: self.width - 190,
                        y: y + 28,
                        width: 112,
                        height: 70,
                        action: Action::Close(app.id.clone()),
                    });
                }
                let action = if app.installed {
                    Action::Launch(app.id.clone())
                } else if let Some(package) = &app.package {
                    Action::Package(remagic_protocol::PackageOperation::Install {
                        package: package.clone(),
                    })
                } else {
                    Action::Unavailable
                };
                buttons.push(Button {
                    x: 38,
                    y,
                    width: self.width - 76,
                    height: card_h,
                    action,
                });
                y += card_h + 22;
            }

            if y + 112 < self.height - 170 {
                self.card(38, y, self.width - 76, 112);
                self.text(font, "Vellum 软件包", 35.0, 72, y + 48, BLACK);
                let vellum_ready = std::path::Path::new("/home/root/.vellum/bin/vellum").is_file()
                    || std::path::Path::new("/usr/bin/vellum").is_file();
                self.text(
                    font,
                    if vellum_ready {
                        "已就绪 · 轻点更新索引"
                    } else {
                        "未安装 · 轻点安全安装"
                    },
                    22.0,
                    72,
                    y + 86,
                    DARK_GRAY,
                );
                buttons.push(Button {
                    x: 38,
                    y,
                    width: self.width - 76,
                    height: 112,
                    action: Action::Package(if vellum_ready {
                        remagic_protocol::PackageOperation::Refresh
                    } else {
                        remagic_protocol::PackageOperation::Bootstrap
                    }),
                });
            }

            let bottom_y = self.height - 150;
            self.round_rect(38, bottom_y, self.width - 76, 104, BLACK);
            self.text(
                font,
                "休眠",
                38.0,
                self.width / 2 - 46,
                bottom_y + 67,
                WHITE,
            );
            buttons.push(Button {
                x: 38,
                y: bottom_y,
                width: self.width - 76,
                height: 104,
                action: Action::Sleep,
            });
            unsafe {
                quill_swap_ex(0, 0, self.width, self.height, 4, 1, 1);
                quill_process_events();
            }
            buttons
        }

        fn card(&mut self, x: i32, y: i32, width: i32, height: i32) {
            self.round_rect(x, y, width, height, GRAY);
            self.hline(x + 18, x + width - 18, y + height - 1, 0xFFD2_D0CB);
        }

        fn flash(&mut self, button: &Button) {
            self.round_rect(button.x, button.y, button.width, button.height, BLACK);
            unsafe { quill_swap_ex(button.x, button.y, button.width, button.height, 4, 0, 1); }
            std::thread::sleep(Duration::from_millis(120));
        }

        fn fill(&mut self, color: u32) {
            for y in 0..self.height {
                for x in 0..self.width {
                    self.pixel(x, y, color);
                }
            }
        }

        fn round_rect(&mut self, x: i32, y: i32, width: i32, height: i32, color: u32) {
            let radius = 18;
            for py in y..y + height {
                for px in x..x + width {
                    let dx = if px < x + radius {
                        x + radius - px
                    } else if px >= x + width - radius {
                        px - (x + width - radius - 1)
                    } else {
                        0
                    };
                    let dy = if py < y + radius {
                        y + radius - py
                    } else if py >= y + height - radius {
                        py - (y + height - radius - 1)
                    } else {
                        0
                    };
                    if dx == 0 || dy == 0 || dx * dx + dy * dy <= radius * radius {
                        self.pixel(px, py, color);
                    }
                }
            }
        }

        fn hline(&mut self, x0: i32, x1: i32, y: i32, color: u32) {
            for x in x0..x1 {
                self.pixel(x, y, color);
            }
        }

        fn text(
            &mut self,
            font: &FontArc,
            text: &str,
            size: f32,
            x: i32,
            baseline: i32,
            color: u32,
        ) {
            let scaled = font.as_scaled(PxScale::from(size));
            let mut caret = x as f32;
            let mut previous = None;
            for ch in text.chars() {
                let id = scaled.glyph_id(ch);
                if let Some(previous) = previous {
                    caret += scaled.kern(previous, id);
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

        fn pixel(&mut self, x: i32, y: i32, color: u32) {
            if x < 0 || y < 0 || x >= self.width || y >= self.height {
                return;
            }
            unsafe {
                let pointer =
                    self.buffer.add(y as usize * self.stride + x as usize * 4) as *mut u32;
                pointer.write_unaligned(color);
            }
        }
    }

    struct Touch {
        fd: RawFd,
        x: i32,
        y: i32,
        max_x: i32,
        max_y: i32,
        down: bool,
    }

    impl Touch {
        fn open() -> io::Result<Self> {
            for index in 0..32 {
                let name = fs::read_to_string(format!("/sys/class/input/event{index}/device/name"))
                    .unwrap_or_default()
                    .to_lowercase();
                if !name.contains("touch") {
                    continue;
                }
                let path = CString::new(format!("/dev/input/event{index}")).unwrap();
                let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
                if fd >= 0 {
                    let (max_x, max_y) = (abs_max(fd, 53).or_else(|| abs_max(fd, 0)).unwrap_or(6760),
                        abs_max(fd, 54).or_else(|| abs_max(fd, 1)).unwrap_or(11960));
                    return Ok(Self {
                        fd,
                        x: 0,
                        y: 0,
                        max_x,
                        max_y,
                        down: false,
                    });
                }
            }
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "touch input not found",
            ))
        }

        fn poll_tap(&mut self) -> Option<(i32, i32)> {
            const EV_ABS: u16 = 3;
            const EV_KEY: u16 = 1;
            const ABS_X: u16 = 0;
            const ABS_Y: u16 = 1;
            const ABS_MT_POSITION_X: u16 = 53;
            const ABS_MT_POSITION_Y: u16 = 54;
            const ABS_MT_TRACKING_ID: u16 = 57;
            let event_size = std::mem::size_of::<libc::timeval>() + 8;
            let mut buffer = [0u8; 24 * 32];
            let mut released = None;
            loop {
                let count = unsafe {
                    libc::read(
                        self.fd,
                        buffer.as_mut_ptr().cast::<libc::c_void>(),
                        buffer.len(),
                    )
                };
                if count <= 0 {
                    break;
                }
                for event in buffer[..count as usize].chunks_exact(event_size) {
                    let offset = event_size - 8;
                    let kind = u16::from_ne_bytes([event[offset], event[offset + 1]]);
                    let code = u16::from_ne_bytes([event[offset + 2], event[offset + 3]]);
                    let value =
                        i32::from_ne_bytes(event[offset + 4..offset + 8].try_into().unwrap());
                    if kind == EV_ABS {
                        match code {
                            ABS_X | ABS_MT_POSITION_X => self.x = value.clamp(0, self.max_x) * 960 / self.max_x,
                            ABS_Y | ABS_MT_POSITION_Y => self.y = value.clamp(0, self.max_y) * 1696 / self.max_y,
                            ABS_MT_TRACKING_ID if value >= 0 => self.down = true,
                            ABS_MT_TRACKING_ID => {
                                if self.down { released = Some((self.x, self.y)); }
                                self.down = false;
                            }
                            _ => {}
                        }
                    } else if kind == EV_KEY && code == 330 {
                        // BTN_TOUCH is used by single-finger capacitive taps.
                        if value != 0 { self.down = true; } else if self.down {
                            released = Some((self.x, self.y));
                            self.down = false;
                        }
                    }
                }
            }
            released
        }
    }

    fn abs_max(fd: RawFd, axis: u16) -> Option<i32> {
        // EVIOCGABS(axis): input_absinfo { value, min, max, fuzz, flat, resolution }.
        let request = 0x8018_4540u64 + axis as u64;
        let mut info = [0i32; 6];
        let rc = unsafe { libc::ioctl(fd, request as libc::c_ulong, info.as_mut_ptr()) };
        (rc == 0 && info[2] > 0).then_some(info[2])
    }

    impl Drop for Touch {
        fn drop(&mut self) {
            unsafe { libc::close(self.fd) };
        }
    }
}
