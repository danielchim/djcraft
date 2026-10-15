//! Dialogs for changing tags: the review shown before any file is written,
//! and the small forms that set up find and replace and renaming.

use djcraft_library::{Cleanup, PlannedChange, TagField};
use gpui_kit::component::{
    ActiveTheme, Disableable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
};
use gpui_kit::*;

pub enum ReviewEvent {
    /// Write these changes, recorded under this label.
    Apply(String, Vec<PlannedChange>),
    Cancel,
}

/// Every change a tag tool would make, file by file, with a box to leave
/// any file out.
pub struct ReviewView {
    label: String,
    changes: Vec<PlannedChange>,
    include: Vec<bool>,
}

impl ReviewView {
    pub fn new(label: String, changes: Vec<PlannedChange>) -> Self {
        let include = vec![true; changes.len()];
        Self {
            label,
            changes,
            include,
        }
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        let chosen = self
            .changes
            .iter()
            .zip(&self.include)
            .filter(|(_, include)| **include)
            .map(|(change, _)| change.clone())
            .collect();
        cx.emit(ReviewEvent::Apply(self.label.clone(), chosen));
    }
}

impl EventEmitter<ReviewEvent> for ReviewView {}

impl Render for ReviewView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, border, danger) = (theme.muted_foreground, theme.border, theme.danger);
        let count = self.include.iter().filter(|include| **include).count();
        let files = self.changes.iter().enumerate().map(|(index, change)| {
            let name = change
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let rows = change
                .differences()
                .into_iter()
                .map(|(field, before, after)| {
                    let shown = |text: String| {
                        if text.is_empty() {
                            "—".to_string()
                        } else {
                            text
                        }
                    };
                    div()
                        .flex()
                        .gap_3()
                        .text_sm()
                        .child(div().w(px(80.)).text_color(muted).child(field))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(danger.opacity(0.85))
                                .line_through()
                                .child(shown(before)),
                        )
                        .child(div().text_color(muted).child("→"))
                        .child(div().flex_1().min_w_0().truncate().child(shown(after)))
                });
            div()
                .flex()
                .flex_col()
                .gap_1()
                .py_2()
                .border_b_1()
                .border_color(border)
                .child(
                    Checkbox::new(("review-file", index))
                        .label(name)
                        .checked(self.include[index])
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            this.include[index] = *checked;
                            cx.notify();
                        })),
                )
                .child(div().flex().flex_col().gap_1().pl_6().children(rows))
        });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child(format!(
                        "{} file{} will change. Untick any to leave alone. You can undo this from Tag tools.",
                        self.changes.len(),
                        if self.changes.len() == 1 { "" } else { "s" }
                    )),
            )
            .child(
                div()
                    .id("review-changes")
                    .flex()
                    .flex_col()
                    .max_h(px(420.))
                    .overflow_y_scroll()
                    .children(files),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("review-cancel")
                            .outline()
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ReviewEvent::Cancel))),
                    )
                    .child(
                        Button::new("review-apply")
                            .primary()
                            .label(match count {
                                1 => "Write 1 file".to_string(),
                                count => format!("Write {count} files"),
                            })
                            .disabled(count == 0)
                            .on_click(cx.listener(|this, _, _, cx| this.apply(cx))),
                    ),
            )
    }
}

pub enum FormEvent {
    /// Plan the clean-up and show the review.
    Replace(Cleanup),
    /// Plan renaming with this template and show the review.
    Rename(String),
    Cancel,
}

/// The form for find and replace.
pub struct ReplaceForm {
    find: Entity<InputState>,
    replace: Entity<InputState>,
    fields: [bool; 5],
    match_case: bool,
}

