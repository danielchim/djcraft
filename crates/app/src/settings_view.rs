use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Icon, Sizable,
    button::{Button, ButtonVariants},
    menu::{DropdownMenu, PopupMenuItem},
    switch::Switch,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use djcraft_library::lookup::Provider;

use crate::settings::{Settings, StartTab, ThemeChoice, data_dir};

actions!(settings, [CloseSettings]);

/// The key context in which Escape closes the settings.
pub const KEY_CONTEXT: &str = "Settings";

pub enum SettingsEvent {
    /// A setting changed; the new values should be applied and saved.
    Changed(Settings),
    Closed,
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Preferences,
    Audio,
    Library,
    Assistant,
    Plugins,
    Marketplace,
    About,
}

impl Page {
    /// Pages grouped as they appear in the sidebar.
    const GROUPS: [(&'static str, &'static [Page]); 3] = [
        (
            "General",
            &[
                Page::Preferences,
                Page::Audio,
                Page::Library,
                Page::Assistant,
            ],
        ),
        ("Extensions", &[Page::Plugins, Page::Marketplace]),
        ("App", &[Page::About]),
    ];

    fn icon(self) -> IconName {
        match self {
            Page::Preferences => IconName::SlidersHorizontal,
            Page::Audio => IconName::Volume2,
            Page::Library => IconName::Library,
            Page::Assistant => IconName::Sparkles,
            Page::Plugins => IconName::Puzzle,
            Page::Marketplace => IconName::Store,
            Page::About => IconName::Info,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Page::Preferences => "Preferences",
            Page::Audio => "Audio",
            Page::Library => "Library",
            Page::Assistant => "Assistant",
            Page::Plugins => "Plugins",
            Page::Marketplace => "Marketplace",
            Page::About => "About",
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Page::Preferences => "How djcraft looks and starts on this computer.",
            Page::Audio => "Where the mix is played.",
            Page::Library => "How your music library is kept up to date.",
            Page::Assistant => "A helper for organising music and preparing sets.",
            Page::Plugins => "Extensions installed on this computer.",
            Page::Marketplace => "Where plugins are found and installed from.",
            Page::About => "This copy of djcraft.",
        }
    }
}

/// The settings dialog, drawn over the whole window.
pub struct SettingsView {
    settings: Settings,
    page: Page,
    /// A description of the audio output in use.
    audio: SharedString,
    focus: FocusHandle,
}

