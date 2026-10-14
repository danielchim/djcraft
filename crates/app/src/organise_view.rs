//! Dialogs for organising the library: naming things, smart playlist rules,
//! and finding moved files.

use std::path::PathBuf;

use djcraft_library::{Camelot, Rule, SmartRules, TextField, TextOp, Track};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu, PopupMenuItem},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// What one of these dialogs asks the library view to do.
pub enum OrganiseEvent {
    /// A name was entered.
    Named(String),
    /// A smart playlist was set up: its name and rules.
    Smart(String, SmartRules),
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

/// The kinds of rule a smart playlist can have, in the order offered.
#[derive(Clone, Copy, PartialEq)]
enum RuleKind {
    Text(TextField),
    Bpm,
    Energy,
    Year,
    Key,
    Added,
}

impl RuleKind {
    const ALL: [RuleKind; 9] = [
        RuleKind::Text(TextField::Title),
        RuleKind::Text(TextField::Artist),
        RuleKind::Text(TextField::Album),
        RuleKind::Text(TextField::Genre),
        RuleKind::Bpm,
        RuleKind::Energy,
        RuleKind::Year,
        RuleKind::Key,
        RuleKind::Added,
    ];

    fn label(self) -> &'static str {
        match self {
            RuleKind::Text(field) => field.label(),
            RuleKind::Bpm => "BPM",
            RuleKind::Energy => "Energy",
            RuleKind::Year => "Year",
            RuleKind::Key => "Key",
            RuleKind::Added => "Added",
        }
    }

    /// Placeholders for the rule's two text boxes; an empty one is not shown.
    fn hints(self) -> [&'static str; 2] {
        match self {
            RuleKind::Text(_) => ["Text", ""],
            RuleKind::Bpm => ["From, such as 122", "To, such as 128"],
            RuleKind::Energy => ["From 1", "To 10"],
            RuleKind::Year => ["From, such as 1990", "To, such as 1999"],
            RuleKind::Key => ["Key, such as 8A or Am", ""],
            RuleKind::Added => ["Days, such as 30", ""],
        }
    }
}

/// One rule as it is being edited.
struct RuleRow {
    kind: RuleKind,
    op: TextOp,
    inputs: [Entity<InputState>; 2],
    compatible: bool,
}

impl RuleRow {
    fn new(kind: RuleKind, window: &mut Window, cx: &mut App) -> Self {
        let inputs = kind
            .hints()
            .map(|hint| cx.new(|cx| InputState::new(window, cx).placeholder(hint)));
        Self {
            kind,
            op: TextOp::Contains,
            inputs,
            compatible: true,
        }
    }

    fn from_rule(rule: &Rule, window: &mut Window, cx: &mut App) -> Self {
        let (kind, values) = match rule {
            Rule::Text { field, value, .. } => {
                (RuleKind::Text(*field), [value.clone(), String::new()])
            }
            Rule::Bpm { min, max } => (RuleKind::Bpm, [trim(*min), trim(*max)]),
            Rule::Energy { min, max } => (RuleKind::Energy, [min.to_string(), max.to_string()]),
            Rule::Year { min, max } => (RuleKind::Year, [min.to_string(), max.to_string()]),
            Rule::Key { key, .. } => (RuleKind::Key, [key.to_string(), String::new()]),
            Rule::AddedWithinDays { days } => (RuleKind::Added, [days.to_string(), String::new()]),
        };
        let mut row = Self::new(kind, window, cx);
        for (input, value) in row.inputs.iter().zip(values) {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        if let Rule::Text { op, .. } = rule {
            row.op = *op;
        }
        if let Rule::Key { compatible, .. } = rule {
            row.compatible = *compatible;
        }
        row
    }

    /// The rule this row describes, or `None` while it is incomplete.
    fn rule(&self, cx: &App) -> Option<Rule> {
        let [a, b] = [0, 1].map(|i| self.inputs[i].read(cx).value().trim().to_string());
        let number = |text: &str| text.parse::<f64>().ok();
        // Either end may be left empty, but not both.
        let range = |low: f64, high: f64| {
            if a.is_empty() && b.is_empty() {
                return None;
            }
            let from = number(&a).unwrap_or(low);
            let to = number(&b).unwrap_or(high);
            Some((from.min(to), from.max(to)))
        };
        Some(match self.kind {
            RuleKind::Text(field) => Rule::Text {
                field,
                op: self.op,
                value: Some(a).filter(|a| !a.is_empty())?,
            },
            RuleKind::Bpm => {
                let (min, max) = range(0.0, 999.0)?;
                Rule::Bpm { min, max }
            }
            RuleKind::Energy => {
                let (min, max) = range(1.0, 10.0)?;
                Rule::Energy {
                    min: min as u8,
                    max: max as u8,
                }
            }
            RuleKind::Year => {
                let (min, max) = range(0.0, 9999.0)?;
                Rule::Year {
                    min: min as i32,
                    max: max as i32,
                }
            }
            RuleKind::Key => Rule::Key {
                key: Camelot::parse(&a)?,
                compatible: self.compatible,
            },
            RuleKind::Added => Rule::AddedWithinDays {
                days: a.parse().ok()?,
            },
        })
    }
}

/// A number without needless decimals: 124 rather than 124.0.
fn trim(value: f64) -> String {
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Sets up a smart playlist: its name, whether a track must pass every rule
/// or any one, and the rules.
pub struct SmartForm {
    name: Entity<InputState>,
    match_all: bool,
    rows: Vec<RuleRow>,
}

impl SmartForm {
    pub fn new(
        name: &str,
        rules: Option<&SmartRules>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial = name.to_string();
        let name = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Name");
            input.set_value(initial, window, cx);
            input
        });
        let rows = match rules {
            Some(rules) => rules
                .rules
                .iter()
                .map(|rule| RuleRow::from_rule(rule, window, cx))
                .collect(),
            None => vec![RuleRow::new(RuleKind::Text(TextField::Genre), window, cx)],
        };
        Self {
            name,
            match_all: rules.is_none_or(|rules| rules.match_all),
            rows,
        }
    }

