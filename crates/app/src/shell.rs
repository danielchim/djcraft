use djcraft_engine::{Engine, MAX_TEMPO, MIN_TEMPO};
use djcraft_protocol::{Action, DeckAction, DeckState, Role};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, TitleBar,
    avatar::{Avatar, AvatarGroup},
    button::{Button, ButtonRounded, ButtonVariants},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::waveform::waveform;

const TOP_BAR_HEIGHT: Pixels = px(52.);
const MAX_B2B_DJS: usize = 4;
const TEMPO_STEP: f64 = 0.01;

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Library,
    Live,
    Radio,
}

pub struct Shell {
    tab: Tab,
    is_live: bool,
    /// DJs in the current back-to-back session, host first.
    djs: Vec<SharedString>,
    engine: Option<Engine>,
    engine_error: Option<SharedString>,
}

impl Shell {
    /// Opens the audio engine and loads `tracks` onto the decks in order.
    pub fn new(tracks: Vec<String>) -> Self {
        let (engine, engine_error) = match Engine::new() {
            Ok(mut engine) => {
                if let Some(dir) = dirs::cache_dir() {
                    engine.set_cache_dir(dir.join("djcraft").join("waveforms"));
                }
                (Some(engine), None)
            }
            Err(error) => (None, Some(error.to_string().into())),
        };
        let mut shell = Self {
            tab: Tab::Live,
            is_live: false,
            djs: vec!["You".into()],
            engine,
            engine_error,
        };
        for (deck, path) in tracks.into_iter().enumerate() {
            shell.send(deck, DeckAction::Load { path });
        }
        shell
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

        let right = div()
            .flex()
            .items_center()
            .gap_2()
            .when(live, |this| {
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

        TitleBar::new()
            .h(TOP_BAR_HEIGHT)
            .child(
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
                self.tool("b2b-invite", IconName::UserPlus, "Invite a DJ to mix back-to-back")
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
            .on_click(cx.listener(move |this, _, _, cx| {
                this.tab = tab;
                cx.notify();
            }))
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
                    .child(self.tab_button(Tab::Library, "tab-library", IconName::Library, "Library", cx))
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
            .child(div().flex_1().min_h_0().when_some(track, |this, track| {
                this.child(waveform(
                    track.clone(),
                    state.position_seconds,
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
            Tab::Library => body.child(self.panel("Library", cx)),
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
                .child(self.panel("Track browser", cx)),
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
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.top_bar(cx))
            .child(self.content(&decks, cx))
            .child(self.bottom_tabs(cx))
    }
}
