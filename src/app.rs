use adw::prelude::*;
pub fn run() -> gtk::glib::ExitCode {
    let app = adw::Application::builder()
        .application_id("io.github.efuseek.EfuSeek")
        .build();
    app.connect_activate(|app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        crate::ui::window::build(app);
    });
    app.run()
}
