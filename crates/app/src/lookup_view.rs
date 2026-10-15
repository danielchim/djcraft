//! The Look up popup: searches an online database for the track in the info
//! panel, lists what it finds, and copies the chosen details back.

use std::path::{Path, PathBuf};

use djcraft_library::lookup::{self, Provider, TrackMatch};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu, PopupMenuItem},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// A detail that a lookup result can fill in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookupField {
    Title,
    Artist,
    Album,
    Genre,
}

impl LookupField {
    pub const ALL: [LookupField; 4] = [
        LookupField::Title,
        LookupField::Artist,
        LookupField::Album,
        LookupField::Genre,
    ];

    fn label(self) -> &'static str {
        match self {
            LookupField::Title => "Title",
            LookupField::Artist => "Artist",
            LookupField::Album => "Album",
            LookupField::Genre => "Genre",
        }
    }

    fn of(self, found: &TrackMatch) -> &str {
        match self {
            LookupField::Title => &found.title,
            LookupField::Artist => &found.artist,
            LookupField::Album => &found.album,
            LookupField::Genre => found.genre.as_deref().unwrap_or(""),
        }
    }
}

pub enum LookupEvent {
    /// Copy these values into the info panel. Nothing is saved yet.
    Apply(Vec<(LookupField, String)>),
    Cancel,
}

pub struct LookupView {
    artist: Entity<InputState>,
    title: Entity<InputState>,
    provider: Provider,
    /// The track's file, which AcoustID identifies by its sound.
    path: PathBuf,
    /// What the info panel holds now, in [`LookupField::ALL`] order.
    current: [String; 4],
    results: Vec<TrackMatch>,
    selected: Option<usize>,
    /// Which details of the selected result to copy, in
    /// [`LookupField::ALL`] order.
    chosen: [bool; 4],
    searching: bool,
    /// Counts searches, so that a slow reply to an old one is ignored.
    searches: u64,
    message: Option<SharedString>,
}

impl LookupView {
    /// Opens on `current` (title, artist, album, genre as the info panel
    /// has them) and starts searching straight away.
    pub fn new(
        current: [String; 4],
        provider: Provider,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut input = |text: &str, placeholder: &'static str, cx: &mut Context<Self>| {
            let text = text.to_string();
            cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder(placeholder);
                input.set_value(text, window, cx);
                input
            })
        };
        let artist = input(&current[1], "Artist", cx);
        let title = input(&current[0], "Title", cx);
        for field in [&artist, &title] {
            cx.subscribe_in(field, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.search(cx);
                }
            })
            .detach();
        }
        let mut view = Self {
            artist,
            title,
            provider,
            path,
            current,
            results: Vec::new(),
            selected: None,
            chosen: [false; 4],
            searching: false,
            searches: 0,
            message: None,
        };
        view.search(cx);
        view
    }

    fn search(&mut self, cx: &mut Context<Self>) {
        let artist = self.artist.read(cx).value().to_string();
        let title = self.title.read(cx).value().to_string();
        let provider = self.provider;
        let path = self.path.clone();
        let keys = cx
            .try_global::<crate::settings::Settings>()
            .map(|settings| settings.lookup_keys())
            .unwrap_or_default();
        self.searches += 1;
        let search = self.searches;
        self.searching = true;
        self.message = None;
        self.results.clear();
        self.selected = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    match provider {
                        Provider::AcoustId => identify_by_sound(&path, &keys.acoustid),
                        _ => lookup::search(provider, &artist, &title, &keys),
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if this.searches != search {
                    return;
                }
                this.searching = false;
                match result {
                    Ok(results) if results.is_empty() => {
                        this.message = Some(
                            format!(
                                "{} found nothing. Try a shorter title, or the other database.",
                                provider.label()
                            )
                            .into(),
                        );
                    }
                    Ok(results) => {
                        this.results = results;
                        this.select(0, cx);
                    }
                    Err(error) => this.message = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_provider(&mut self, provider: Provider, cx: &mut Context<Self>) {
        if provider != self.provider {
            self.provider = provider;
            self.search(cx);
        }
    }

    /// Picks a result. The details it would change are ticked; those it
    /// leaves as they are, or has nothing for, are not.
    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(found) = self.results.get(index) else {
            return;
        };
        self.selected = Some(index);
        for (slot, field) in LookupField::ALL.into_iter().enumerate() {
            let new = field.of(found).trim();
            self.chosen[slot] = !new.is_empty() && new != self.current[slot].trim();
        }
        cx.notify();
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        let Some(found) = self.selected.and_then(|index| self.results.get(index)) else {
            return;
        };
        let values = LookupField::ALL
            .into_iter()
            .zip(self.chosen)
            .filter(|(_, chosen)| *chosen)
            .map(|(field, _)| (field, field.of(found).trim().to_string()))
            .collect();
        cx.emit(LookupEvent::Apply(values));
    }

    fn provider_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        let current = self.provider;
        Button::new("lookup-provider")
            .outline()
            .label(current.label())
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for provider in Provider::ALL.into_iter().chain([Provider::AcoustId]) {
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(provider.label())
                            .checked(provider == current)
                            .on_click(move |_, _, cx| {
                                view.update(cx, |view, cx| view.set_provider(provider, cx))
                                    .ok();
                            }),
                    );
                }
                menu
            })
    }

    fn result_row(
        &self,
        index: usize,
        found: &TrackMatch,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        let selected = self.selected == Some(index);
        let detail = [
            Some(found.album.clone()).filter(|album| !album.is_empty()),
            found.year.clone(),
            found.genre.clone(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let badge = |text: String| {
            div()
                .px_1()
                .rounded_sm()
                .text_xs()
                .bg(theme.secondary)
                .text_color(theme.muted_foreground)
                .child(text)
        };
        div()
            .id(("lookup-result", index))
            .flex()
            .items_center()
            .gap_3()
            .px_2()
            .py_1p5()
            .rounded_md()
            .border_1()
            .border_color(if selected {
                theme.primary
            } else {
                theme.transparent
            })
            .when(selected, |this| this.bg(theme.secondary))
            .hover(|style| style.bg(theme.secondary))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .child(format!("{} — {}", found.artist, found.title)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(detail),
                    ),
            )
            .when(found.compilation, |this| {
                this.child(badge("Compilation".into()))
            })
            .child(badge(format!("{}% match", found.score)))
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.select(index, cx);
                // A double click takes the result as it is.
                if event.click_count() >= 2 {
                    this.apply(cx);
                }
            }))
    }

    /// For the selected result, each detail as it is and as it would be,
    /// with a box to say whether to take it.
    fn comparison(&self, cx: &mut Context<Self>) -> Option<Div> {
        let found = self.selected.and_then(|index| self.results.get(index))?;
        let theme = cx.theme();
        let (muted, border) = (theme.muted_foreground, theme.border);
        let rows = LookupField::ALL
            .into_iter()
            .enumerate()
            .map(|(slot, field)| {
                let (now, new) = (self.current[slot].trim(), field.of(found).trim());
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .text_sm()
                    .child(
                        div().w(px(110.)).child(
                            Checkbox::new(("lookup-use", slot))
                                .label(field.label())
                                .checked(self.chosen[slot])
                                .disabled(new.is_empty())
                                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                    this.chosen[slot] = *checked;
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(div().flex_1().min_w_0().truncate().text_color(muted).child(
                        if now.is_empty() {
                            "—".to_string()
                        } else {
                            now.to_string()
                        },
                    ))
                    .child(div().text_color(muted).child("→"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(if new.is_empty() {
                                "—".to_string()
                            } else {
                                new.to_string()
                            }),
                    )
            });
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .pt_3()
                .border_t_1()
                .border_color(border)
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("Tick the details to copy into the info panel. Press Save there to write them to the file."),
                )
                .children(rows),
        )
    }
}

