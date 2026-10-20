mod app;
mod handler;
mod i18n;
mod keymap;
mod packaging;
mod placement;
mod wire;

use app::App;

fn main() -> iced::Result {
    // Before anything that could panic: a crash leaves a report to find.
    wire::watch_for_crashes();
    iced::application(boot, App::update, App::view)
        .title("open-mtp-client")
        .theme(App::theme)
        .subscription(App::subscription)
        .window(wire::opening_window())
        .run()
}

fn boot() -> (App, iced::Task<app::Message>) {
    App::new(wire::initialize())
}
