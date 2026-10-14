use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use djcraft_engine::{Engine, MAX_TEMPO, MIN_TEMPO};
use djcraft_library::TempoResult;
use djcraft_protocol::{Action, DeckAction, DeckState, Role};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, TitleBar,
    avatar::{Avatar, AvatarGroup},
    button::{Button, ButtonRounded, ButtonVariants},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::library_view::{LibraryEvent, LibraryView};
use crate::settings::{Settings, StartTab, data_dir};
use crate::settings_view::{SettingsEvent, SettingsView};
use crate::waveform::{overview, waveform};

const TOP_BAR_HEIGHT: Pixels = px(52.);
const MAX_B2B_DJS: usize = 4;
const TEMPO_STEP: f64 = 0.01;
const OVERVIEW_HEIGHT: f32 = 36.;

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    Library,
    Live,
    Radio,
}

pub struct Shell {
    tab: Tab,
    is_live: bool,
    /// DJs in the current back-to-back session, host first.
    djs: Vec<SharedString>,
    library: Entity<LibraryView>,
    /// The track browser under the decks.
    browser: Entity<LibraryView>,
    settings: Settings,
    /// The settings dialog, while it is open.
    settings_view: Option<Entity<SettingsView>>,
    /// Set when the dialog has just opened and should take keyboard focus.
    focus_settings: bool,
    engine: Option<Engine>,
    engine_error: Option<SharedString>,
    /// The file last loaded onto each deck.
    deck_paths: Vec<Option<PathBuf>>,
    /// The beat grid of each deck's track, when it is a library track that
    /// has been analysed.
    grids: Vec<Option<TempoResult>>,
    /// Where each deck's whole-track waveform is on screen, for seeking.
    overview_areas: Vec<Rc<Cell<Bounds<Pixels>>>>,
}

impl Shell {
    /// Opens the audio engine and loads `tracks` onto the decks in order.
    pub fn new(
        tracks: Vec<String>,
        settings: Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (engine, engine_error) = match Engine::new() {
            Ok(mut engine) => {
                engine.set_cache_dir(crate::settings::waveform_dir());
                (Some(engine), None)
            }
            Err(error) => (None, Some(error.to_string().into())),
        };
        let scan = settings.scan_on_startup;
        let library = cx.new(|cx| LibraryView::new(false, scan, window, cx));
        library.update(cx, |view, cx| {
            view.set_provider(settings.info_provider, cx);
            view.set_watching(settings.watch_folders);
        });
        cx.subscribe(&library, |this, _, event: &LibraryEvent, cx| match event {
            LibraryEvent::Load { path, deck, .. } => {
                this.load_from_library(path, *deck);
                this.refresh_grids(cx);
                this.show(Tab::Live, cx);
            }
            LibraryEvent::Selected { .. } => {}
            // The browser under the decks reads the same database, and a
            // track on a deck may just have been analysed.
            LibraryEvent::Changed => {
                this.browser.update(cx, |view, cx| view.refresh(cx));
                this.refresh_grids(cx);
                cx.notify();
            }
        })
        .detach();
        let browser = cx.new(|cx| LibraryView::new(true, false, window, cx));
        cx.subscribe(&browser, |this, _, event: &LibraryEvent, cx| match event {
            LibraryEvent::Load { path, deck, .. } => {
                this.load_from_library(path, *deck);
                this.refresh_grids(cx);
                cx.notify();
            }
            LibraryEvent::Selected { .. } | LibraryEvent::Changed => {}
        })
        .detach();
        let mut shell = Self {
            // Tracks named on the command line go to the decks, so show them.
            tab: match settings.start_tab {
                _ if !tracks.is_empty() => Tab::Live,
                StartTab::Library => Tab::Library,
                StartTab::Live => Tab::Live,
                StartTab::Radio => Tab::Radio,
            },
            is_live: false,
            djs: vec!["You".into()],
            library,
            browser,
            settings,
            settings_view: None,
            focus_settings: false,
            engine,
            engine_error,
            deck_paths: vec![None; 2],
            grids: vec![None; 2],
            overview_areas: (0..2).map(|_| Rc::default()).collect(),
        };
        for (deck, path) in tracks.into_iter().enumerate() {
            shell.send(deck, DeckAction::Load { path });
        }
        shell.refresh_grids(cx);
        shell
    }

    /// Looks up the beat grid of each deck's track in the library.
    fn refresh_grids(&mut self, cx: &mut Context<Self>) {
        let library = self.library.read(cx);
        self.grids = self
            .deck_paths
            .iter()
            .map(|path| path.as_deref().and_then(|path| library.beat_grid_at(path)))
            .collect();
    }

