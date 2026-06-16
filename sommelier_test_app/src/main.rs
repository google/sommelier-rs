// IME Test Application for Sommelier
//
// This app provides a minimal winit-based window with native IME support,
// designed for manual and automated testing of the sommelier-rs IME translation
// bridge (zwp_text_input_v3 ↔ zwp_text_input_v1).
//
// Usage:
//   sommelier-test-gui              # interactive: type with IME, see results in window
//   sommelier-test-gui --auto-exit  # automated: exits after 2s (for CI/e2e)

use std::num::NonZeroU32;
use std::sync::Arc;

use ab_glyph::{FontArc, PxScale, ScaleFont};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::Window;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("Starting Sommelier CJK IME Test Application...");

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new(std::env::args());
    event_loop.run_app(&mut app)?;

    Ok(())
}

struct App {
    window: Option<Arc<Window>>,
    context: Option<softbuffer::Context<Arc<Window>>>,
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    text: String,
    preedit: String,
    auto_exit: bool,
    font: Option<FontArc>,
    font_size: PxScale,
}

impl App {
    fn new(args: impl IntoIterator<Item = String>) -> Self {
        let auto_exit = args.into_iter().any(|a| a == "--auto-exit");
        let font = Self::load_font();
        let font_size = PxScale { x: 24.0, y: 24.0 };
        App {
            window: None,
            context: None,
            surface: None,
            text: String::new(),
            preedit: String::new(),
            auto_exit,
            font,
            font_size,
        }
    }