impl ReplaceForm {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let find = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
        let replace = cx.new(|cx| InputState::new(window, cx).placeholder("Replace with"));
        for input in [&find, &replace] {
            cx.subscribe_in(input, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit(cx);
                }
            })
            .detach();
        }
        find.update(cx, |input, cx| input.focus(window, cx));
        Self {
            find,
            replace,
            fields: [true, false, false, false, false],
            match_case: false,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let find = self.find.read(cx).value().to_string();
        if find.is_empty() || !self.fields.iter().any(|on| *on) {
            return;
        }
        let fields = TagField::ALL
            .into_iter()
            .zip(self.fields)
            .filter(|(_, on)| *on)
            .map(|(field, _)| field)
            .collect();
        cx.emit(FormEvent::Replace(Cleanup::Replace {
            find,
            replace: self.replace.read(cx).value().to_string(),
            fields,
            match_case: self.match_case,
        }));
    }
}

impl EventEmitter<FormEvent> for ReplaceForm {}

impl Render for ReplaceForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let ready = !self.find.read(cx).value().is_empty() && self.fields.iter().any(|on| *on);
        let fields = TagField::ALL.into_iter().enumerate().map(|(slot, field)| {
            Checkbox::new(("replace-field", slot))
                .label(field.label())
                .checked(self.fields[slot])
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    this.fields[slot] = *checked;
                    cx.notify();
                }))
        });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(Input::new(&self.find))
            .child(Input::new(&self.replace))
            .child(div().text_sm().text_color(muted).child("In"))
            .child(div().flex().flex_wrap().gap_4().children(fields))
            .child(
                Checkbox::new("replace-case")
                    .label("Match capitals")
                    .checked(self.match_case)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.match_case = *checked;
                        cx.notify();
                    })),
            )
            .child(form_buttons("replace", "Preview changes", ready, cx))
    }
}

/// The form for renaming files from their details.
pub struct RenameForm {
    template: Entity<InputState>,
}

impl RenameForm {
    pub const DEFAULT_TEMPLATE: &str = "{artist} - {title}";

    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let template = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder(Self::DEFAULT_TEMPLATE);
            input.set_value(Self::DEFAULT_TEMPLATE, window, cx);
            input
        });
        cx.subscribe_in(&template, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.submit(cx);
            }
        })
        .detach();
        Self { template }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let template = self.template.read(cx).value().trim().to_string();
        if !template.is_empty() {
            cx.emit(FormEvent::Rename(template));
        }
    }
}

impl EventEmitter<FormEvent> for RenameForm {}

impl Render for RenameForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let ready = !self.template.read(cx).value().trim().is_empty();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(Input::new(&self.template))
            .child(
                div().text_sm().text_color(muted).child(
                    "Details in braces are filled in: {artist}, {title}, {album}, {genre}, {bpm}, {key} and {energy}. Files stay in their folder and keep their extension.",
                ),
            )
            .child(form_buttons("rename", "Preview changes", ready, cx))
    }
}

/// Cancel and a primary button, for a form that emits [`FormEvent`].
fn form_buttons<V>(id: &'static str, label: &'static str, ready: bool, cx: &mut Context<V>) -> Div
where
    V: EventEmitter<FormEvent> + FormSubmit,
{
    div()
        .flex()
        .justify_end()
        .gap_2()
        .child(
            Button::new((id, 0usize))
                .outline()
                .label("Cancel")
                .on_click(cx.listener(|_, _, _, cx| cx.emit(FormEvent::Cancel))),
        )
        .child(
            Button::new((id, 1usize))
                .primary()
                .label(label)
                .disabled(!ready)
                .on_click(cx.listener(|this, _, _, cx| this.submit_form(cx))),
        )
}

/// A form that can be sent from its primary button.
pub trait FormSubmit: Sized {
    fn submit_form(&mut self, cx: &mut Context<Self>);
}

impl FormSubmit for ReplaceForm {
    fn submit_form(&mut self, cx: &mut Context<Self>) {
        self.submit(cx);
    }
}

impl FormSubmit for RenameForm {
    fn submit_form(&mut self, cx: &mut Context<Self>) {
        self.submit(cx);
    }
}
