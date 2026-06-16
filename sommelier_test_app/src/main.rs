use std::num::NonZeroU32;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::Window;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("Starting Sommelier CJK IME Test Application...");

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::default();
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
}

impl Default for App {
    fn default() -> Self {
        let args: Vec<String> = std::env::args().collect();
        let auto_exit = args.contains(&"--auto-exit".to_string());
        App {
            window: None,
            context: None,
            surface: None,
            text: String::new(),
            preedit: String::new(),
            auto_exit,
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
        
        // Enable winit's native IME processing
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

                if let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) {
                    surface.resize(w, h).unwrap();
                    let mut buffer = surface.buffer_mut().unwrap();

                    // Render background color depending on text state:
                    // Teal if text exists, dark slate otherwise.
                    let bg_color = if !self.text.is_empty() {
                        0x0016_a0_85
                    } else {
                        0x002c_3e_50
                    };

                    buffer.fill(bg_color);
                    buffer.present().unwrap();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(surface) = &mut self.surface {
                    if let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) {
                        surface.resize(w, h).unwrap();
                    }
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                log::info!("[Winit IME] Commit text: {:?}", text);
                self.text.push_str(&text);
                self.preedit.clear();
                log::info!("Active buffer state -> text: {:?}, preedit: {:?}", self.text, self.preedit);
                if let Some(w) = &self.window {
                    w.set_title(&format!("Sommelier IME Test | Text: {:?} | Preedit: {:?}", self.text, self.preedit));
                    w.request_redraw();
                }
            }
            WindowEvent::Ime(winit::event::Ime::Preedit(text, cursor)) => {
                log::info!("[Winit IME] Preedit: {:?} (cursor={:?})", text, cursor);
                self.preedit = text;
                log::info!("Active buffer state -> text: {:?}, preedit: {:?}", self.text, self.preedit);
                if let Some(w) = &self.window {
                    w.set_title(&format!("Sommelier IME Test | Text: {:?} | Preedit: {:?}", self.text, self.preedit));
                    w.request_redraw();
                }
            }
            WindowEvent::Ime(winit::event::Ime::Disabled) => {
                log::info!("[Winit IME] Disabled");
                self.preedit.clear();
                if let Some(w) = &self.window {
                    w.set_title(&format!("Sommelier IME Test | Text: {:?} | Preedit: {:?}", self.text, self.preedit));
                    w.request_redraw();
                }
            }
            WindowEvent::Ime(winit::event::Ime::Enabled) => {
                log::info!("[Winit IME] Enabled");
            }
            WindowEvent::KeyboardInput { event: key_event, .. } => {
                if key_event.state == winit::event::ElementState::Pressed {
                    match &key_event.logical_key {
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape) => {
                            log::info!("Escape pressed. Exiting.");
                            event_loop.exit();
                        }
                        winit::keyboard::Key::Named(winit::keyboard::NamedKey::Backspace) => {
                            log::info!("[Keyboard Event] Backspace pressed");
                            if self.preedit.is_empty() && !self.text.is_empty() {
                                if let Some(_) = self.text.pop() {
                                    log::info!("Backspace deleted character. Remaining text: {:?}", self.text);
                                }
                                if let Some(w) = &self.window {
                                    w.set_title(&format!("Sommelier IME Test | Text: {:?} | Preedit: {:?}", self.text, self.preedit));
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
