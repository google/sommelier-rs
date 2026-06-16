use eframe::egui;
use egui::{Event, ImeEvent, Ui};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    log::info!("Starting Sommelier IME Test Application...");

    let auto_exit = std::env::args().any(|a| a == "--auto-exit");

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Sommelier IME Test",
        options,
        Box::new(|_cc| Ok(Box::new(App { text: String::new(), auto_exit, started: std::time::Instant::now() }))),
    )?;

    Ok(())
}

struct App {
    text: String,
    auto_exit: bool,
    started: std::time::Instant,
}

impl App {
    fn log_event(event: &Event) {
        match event {
            Event::Ime(ime) => match ime {
                ImeEvent::Enabled => log::info!("[IME] Enabled"),
                ImeEvent::Preedit(text) => log::info!("[IME] Preedit: {:?}", text),
                ImeEvent::Commit(text) => log::info!("[IME] Commit: {:?}", text),
                ImeEvent::Disabled => log::info!("[IME] Disabled"),
            },
            Event::Text(text) => log::info!("[Text] {:?}", text),
            Event::Key { key, physical_key, pressed, repeat, modifiers } => {
                log::info!("[Key] key={:?} physical={:?} pressed={} repeat={} mods={:?}",
                    key, physical_key, pressed, repeat, modifiers);
            }
            Event::Copy => log::info!("[Clipboard] Copy"),
            Event::Cut => log::info!("[Clipboard] Cut"),
            Event::Paste(text) => log::info!("[Clipboard] Paste: {:?}", text),
            Event::PointerMoved(pos) => log::trace!("[Pointer] Moved: {:?}", pos),
            Event::PointerButton { pos, button, pressed, .. } => {
                log::info!("[Pointer] Button={:?} pos={:?} pressed={}", button, pos, pressed);
            }
            Event::PointerGone => log::trace!("[Pointer] Gone"),
            Event::WindowFocused(focused) => log::info!("[Focus] WindowFocused={}", focused),
            Event::MouseWheel { delta, .. } => log::info!("[Scroll] delta={:?}", delta),
            _ => log::trace!("[Event] {:?}", event),
        }
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        for event in &raw_input.events {
            Self::log_event(event);
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        ui.heading("Sommelier IME Test");
        ui.add_space(8.0);

        let te = egui::TextEdit::multiline(&mut self.text)
            .desired_rows(10)
            .desired_width(f32::INFINITY);
        te.show(ui);

        if self.auto_exit && self.started.elapsed() > std::time::Duration::from_secs(2) {
            log::info!("Auto-exit timeout reached. Exiting application.");
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn on_exit(&mut self) {
        log::info!("[OnExit] Application closing. Final text buffer: {:?}", self.text);
    }
}
