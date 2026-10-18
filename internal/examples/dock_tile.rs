//! Draws the Dock tile's progress bar over the app icon offscreen and writes
//! it to PNG files, to look at how it is drawn without touching the Dock:
//! `cargo run -p open-mtp-client-internal --example dock_tile -- <icon> <dir>`.

fn main() {
    let mut args = std::env::args().skip(1);
    let icon = args
        .next()
        .unwrap_or_else(|| "assets/icon-1024.png".to_owned());
    let dir = args.next().unwrap_or_else(|| ".".to_owned());
    for percent in [0, 42, 100] {
        let png = internal::service::local::dock_tile_png(&icon, percent)
            .expect("the tile could not be drawn");
        let path = format!("{dir}/dock-tile-{percent}.png");
        std::fs::write(&path, png).expect("the image could not be written");
        println!("{path}");
    }
}