    pub fn show(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        // The two library views share a database; whichever is coming into
        // view may be behind the other.
        match tab {
            Tab::Library => self.library.update(cx, |view, cx| view.refresh(cx)),
            Tab::Live => self.browser.update(cx, |view, cx| view.refresh(cx)),
            Tab::Radio => {}
        }
        if tab != Tab::Library {
            self.library.update(cx, |view, cx| view.pause_players(cx));
        }
        cx.notify();
    }

    pub fn open_settings(&mut self, cx: &mut Context<Self>) {
        if self.settings_view.is_some() {
            return;
        }
        let audio: SharedString = match (&self.engine, &self.engine_error) {
            (Some(engine), _) => format!("System default, {} Hz", engine.sample_rate()).into(),
            (None, Some(error)) => error.clone(),
            (None, None) => "Not available".into(),
        };
        let settings = self.settings.clone();
        let view = cx.new(|cx| SettingsView::new(settings, audio, cx));
        cx.subscribe(&view, |this, _, event: &SettingsEvent, cx| {
            match event {
                SettingsEvent::Changed(settings) => {
                    this.settings = settings.clone();
                    this.settings.apply_theme(cx);
                    cx.set_global(settings.clone());
                    this.library.update(cx, |view, cx| {
                        view.set_provider(settings.info_provider, cx);
                        view.apply_settings(settings, cx);
                    });
                    // How tempos are shown changes every view.
                    cx.refresh_windows();
                    if let Err(error) = this.settings.save(&data_dir()) {
                        eprintln!("could not save settings: {error}");
                    }
                }
                SettingsEvent::Closed => this.settings_view = None,
            }
            cx.notify();
        })
        .detach();
        self.settings_view = Some(view);
        self.focus_settings = true;
        cx.notify();
    }

    /// Loads a library track. With no deck named, it goes to the first one
    /// that is not playing; if every deck is playing, nothing is replaced.
    fn load_from_library(&mut self, path: &Path, deck: Option<usize>) {
        let Some(engine) = &mut self.engine else {
            return;
        };
        let free = || engine.state().decks.iter().position(|deck| !deck.playing);
        let Some(deck) = deck.or_else(free) else {
            return;
        };
        let path = path.to_string_lossy().into_owned();
        self.send(deck, DeckAction::Load { path });
    }

    /// Sends a deck action to the engine as the session host.
    fn send(&mut self, deck: usize, action: DeckAction) {
        let Some(engine) = &mut self.engine else {
            return;
        };
        let action = Action::Deck {
            deck: deck as u8,
            action,
        };
        if let Action::Deck {
            action: DeckAction::Load { path },
            ..
        } = &action
            && let Some(slot) = self.deck_paths.get_mut(deck)
        {
            *slot = Some(PathBuf::from(path));
        }
        if let Err(reason) = engine.apply(Role::Host, action) {
            eprintln!("action rejected: {reason:?}");
        }
    }

    fn pill(&self, cx: &Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .gap_1()
            .px_1()
            .h(px(36.))
            .rounded_full()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
    }

    fn tool(&self, id: impl Into<ElementId>, icon: IconName, tooltip: &'static str) -> Button {
        Button::new(id)
            .ghost()
            .small()
            .rounded(ButtonRounded::Size(px(999.)))
            .icon(icon)
            .tooltip(tooltip)
    }

    fn top_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let live = self.tab == Tab::Live;

        let left = div()
            .flex()
            .items_center()
            .gap_2()
            .ml_3()
            .when(live, |this| {
                this.child(
                    self.pill(cx)
                        .child(self.tool("master-volume", IconName::Volume2, "Master volume"))
                        .child(self.tool("record", IconName::CircleDot, "Record mix")),
                )
                .child(
                    self.pill(cx)
                        .child(self.tool("automix", IconName::Repeat, "Automix"))
                        .child(self.tool("fx", IconName::SlidersHorizontal, "FX"))
                        .child(self.tool("stems", IconName::AudioLines, "Stems"))
                        .child(self.tool("sampler", IconName::LayoutGrid, "Sampler")),
                )
            });

        let right = div().flex().items_center().gap_2().when(live, |this| {
            this.child(self.b2b(cx))
                .child(
                    Button::new("share")
                        .outline()
                        .rounded(ButtonRounded::Size(px(999.)))
                        .icon(IconName::Share2)
                        .label("Share"),
                )
                .child(self.go_live(cx))
                .child(
                    Button::new("deck-mode")
                        .outline()
                        .rounded(ButtonRounded::Size(px(999.)))
                        .icon(IconName::Columns2)
                        .label("Two Decks")
                        .dropdown_caret(true),
                )
        });
        let right = right.child(
            self.tool("settings", IconName::Settings, "Settings")
                .on_click(cx.listener(|this, _, _, cx| this.open_settings(cx))),
        );

