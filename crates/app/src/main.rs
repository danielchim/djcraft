mod analysis;
mod flow_view;
mod library_view;
mod lookup_view;
mod settings;
mod settings_view;
mod shell;
mod waveform;

use gpui_kit::assets::AllAssets;
use gpui_kit::*;

actions!(
    djcraft,
    [Quit, ShowLibrary, ShowLive, ShowRadio, OpenSettings]
);

fn main() {
    gpui_kit::application().with_assets(AllAssets).run(|cx| {
        gpui_kit::init(cx);
        let settings = settings::Settings::load(&settings::data_dir());
        settings::move_old_waveforms();
        // Views read display settings, such as how tempos are shown, from here.
        cx.set_global(settings.clone());
        settings.apply_theme(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-1", ShowLibrary, None),
            KeyBinding::new("cmd-2", ShowLive, None),
            KeyBinding::new("cmd-3", ShowRadio, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new(
                "escape",
                settings_view::CloseSettings,
                Some(settings_view::KEY_CONTEXT),
            ),
        ]);

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
        gpui_kit::open_window(options, cx, |window, cx| {
            let shell = cx.new(|cx| shell::Shell::new(tracks, settings, window, cx));
            let show = |tab: shell::Tab| {
                let shell = shell.clone();
                move |cx: &mut App| shell.update(cx, |shell, cx| shell.show(tab, cx))
            };
            let (library, live, radio) = (
                show(shell::Tab::Library),
                show(shell::Tab::Live),
                show(shell::Tab::Radio),
            );
            cx.on_action(move |_: &ShowLibrary, cx| library(cx));
            cx.on_action(move |_: &ShowLive, cx| live(cx));
            cx.on_action(move |_: &ShowRadio, cx| radio(cx));
            let settings_shell = shell.clone();
            cx.on_action(move |_: &OpenSettings, cx| {
                settings_shell.update(cx, |shell, cx| shell.open_settings(cx))
            });
            shell
        })
        .expect("failed to open window");
        cx.activate(true);
    });
}
