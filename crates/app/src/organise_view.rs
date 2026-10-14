//! Dialogs for organising the library: naming things, and finding moved
//! files.

use std::path::PathBuf;

use djcraft_library::Track;
use gpui_kit::component::{
    ActiveTheme, Disableable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
};
use gpui_kit::*;

/// What one of these dialogs asks the library view to do.
pub enum OrganiseEvent {
    /// A name was entered.
    Named(String),
    /// Point each track at its file in a new place.
    Relocate(Vec<(i64, PathBuf)>),
    Close,
}

/// Cancel and a primary button, at the foot of a dialog.
fn footer<V: EventEmitter<OrganiseEvent>>(
    id: &'static str,
    label: impl Into<SharedString>,
    ready: bool,
    submit: fn(&mut V, &mut Window, &mut Context<V>),
    cx: &mut Context<V>,
) -> Div {
    div()
        .flex()
        .justify_end()
        .gap_2()
        .child(
            Button::new((id, 0usize))
                .outline()
                .label("Cancel")
                .on_click(cx.listener(|_, _, _, cx| cx.emit(OrganiseEvent::Close))),
        )
        .child(
            Button::new((id, 1usize))
                .primary()
                .label(label)
                .disabled(!ready)
                .on_click(cx.listener(move |this, _, window, cx| submit(this, window, cx))),
        )
}

/// Asks for a name, as for a new folder or a rename.
pub struct NameForm {
    name: Entity<InputState>,
    action: SharedString,
}

impl NameForm {
    pub fn new(
        initial: &str,
        action: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial = initial.to_string();
        let name = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Name");
            input.set_value(initial, window, cx);
            input
        });
        cx.subscribe_in(&name, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.submit(window, cx);
            }
        })
        .detach();
        name.update(cx, |input, cx| input.focus(window, cx));
        Self {
            name,
            action: action.into(),
        }
    }

    fn submit(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        if !name.is_empty() {
            cx.emit(OrganiseEvent::Named(name));
        }
    }
}

impl EventEmitter<OrganiseEvent> for NameForm {}

impl Render for NameForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = !self.name.read(cx).value().trim().is_empty();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(Input::new(&self.name))
            .child(footer("name", self.action.clone(), ready, Self::submit, cx))
    }
}

/// Files found for missing tracks, to confirm before relinking.
pub struct RelocateView {
    found: Vec<(Track, PathBuf)>,
    include: Vec<bool>,
}

impl RelocateView {
    pub fn new(found: Vec<(Track, PathBuf)>) -> Self {
        let include = vec![true; found.len()];
        Self { found, include }
    }

    fn submit(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let chosen = self
            .found
            .iter()
            .zip(&self.include)
            .filter(|(_, include)| **include)
            .map(|((track, path), _)| (track.id, path.clone()))
            .collect();
        cx.emit(OrganiseEvent::Relocate(chosen));
    }
}

impl EventEmitter<OrganiseEvent> for RelocateView {}

impl Render for RelocateView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let count = self.include.iter().filter(|include| **include).count();
        let rows: Vec<AnyElement> = self
            .found
            .iter()
            .enumerate()
            .map(|(index, (track, path))| {
                div()
                    .flex()
                    .flex_col()
                    .py_1()
                    .child(
                        Checkbox::new(("relocate", index))
                            .label(format!("{} — {}", track.artist, track.title))
                            .checked(self.include[index])
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                this.include[index] = *checked;
                                cx.notify();
                            })),
                    )
                    .child(div().pl_6().text_xs().text_color(muted).child(format!(
                        "{} → {}",
                        track.path.display(),
                        path.display()
                    )))
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_sm().text_color(muted).child(format!(
                "Found {} of the missing files, by name and size. Their tracks keep their playlists, cue points and analysis.",
                self.found.len()
            )))
            .child(
                div()
                    .id("relocate-list")
                    .flex()
                    .flex_col()
                    .max_h(px(400.))
                    .overflow_y_scroll()
                    .children(rows),
            )
            .child(footer(
                "relocate",
                format!("Relink {count}"),
                count > 0,
                Self::submit,
                cx,
            ))
    }
}
