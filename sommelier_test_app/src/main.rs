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
        Box::new(|_cc| Ok(Box::new(App { text: String::new(), auto_exit, started: std::time::Instant::now(), has_focused: false }))),
    )?;

    Ok(())
}

struct App {
    text: String,
    auto_exit: bool,
    started: std::time::Instant,
    has_focused: bool,
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
        self.show_ui(ui);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::from_rgb(0.12, 0.12, 0.12).to_array()
    }

    fn on_exit(&mut self) {
        log::info!("[OnExit] Application closing. Final text buffer: {:?}", self.text);
    }
}

impl App {
    fn show_ui(&mut self, ui: &mut Ui) {
        ui.heading("Sommelier IME Test");
        ui.add_space(8.0);

        let text_edit_id = egui::Id::new("ime_text_field");
        let response = egui::TextEdit::multiline(&mut self.text)
            .id(text_edit_id)
            .desired_rows(10)
            .desired_width(f32::INFINITY)
            .show(ui);

        if !self.has_focused {
            ui.memory_mut(|mem| mem.request_focus(response.response.id));
            self.has_focused = true;
        }

        if self.auto_exit && self.started.elapsed() > std::time::Duration::from_secs(2) {
            log::info!("Auto-exit timeout reached. Exiting application.");
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::RawInput;

    fn setup_app() -> (egui::Context, App) {
        let ctx = egui::Context::default();
        ctx.options_mut(|o| o.max_passes = 1.try_into().unwrap());
        let app = App {
            text: String::new(),
            auto_exit: false,
            started: std::time::Instant::now(),
            has_focused: false,
        };
        (ctx, app)
    }

    fn run_frame(ctx: &egui::Context, app: &mut App, events: Vec<Event>) {
        let input = RawInput { events, ..Default::default() };
        let _ = ctx.run_ui(input, |ui| app.show_ui(ui));
    }

    #[test]
    fn test_ime_commit_appends_multiple_characters() {
        let (ctx, mut app) = setup_app();
        run_frame(&ctx, &mut app, vec![]);
        assert!(app.has_focused, "TextEdit should be focused after first frame");

        for i in 0..5 {
            run_frame(&ctx, &mut app, vec![
                Event::Ime(ImeEvent::Enabled),
                Event::Ime(ImeEvent::Commit("f".to_owned())),
            ]);
            assert_eq!(app.text.len(), i + 1, "After {} commit(s), got {:?}", i + 1, app.text);
        }

        assert_eq!(app.text, "fffff", "Expected 5 f's but got: {:?}", app.text);
    }

    #[test]
    fn test_ime_commit_with_preedit_appends() {
        let (ctx, mut app) = setup_app();
        run_frame(&ctx, &mut app, vec![]);

        run_frame(&ctx, &mut app, vec![
            Event::Ime(ImeEvent::Enabled),
            Event::Ime(ImeEvent::Preedit("한".to_owned())),
            Event::Ime(ImeEvent::Commit("한".to_owned())),
        ]);
        assert_eq!(app.text, "한", "Expected '한' but got: {:?}", app.text);
    }

    #[test]
    fn test_text_event_appends_multiple() {
        let (ctx, mut app) = setup_app();
        run_frame(&ctx, &mut app, vec![]);

        run_frame(&ctx, &mut app, vec![Event::Text("abc".to_owned())]);
        assert_eq!(app.text, "abc", "Expected 'abc' but got: {:?}", app.text);
    }

    #[test]
    fn test_key_events_append_text() {
        let (ctx, mut app) = setup_app();
        run_frame(&ctx, &mut app, vec![]);

        run_frame(&ctx, &mut app, vec![
            Event::Key { key: egui::Key::F, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::default() },
            Event::Text("f".to_owned()),
            Event::Key { key: egui::Key::O, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::default() },
            Event::Text("o".to_owned()),
            Event::Text("o".to_owned()),
        ]);
        assert_eq!(app.text, "foo", "Expected 'foo' but got: {:?}", app.text);
    }
}
