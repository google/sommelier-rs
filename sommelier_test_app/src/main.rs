use eframe::egui;
use egui::Ui;

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

impl eframe::App for App {
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
}