        TitleBar::new().h(TOP_BAR_HEIGHT).child(
            div()
                .flex()
                .flex_1()
                .items_center()
                .justify_between()
                .pr_3()
                .child(div().flex().flex_1().child(left))
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::BOLD)
                        .child("djcraft"),
                )
                .child(div().flex().flex_1().justify_end().child(right)),
        )
    }

    fn b2b(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let full = self.djs.len() >= MAX_B2B_DJS;
        self.pill(cx)
            .pl_2()
            .gap_2()
            .child(
                AvatarGroup::new()
                    .small()
                    .children(self.djs.iter().map(|name| Avatar::new().name(name.clone()))),
            )
            .child(
                self.tool(
                    "b2b-invite",
                    IconName::UserPlus,
                    "Invite a DJ to mix back-to-back",
                )
                .disabled(full)
                .on_click(cx.listener(|this, _, _, cx| {
                    if this.djs.len() < MAX_B2B_DJS {
                        let n = this.djs.len() + 1;
                        this.djs.push(format!("DJ {n}").into());
                        cx.notify();
                    }
                })),
            )
    }

    fn go_live(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let button = Button::new("go-live")
            .rounded(ButtonRounded::Size(px(999.)))
            .icon(IconName::RadioTower);
        let button = if self.is_live {
            button.danger().label("Live · End")
        } else {
            button.primary().label("Go Live")
        };
        button.on_click(cx.listener(|this, _, _, cx| {
            this.is_live = !this.is_live;
            cx.notify();
        }))
    }

    fn tab_button(
        &self,
        tab: Tab,
        id: &'static str,
        icon: IconName,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Button::new(id)
            .ghost()
            .rounded(ButtonRounded::Size(px(999.)))
            .icon(icon)
            .label(label)
            .selected(self.tab == tab)
            .on_click(cx.listener(move |this, _, _, cx| this.show(tab, cx)))
    }

    fn bottom_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .justify_center()
            .py_2()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                self.pill(cx)
                    .h(px(44.))
                    .child(self.tab_button(
                        Tab::Library,
                        "tab-library",
                        IconName::Library,
                        "Library",
                        cx,
                    ))
                    .child(self.tab_button(Tab::Live, "tab-live", IconName::Disc3, "Live", cx))
                    .child(self.tab_button(Tab::Radio, "tab-radio", IconName::Radio, "Radio", cx)),
            )
    }

    fn panel(&self, label: impl Into<SharedString>, cx: &Context<Self>) -> Div {
        div()
            .flex()
            .flex_1()
            .p_4()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .text_color(cx.theme().muted_foreground)
            .child(label.into())
    }

    fn deck(&self, index: usize, state: &DeckState, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (title, time): (SharedString, String) = match (&state.track, &state.error) {
            _ if state.loading => ("Loading…".into(), String::new()),
            (_, Some(error)) => (error.clone().into(), String::new()),
            (Some(track), _) => (
                track.title.clone().into(),
                format!(
                    "{} / {}",
                    clock(state.position_seconds),
                    clock(track.duration_seconds)
                ),
            ),
            (None, None) => ("No track loaded".into(), String::new()),
        };
        let track = self.engine.as_ref().and_then(|engine| engine.track(index));
        let grid = self.grids.get(index).copied().flatten();
        let area = self.overview_areas[index].clone();
        let duration = state
            .track
            .as_ref()
            .map_or(1., |track| track.duration_seconds.max(0.001));
        let tempo = state.tempo;
        let key_lock = state.key_lock;
        let playing = state.playing;

        let controls = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                Button::new(("load", index))
                    .outline()
                    .small()
                    .icon(IconName::FolderOpen)
                    .label("Load")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        let paths = cx.prompt_for_paths(PathPromptOptions {
                            files: true,
                            directories: false,
                            multiple: false,
                            prompt: Some("Load".into()),
                        });
                        cx.spawn(async move |this, cx| {
                            let Ok(Ok(Some(paths))) = paths.await else {
                                return;
                            };
                            let Some(path) = paths.first() else {
                                return;
                            };
                            let path = path.to_string_lossy().into_owned();
                            this.update(cx, |this, cx| {
                                this.send(index, DeckAction::Load { path });
                                this.refresh_grids(cx);
                                cx.notify();
                            })
                            .ok();
                        })
                        .detach();
                    })),
            )
            .child(
                Button::new(("play", index))
                    .primary()
                    .small()
                    .icon(if playing {
                        IconName::Pause
                    } else {
                        IconName::Play
                    })
                    .disabled(state.track.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let action = if playing {
                            DeckAction::Pause
                        } else {
                            DeckAction::Play
                        };
                        this.send(index, action);
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(
                self.tool(("tempo-down", index), IconName::Minus, "Slower")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let ratio = (tempo - TEMPO_STEP).max(MIN_TEMPO);
                        this.send(index, DeckAction::SetTempo { ratio });
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .w(px(56.))
                    .text_center()
                    .text_sm()
                    .child(format!("{:+.1}%", (tempo - 1.) * 100.)),
            )
            .child(
                self.tool(("tempo-up", index), IconName::Plus, "Faster")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let ratio = (tempo + TEMPO_STEP).min(MAX_TEMPO);
                        this.send(index, DeckAction::SetTempo { ratio });
                        cx.notify();
                    })),
            )
            .child(
                Button::new(("key-lock", index))
                    .outline()
                    .small()
                    .icon(IconName::Lock)
                    .label("Key lock")
                    .selected(key_lock)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.send(index, DeckAction::SetKeyLock { on: !key_lock });
                        cx.notify();
                    })),
            );

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap_3()
            .p_4()
            .border_1()
            .border_color(theme.border)
            .rounded_lg()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(format!("Deck {}", index + 1)),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .child(div().text_sm().child(time)),
            )
            .child(
                div()
                    .id(("overview", index))
                    .relative()
                    .h(px(OVERVIEW_HEIGHT))
                    .rounded_md()
                    .bg(theme.secondary.opacity(0.5))
                    .overflow_hidden()
                    .when_some(track, |this, track| {
                        this.child(overview(
                            track.clone(),
                            state.position_seconds / duration,
                            area.clone(),
                            theme.primary.opacity(0.5),
                            theme.muted_foreground,
                            theme.foreground,
                        ))
                    })
                    // A click anywhere along the whole track jumps there.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            let area = this.overview_areas[index].get();
                            let width = area.size.width.as_f32().max(1.);
                            let fraction =
                                ((event.position.x - area.origin.x).as_f32() / width).clamp(0., 1.);
                            this.send(
                                index,
                                DeckAction::Seek {
                                    seconds: fraction as f64 * duration,
                                },
                            );
                            cx.notify();
                        }),
                    ),
            )
            .child(div().flex_1().min_h_0().when_some(track, |this, track| {
                this.child(waveform(
                    track.clone(),
                    state.position_seconds,
                    grid,
                    theme.muted_foreground,
                    theme.primary,
                    theme.foreground,
                ))
            }))
            .child(controls)
    }

    fn content(&self, decks: &[DeckState], cx: &mut Context<Self>) -> Div {
        let body = div().flex().flex_col().flex_1().min_h_0().gap_3().p_3();
        match self.tab {
            Tab::Library => body.child(self.library.clone()),
            Tab::Live => body
                .when_some(self.engine_error.clone(), |this, error| {
                    this.child(div().text_color(cx.theme().danger).child(error))
                })
                .child(
                    div().flex().flex_1().min_h_0().gap_3().children(
                        decks
                            .iter()
                            .enumerate()
                            .map(|(index, state)| self.deck(index, state, cx))
                            .collect::<Vec<_>>(),
                    ),
                )
                .child(div().flex().flex_1().min_h_0().child(self.browser.clone())),
            Tab::Radio => body.child(self.panel("Radio: live mixes from other DJs", cx)),
        }
    }
}

fn clock(seconds: f64) -> String {
    let seconds = seconds.max(0.) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let decks = match &mut self.engine {
            Some(engine) => engine.state().decks,
            None => vec![DeckState::default(); 2],
        };
        // Positions and finished loads come from the engine, not from UI
        // events, so keep drawing while anything is moving.
        let moving = |deck: &DeckState| {
            deck.playing || deck.loading || deck.track.as_ref().is_some_and(|t| !t.fully_loaded)
        };
        if decks.iter().any(moving) {
            window.request_animation_frame();
        }
        if std::mem::take(&mut self.focus_settings)
            && let Some(view) = &self.settings_view
        {
            let handle = view.read(cx).focus_handle().clone();
            window.focus(&handle, cx);
        }
        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.top_bar(cx))
            .child(self.content(&decks, cx))
            .child(self.bottom_tabs(cx))
            .children(self.settings_view.clone())
    }
}