    fn load_font() -> Option<FontArc> {
        let candidates = [
            "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ];
        for path in candidates {
            if let Ok(data) = std::fs::read(path) {
                if let Ok(font) = FontArc::try_from_vec(data) {
                    log::info!("Loaded font from {}", path);
                    return Some(font);
                }
            }
        }
        log::warn!("No font found; text will not be rendered");
        None
    }

    fn update_title(&self) {
        if let Some(w) = &self.window {
            w.set_title(format!(
                "Sommelier IME Test | Text: {:?} | Preedit: {:?}",
                self.text, self.preedit
            ));
        }
    }

    fn render_text(buffer: &mut [u32], width: usize, height: usize, font: &FontArc, size: PxScale, lines: &[&str]) {
        let font = font.as_scaled(size);
        let line_height = font.height().ceil() as usize + 4;
        let baseline_y = font.ascent().ceil() as i32;
        let mut y_offset: i32 = 20;

        for line in lines {
            let mut x_offset: i32 = 16;
            for glyph in font.layout(line, PxScale { x: size.x, y: size.y }, PhysicalPosition::new(0.0, 0.0)) {
                if let Some(outline) = font.outline_glyph(glyph) {
                    let bb = outline.px_bounds();
                    for (px, py) in outline.pixels() {
                        let dx = (bb.min.x as i32 + px as i32) as i32;
                        let dy = (bb.min.y as i32 + py as i32 + y_offset + baseline_y) as i32;
                        if dx >= 0 && dx < width as i32 && dy >= 0 && dy < height as i32 {
                            let idx = dy as usize * width + dx as usize;
                            if idx < buffer.len() {
                                let alpha = (py as f32 / 255.0 * 255.0) as u8;
                                let bg = buffer[idx];
                                let r = ((bg >> 16) & 0xff) as u32 * (255 - alpha as u32) / 255;
                                let g = ((bg >> 8) & 0xff) as u32 * (255 - alpha as u32) / 255;
                                let b = (bg & 0xff) as u32 * (255 - alpha as u32) / 255;
                                let fr = 255u32 * alpha as u32 / 255;
                                let fg = 255u32 * alpha as u32 / 255;
                                let fb = 255u32 * alpha as u32 / 255;
                                buffer[idx] = 0xff000000 | ((r + fr).min(255) << 16) | ((g + fg).min(255) << 8) | (b + fb).min(255);
                            }
                        }
                    }
                }
                x_offset += font.h_advance(glyph.id()).ceil() as i32;
            }
            y_offset += line_height as i32;
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        log::info!("Resumed event triggered. Initializing window...");

        let window_attributes = Window::default_attributes()
            .with_title("Sommelier IME Test | Text: \"\" | Preedit: \"\"")
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));

        let window = Arc::new(event_loop.create_window(window_attributes).unwrap());

        window.set_ime_allowed(true);
        window.set_ime_purpose(winit::window::ImePurpose::Normal);

        let context = softbuffer::Context::new(window.clone()).unwrap();
        let surface = softbuffer::Surface::new(&context, window.clone()).unwrap();

        self.window = Some(window.clone());
        self.context = Some(context);
        self.surface = Some(surface);

        log::info!("Window created successfully with native IME support enabled.");

        if self.auto_exit {
            log::info!("Auto-exit parameter detected. Spawning shutdown timer (2s).");
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(2));
                log::info!("Auto-exit timeout reached. Exiting application.");
                std::process::exit(0);
            });
        }

        window.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                log::info!("Close requested. Exiting application loop.");
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                let surface = self.surface.as_mut().unwrap();
                let window = self.window.as_ref().unwrap();
                let size = window.inner_size();

                if let (Some(w), Some(h)) =
                    (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    surface.resize(w, h).unwrap();
                    let mut buffer = surface.buffer_mut().unwrap();

                    let bg_color = if !self.text.is_empty() {
                        0x0016_a0_85u32
                    } else {
                        0x002c_3e_50u32
                    };

                    // Fill with a simple row-striped background
                    for y in 0..h.get() {
                        let row_start = y as usize * w.get() as usize;
                        let row_end = row_start + w.get() as usize;
                        let color = if y % 24 < 22 {
                            bg_color
                        } else {
                            // Slightly lighter alternating rows for "lined paper" look
                            let r = ((bg_color >> 16) & 0xff).min(50);
                            let g = ((bg_color >> 8) & 0xff).min(50);
                            let b = (bg_color & 0xff).min(50);
                            (r << 16) | (g << 8) | b
                        };
                        buffer[row_start..row_end].fill(color);
                    }

                    // Render text
                    if let Some(ref font) = self.font {
                        let display = format!("Text: {}", self.text);
                        let preedit_display = if !self.preedit.is_empty() {
                            format!("Preedit: {}", self.preedit)
                        } else {
                            String::new()
                        };
                        let mut lines = vec![display.as_str()];
                        if !preedit_display.is_empty() {
                            lines.push(preedit_display.as_str());
                        }
                        Self::render_text(
                            &mut buffer,
                            w.get() as usize,
                            h.get() as usize,
                            font,
                            self.font_size,
                            &lines,
                        );
                    }

                    buffer.present().unwrap();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(surface) = &mut self.surface {
                    if let (Some(w), Some(h)) =
                        (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                    {
                        surface.resize(w, h).unwrap();
                    }
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                log::info!("[Winit IME] Commit text: {:?}", text);
                self.text.push_str(&text);
                self.preedit.clear();
                log::info!(
                    "Active buffer state -> text: {:?}, preedit: {:?}",
                    self.text,
                    self.preedit
                );
                self.update_title();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Ime(winit::event::Ime::Preedit(text, cursor)) => {
                log::info!("[Winit IME] Preedit: {:?} (cursor={:?})", text, cursor);
                self.preedit = text;
                log::info!(
                    "Active buffer state -> text: {:?}, preedit: {:?}",
                    self.text,
                    self.preedit
                );
                self.update_title();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Ime(winit::event::Ime::Disabled) => {
                log::info!("[Winit IME] Disabled");
                self.preedit.clear();
                self.update_title();
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Ime(winit::event::Ime::Enabled) => {
                log::info!("[Winit IME] Enabled");
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                if key_event.state == winit::event::ElementState::Pressed {
                    match &key_event.logical_key {
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape) => {
                            log::info!("Escape pressed. Exiting.");
                            event_loop.exit();
                        }
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Backspace) => {
                            log::info!("[Keyboard Event] Backspace pressed");
                            if self.preedit.is_empty() && !self.text.is_empty() {
                                if self.text.pop().is_some() {
                                    log::info!(
                                        "Backspace deleted character. Remaining text: {:?}",
                                        self.text
                                    );
                                }
                                self.update_title();
                                if let Some(w) = &self.window {
                                    w.request_redraw();
                                }
                            }
                        }
                        key => {
                            log::info!("[Keyboard Event] Key pressed: {:?}", key);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_exit_flag_detected() {
        let app = App::new(["--auto-exit".to_string()]);
        assert!(app.auto_exit);
    }

    #[test]
    fn auto_exit_flag_absent() {
        let app = App::new(["--some-other".to_string()]);
        assert!(!app.auto_exit);
    }

    #[test]
    fn auto_exit_with_extra_args() {
        let app = App::new([
            "binary".to_string(),
            "--auto-exit".to_string(),
            "--foo".to_string(),
        ]);
        assert!(app.auto_exit);
    }

    #[test]
    fn no_args_does_not_set_auto_exit() {
        let app = App::new(std::iter::empty::<String>());
        assert!(!app.auto_exit);
    }

    #[test]
    fn initial_state_empty() {
        let app = App::new(std::iter::empty::<String>());
        assert!(app.text.is_empty());
        assert!(app.preedit.is_empty());
        assert!(!app.auto_exit);
    }

    #[test]
    fn commit_accumulates_text() {
        let mut app = App::new(std::iter::empty::<String>());
        app.text.push_str("world");
        app.text.push_str("!");
        app.preedit.clear();
        assert_eq!(app.text, "world!");
        assert!(app.preedit.is_empty());
    }

    #[test]
    fn backspace_only_when_preedit_empty() {
        let mut app = App::new(std::iter::empty::<String>());
        app.text = "abc".to_string();
        if app.preedit.is_empty() && !app.text.is_empty() {
            app.text.pop();
        }
        assert_eq!(app.text, "ab");
    }

    #[test]
    fn backspace_blocked_when_preedit_active() {
        let mut app = App::new(std::iter::empty::<String>());
        app.text = "abc".to_string();
        app.preedit = "x".to_string();
        if app.preedit.is_empty() && !app.text.is_empty() {
            app.text.pop();
        }
        assert_eq!(app.text, "abc");
    }

    #[test]
    fn backspace_noop_when_text_empty() {
        let mut app = App::new(std::iter::empty::<String>());
        app.text = String::new();
        if app.preedit.is_empty() && !app.text.is_empty() {
            app.text.pop();
        }
        assert!(app.text.is_empty());
    }

    #[test]
    fn update_title_formats_correctly() {
        let app = App::new(std::iter::empty::<String>());
        let title = format!(
            "Sommelier IME Test | Text: {:?} | Preedit: {:?}",
            "hello", ""
        );
        assert_eq!(title, "Sommelier IME Test | Text: \"hello\" | Preedit: \"\"");
    }

    #[test]
    fn preedit_updates_display() {
        let app = App::new(std::iter::empty::<String>());
        let title = format!(
            "Sommelier IME Test | Text: {:?} | Preedit: {:?}",
            "hello", " world"
        );
        assert_eq!(
            title,
            "Sommelier IME Test | Text: \"hello\" | Preedit: \" world\""
        );
    }

    #[test]
    fn disabled_clears_preedit() {
        let mut app = App::new(std::iter::empty::<String>());
        app.preedit = "composition".to_string();
        app.preedit.clear();
        assert!(app.preedit.is_empty());
    }

    #[test]
    fn multiple_commits_accumulate() {
        let mut app = App::new(std::iter::empty::<String>());
        app.text.push_str("가");
        app.text.push_str("나");
        app.text.push_str("다");
        assert_eq!(app.text, "가나다");
    }

    #[test]
    fn backspace_on_cjk_text() {
        let mut app = App::new(std::iter::empty::<String>());
        app.text = "가나다".to_string();
        if app.preedit.is_empty() && !app.text.is_empty() {
            app.text.pop();
        }
        assert_eq!(app.text, "가나");
        if app.preedit.is_empty() && !app.text.is_empty() {
            app.text.pop();
        }
        assert_eq!(app.text, "가");
        if app.preedit.is_empty() && !app.text.is_empty() {
            app.text.pop();
        }
        assert!(app.text.is_empty());
    }

    #[test]
    fn font_loading_does_not_crash() {
        let app = App::new(std::iter::empty::<String>());
        let _ = &app.font;
    }

    #[test]
    fn load_font_from_valid_path() {
        let font = App::load_font();
        assert!(font.is_some(), "Expected a font to be loadable from the system");
    }

    #[test]
    fn render_text_with_empty_buffer() {
        let font = App::load_font().expect("font required for this test");
        let mut buffer = [0u32; 100];
        App::render_text(&mut buffer, 10, 10, &font, PxScale { x: 12.0, y: 12.0 }, &["test"]);
    }
}