/// AcoustID decodes at this rate itself, so there is no point in more.
const FINGERPRINT_RATE: u32 = 11_025;

/// Decodes a track, fingerprints its sound and asks AcoustID what it is.
fn identify_by_sound(path: &Path, key: &str) -> djcraft_library::Result<Vec<TrackMatch>> {
    if key.is_empty() {
        // Before decoding anything: say what is missing.
        return lookup::identify(key, "", 0);
    }
    let track = djcraft_engine::Track::decode(path, FINGERPRINT_RATE).map_err(|error| {
        djcraft_library::Error::Lookup(format!("The track could not be read: {error}"))
    })?;
    let print =
        djcraft_analysis::fingerprint(&track.mono(), FINGERPRINT_RATE).ok_or_else(|| {
            djcraft_library::Error::Lookup(
                "The track is too short to identify by its sound.".into(),
            )
        })?;
    lookup::identify(key, &print.encoded, print.duration_seconds)
}

impl EventEmitter<LookupEvent> for LookupView {}

impl Render for LookupView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let searching = self.searching;
        let can_apply = self.selected.is_some() && self.chosen.iter().any(|chosen| *chosen);
        let rows: Vec<_> = self
            .results
            .clone()
            .iter()
            .enumerate()
            .map(|(index, found)| self.result_row(index, found, cx))
            .collect();
        let comparison = self.comparison(cx);
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.artist)))
                    .child(div().flex_1().child(Input::new(&self.title)))
                    .child(self.provider_menu(cx))
                    .child(
                        Button::new("lookup-search")
                            .primary()
                            .icon(IconName::Search)
                            .label(if searching { "Searching…" } else { "Search" })
                            .disabled(searching)
                            .on_click(cx.listener(|this, _, _, cx| this.search(cx))),
                    ),
            )
            .child(
                div()
                    .id("lookup-results")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .h(px(280.))
                    .overflow_y_scroll()
                    .when(rows.is_empty(), |this| {
                        this.items_center()
                            .justify_center()
                            .text_sm()
                            .text_color(muted)
                            .child(match (&self.message, searching) {
                                (_, true) => SharedString::from("Searching…"),
                                (Some(message), false) => message.clone(),
                                (None, false) => "No results yet.".into(),
                            })
                    })
                    .children(rows),
            )
            .children(comparison)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("lookup-cancel")
                            .outline()
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(LookupEvent::Cancel))),
                    )
                    .child(
                        Button::new("lookup-apply")
                            .primary()
                            .label("Use these details")
                            .disabled(!can_apply)
                            .on_click(cx.listener(|this, _, _, cx| this.apply(cx))),
                    ),
            )
    }
}
