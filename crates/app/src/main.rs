mod shell;
mod waveform;

use gpui_kit::assets::AllAssets;
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

actions!(djcraft, [Quit]);

fn main() {
    gpui_kit::application().with_assets(AllAssets).run(|cx| {
        gpui_kit::init(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);

        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(960.), px(600.))),
            titlebar: Some(TitlebarOptions {
                title: Some("djcraft".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(16.), px(18.))),
            }),
            ..Default::default()
        };
        // Tracks named on the command line are loaded onto the decks in order.
        let tracks = std::env::args().skip(1).collect();
        gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| shell::Shell::new(tracks)))
            .expect("failed to open window");
        cx.activate(true);
    });
}