    fn rules(&self, cx: &App) -> Option<SmartRules> {
        let rules: Option<Vec<Rule>> = self.rows.iter().map(|row| row.rule(cx)).collect();
        Some(SmartRules {
            match_all: self.match_all,
            rules: rules.filter(|rules| !rules.is_empty())?,
        })
    }

    fn submit(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        if let Some(rules) = self.rules(cx).filter(|_| !name.is_empty()) {
            cx.emit(OrganiseEvent::Smart(name, rules));
        }
    }

    fn choice<T: Copy + PartialEq + 'static>(
        &self,
        id: impl Into<ElementId>,
        current: T,
        choices: Vec<T>,
        label: fn(T) -> &'static str,
        set: impl Fn(&mut Self, T, &mut Window, &mut Context<Self>) + Clone + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.weak_entity();
        Button::new(id)
            .outline()
            .small()
            .label(label(current))
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for choice in choices.clone() {
                    let (view, set) = (view.clone(), set.clone());
                    menu = menu.item(
                        PopupMenuItem::new(label(choice))
                            .checked(choice == current)
                            .on_click(move |_, window, cx| {
                                view.update(cx, |view, cx| {
                                    set(view, choice, window, cx);
                                    cx.notify();
                                })
                                .ok();
                            }),
                    );
                }
                menu
            })
    }
}

impl EventEmitter<OrganiseEvent> for SmartForm {}

impl Render for SmartForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let ready = !self.name.read(cx).value().trim().is_empty() && self.rules(cx).is_some();
        let match_label = |all: bool| {
            if all { "every rule" } else { "any rule" }
        };
        let rows: Vec<AnyElement> = (0..self.rows.len())
            .map(|index| {
                let row = &self.rows[index];
                let kind = self.choice(
                    ("rule-kind", index),
                    row.kind,
                    RuleKind::ALL.to_vec(),
                    RuleKind::label,
                    move |this: &mut Self, kind, window, cx| {
                        if this.rows[index].kind != kind {
                            this.rows[index] = RuleRow::new(kind, window, cx);
                        }
                    },
                    cx,
                );
                let hints = row.kind.hints();
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(110.)).child(kind))
                    .when(matches!(row.kind, RuleKind::Text(_)), |this| {
                        this.child(self.choice(
                            ("rule-op", index),
                            row.op,
                            TextOp::ALL.to_vec(),
                            TextOp::label,
                            move |this: &mut Self, op, _, _| this.rows[index].op = op,
                            cx,
                        ))
                    })
                    .child(div().flex_1().child(Input::new(&row.inputs[0]).small()))
                    .when(!hints[1].is_empty(), |this| {
                        this.child(div().flex_1().child(Input::new(&row.inputs[1]).small()))
                    })
                    .when(row.kind == RuleKind::Key, |this| {
                        this.child(
                            Checkbox::new(("rule-compatible", index))
                                .label("or keys that mix")
                                .checked(row.compatible)
                                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                    this.rows[index].compatible = *checked;
                                    cx.notify();
                                })),
                        )
                    })
                    .when(row.kind == RuleKind::Added, |this| {
                        this.child(div().text_sm().text_color(muted).child("days ago or less"))
                    })
                    .child(
                        Button::new(("rule-remove", index))
                            .ghost()
                            .small()
                            .icon(IconName::Minus)
                            .tooltip("Remove this rule")
                            .disabled(self.rows.len() == 1)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.rows.remove(index);
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(Input::new(&self.name))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .child("Tracks that pass")
                    .child(self.choice(
                        "match",
                        self.match_all,
                        vec![true, false],
                        match_label,
                        |this: &mut Self, all, _, _| this.match_all = all,
                        cx,
                    )),
            )
            .children(rows)
            .child(
                div().flex().child(
                    Button::new("rule-add")
                        .outline()
                        .small()
                        .icon(IconName::Plus)
                        .label("Add rule")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.rows.push(RuleRow::new(
                                RuleKind::Text(TextField::Artist),
                                window,
                                cx,
                            ));
                            cx.notify();
                        })),
                ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("The playlist updates by itself as the library changes."),
            )
            .child(footer("smart", "Save", ready, Self::submit, cx))
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