impl SettingsView {
    pub fn new(settings: Settings, audio: SharedString, cx: &mut Context<Self>) -> Self {
        Self {
            settings,
            page: Page::Preferences,
            audio,
            focus: cx.focus_handle(),
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    fn change(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        change(&mut self.settings);
        cx.emit(SettingsEvent::Changed(self.settings.clone()));
        cx.notify();
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (hover, muted) = (theme.secondary, theme.muted_foreground);
        div()
            .flex()
            .flex_col()
            .w(px(240.))
            .gap_4()
            .p_3()
            .border_r_1()
            .border_color(theme.border)
            .children(Page::GROUPS.map(|(group, pages)| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().px_2().pb_1().text_xs().text_color(muted).child(group))
                    .children(pages.iter().map(|&page| {
                        div()
                            .id(page.title())
                            .flex()
                            .items_center()
                            .gap_2()
                            .h(px(34.))
                            .px_2()
                            .rounded_md()
                            .text_sm()
                            .when(self.page == page, |this| {
                                this.bg(hover).font_weight(FontWeight::MEDIUM)
                            })
                            .hover(|style| style.bg(hover))
                            .child(Icon::new(page.icon()).small())
                            .child(page.title())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.page = page;
                                cx.notify();
                            }))
                    }))
            }))
    }

    /// A titled group of rows.
    fn section(&self, title: &'static str, rows: Vec<AnyElement>, cx: &Context<Self>) -> Div {
        let border = cx.theme().border;
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .pb_2()
                    .border_b_1()
                    .border_color(border)
                    .font_weight(FontWeight::BOLD)
                    .child(title),
            )
            .children(rows)
    }

    /// One setting: its name and explanation on the left, its control on
    /// the right.
    fn row(
        &self,
        name: &'static str,
        detail: impl Into<SharedString>,
        control: impl IntoElement,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        div()
            .flex()
            .items_center()
            .gap_4()
            .py_3()
            .border_b_1()
            .border_color(theme.border.opacity(0.5))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(name))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(detail.into()),
                    ),
            )
            .child(control)
            .into_any_element()
    }

    /// A dropdown showing `current`, offering every choice in `choices`.
    fn dropdown<T: Copy + PartialEq + 'static>(
        &self,
        id: &'static str,
        current: T,
        choices: &'static [T],
        label: fn(T) -> &'static str,
        set: fn(&mut Settings, T),
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<T> {
        let view = cx.weak_entity();
        Button::new(id)
            .outline()
            .w(px(200.))
            .label(label(current))
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for &choice in choices {
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label(choice))
                            .checked(choice == current)
                            .on_click(move |_, _, cx| {
                                view.update(cx, |view, cx| {
                                    view.change(|settings| set(settings, choice), cx)
                                })
                                .ok();
                            }),
                    );
                }
                menu
            })
    }

    fn info(&self, text: impl Into<SharedString>, cx: &Context<Self>) -> Div {
        div()
            .max_w(px(360.))
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text.into())
    }

    fn page(&self, cx: &mut Context<Self>) -> Vec<Div> {
        match self.page {
            Page::Preferences => {
                let theme = self.dropdown(
                    "theme",
                    self.settings.theme,
                    &ThemeChoice::ALL,
                    ThemeChoice::label,
                    |settings, theme| settings.theme = theme,
                    cx,
                );
                let start_tab = self.dropdown(
                    "start-tab",
                    self.settings.start_tab,
                    &StartTab::ALL,
                    StartTab::label,
                    |settings, tab| settings.start_tab = tab,
                    cx,
                );
                vec![
                    self.section(
                        "Appearance",
                        vec![self.row(
                            "Theme",
                            "Follow the system, or always use one look.",
                            theme,
                            cx,
                        )],
                        cx,
                    ),
                    self.section(
                        "Start-up",
                        vec![self.row(
                            "Open on",
                            "The tab shown when djcraft starts.",
                            start_tab,
                            cx,
                        )],
                        cx,
                    ),
                ]
            }
            Page::Audio => vec![self.section(
                "Output",
                vec![self.row(
                    "Output device",
                    "djcraft plays through the system's default output. Choosing a device is not available yet.",
                    self.info(self.audio.clone(), cx),
                    cx,
                )],
                cx,
            )],
            Page::Library => {
                let scan = Switch::new("scan-on-startup")
                    .checked(self.settings.scan_on_startup)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        let checked = *checked;
                        this.change(|settings| settings.scan_on_startup = checked, cx);
                    }));
                let provider = self.dropdown(
                    "info-provider",
                    self.settings.info_provider,
                    &Provider::ALL,
                    Provider::label,
                    |settings, provider| settings.info_provider = provider,
                    cx,
                );
                let music_info = self.section(
                    "Music info",
                    vec![self.row(
                        "Look up tracks with",
                        "The online database searched when you press Look up on a track. MusicBrainz covers more releases; the iTunes Store gives cleaner albums and a genre.",
                        provider,
                        cx,
                    )],
                    cx,
                );
                vec![self.section(
                    "Folders",
                    vec![
                        self.row(
                            "Scan folders at start-up",
                            "Look for new, changed and removed files each time djcraft opens.",
                            scan,
                            cx,
                        ),
                        self.row(
                            "Library location",
                            "Where the library database and settings are stored.",
                            self.info(data_dir().to_string_lossy().into_owned(), cx),
                            cx,
                        ),
                    ],
                    cx,
                ), music_info]
            }
            Page::Assistant => vec![self.section(
                "Assistant",
                vec![self.row(
                    "Not available yet",
                    "The assistant has not been built. Its settings will appear here.",
                    div(),
                    cx,
                )],
                cx,
            )],
            Page::Plugins => vec![self.section(
                "Installed plugins",
                vec![self.row(
                    "No plugins installed",
                    "djcraft cannot run plugins yet. Installed plugins and their permissions will be managed here.",
                    div(),
                    cx,
                )],
                cx,
            )],
            Page::Marketplace => vec![self.section(
                "Stores",
                vec![self.row(
                    "No marketplace connected",
                    "Plugin stores you subscribe to will be listed here once plugins can be installed.",
                    div(),
                    cx,
                )],
                cx,
            )],
            Page::About => vec![self.section(
                "djcraft",
                vec![
                    self.row(
                        "Version",
                        "The version of this copy.",
                        self.info(env!("CARGO_PKG_VERSION"), cx),
                        cx,
                    ),
                    self.row(
                        "Licence",
                        "djcraft is open-source software.",
                        self.info("MIT", cx),
                        cx,
                    ),
                ],
                cx,
            )],
        }
    }
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl Render for SettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = self.page;
        let sections = self.page(cx);
        let sidebar = self.sidebar(cx).into_any_element();
        let theme = cx.theme();
        let (background, border, muted) = (theme.background, theme.border, theme.muted_foreground);

        // The dimmed backdrop; clicking it closes the dialog.
        div()
            .id("settings-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .p_8()
            .bg(gpui_kit::black().opacity(0.6))
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &CloseSettings, _, cx| cx.emit(SettingsEvent::Closed)))
            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Closed)))
            .child(
                div()
                    .id("settings")
                    .flex()
                    .size_full()
                    .max_w(px(980.))
                    .max_h(px(660.))
                    .bg(background)
                    .border_1()
                    .border_color(border)
                    .rounded_xl()
                    .overflow_hidden()
                    .shadow_lg()
                    // Clicks inside the dialog are not clicks on the backdrop.
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(sidebar)
                    .child(
                        div()
                            .id("settings-page")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap_6()
                            .p_8()
                            .overflow_y_scroll()
                            .child(
                                div()
                                    .flex()
                                    .items_start()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .flex_1()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_2xl()
                                                    .font_weight(FontWeight::BOLD)
                                                    .child(page.title()),
                                            )
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(muted)
                                                    .child(page.summary()),
                                            ),
                                    )
                                    .child(
                                        Button::new("close-settings")
                                            .ghost()
                                            .icon(IconName::X)
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(SettingsEvent::Closed)
                                            })),
                                    ),
                            )
                            .children(sections),
                    ),
            )
    }
}
