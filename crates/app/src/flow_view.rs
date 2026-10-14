//! The Flow editor: a set planned on a timeline of lanes.
//!
//! This is for planning only. Nothing here plays audio; it shows what goes
//! where and how well neighbouring tracks should mix.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use djcraft_engine::Engine;
use djcraft_protocol::{Action, DeckAction, DeckState, Role};

use djcraft_library::{
    ClipPlacement, CuePoint, FLOW_LANES, Flow, FlowClip, Library, MAX_CLIP_SPEED, MIN_CLIP_SECONDS,
    MIN_CLIP_SPEED, TempoResult, Track,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, WindowExt,
    button::{Button, ButtonVariants},
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    notification::Notification,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::library_view::{DraggedTrack, LibraryEvent, LibraryView, key_colour, wheel_display};
use crate::waveform::overview;

const LANE_HEIGHT: f32 = 56.;
const RULER_HEIGHT: f32 = 22.;
const HINTS_HEIGHT: f32 = 24.;
/// How close, on screen, a moved clip's edge must come to another clip's
/// edge to snap to it.
const SNAP_PIXELS: f32 = 6.;
const MIN_ZOOM: f32 = 0.75;
const MAX_ZOOM: f32 = 12.;
/// Empty timeline kept after the last clip, so there is room to add more.
const TAIL_SECONDS: f64 = 240.;
/// How much a newly added track overlaps the one before it when its tempo
/// is not known. With a tempo, the overlap is `OVERLAP_BARS` bars.
const DEFAULT_OVERLAP_SECONDS: f64 = 30.;
const OVERLAP_BARS: f64 = 16.;
/// What a flow is called until it is given a name.
const UNTITLED: &str = "Untitled flow";
/// The most cue points a track may have.
const MAX_CUES: usize = 8;
const PLAYER_WAVE_HEIGHT: f32 = 72.;
/// Width of the grab area on each end of a block.
const HANDLE_WIDTH: f32 = 12.;
/// How close a stretched tempo must come to a neighbour's to lock onto it.
const TEMPO_MAGNET_BPM: f64 = 0.6;

/// Which part of a block was grabbed.
#[derive(Clone, Copy, PartialEq)]
enum Grip {
    Body,
    Start,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum DragMode {
    Move,
    /// Cutting: the edge moves through the track, which keeps its tempo.
    TrimStart,
    TrimEnd,
    /// Stretching: the same music is squeezed or spread, changing its tempo.
    StretchStart,
    StretchEnd,
}

/// What a held clip is measured against as it is dragged.
struct DragContext<'a> {
    track_seconds: f64,
    grid: Option<TempoResult>,
    cues: &'a [CuePoint],
    /// The track's own tempo, if known.
    bpm: Option<f64>,
    /// Start and end times of the other clips.
    edges: &'a [f64],
    /// Tempos of the clips this one overlaps.
    neighbour_bpms: &'a [f64],
    /// How close, in seconds of timeline, counts as near enough to snap.
    reach: f64,
}

/// A clip being moved, trimmed or stretched with the mouse.
struct ClipDrag {
    clip: i64,
    mode: DragMode,
    /// Timeline time under the pointer when the drag began.
    grabbed_at: f64,
    original: ClipPlacement,
}

pub struct FlowView {
    library: Option<Library>,
    error: Option<SharedString>,
    flows: Vec<Flow>,
    /// The saved flow open in the editor. `None` is a new flow, which is
    /// saved once it has a track or a name.
    flow: Option<i64>,
    clips: Vec<FlowClip>,
    selected: Option<i64>,
    drag: Option<ClipDrag>,
    /// Whether dragging a block's edge stretches it instead of cutting.
    stretch: bool,
    /// Pixels per second of timeline.
    zoom: f32,
    /// Where the lanes are on screen, as of the last frame.
    lanes: Rc<Cell<Bounds<Pixels>>>,
    /// The open flow's name.
    name: Entity<InputState>,
    /// A player of its own, for hearing one track. Opened on first use.
    player: Option<Engine>,
    /// The track in the player, with its cue points.
    playing: Option<Track>,
    cues: Vec<CuePoint>,
    /// Where the player's waveform is on screen, as of the last frame.
    wave: Rc<Cell<Bounds<Pixels>>>,
    /// Whether the player's key panel is showing.
    key_open: bool,
    /// When that panel was last closed by a click outside it.
    key_dismissed: Option<Instant>,
    /// Keyboard focus for the timeline, so Delete reaches it.
    focus: FocusHandle,
    browser: Entity<LibraryView>,
}

impl FlowView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let db_path = crate::settings::data_dir().join("library.db");
        let (library, error) = match Library::open(&db_path) {
            Ok(library) => (Some(library), None),
            Err(error) => (None, Some(error.to_string().into())),
        };
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(UNTITLED));
        cx.subscribe(&name, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.rename(cx);
            }
        })
        .detach();
        let browser = cx.new(|cx| LibraryView::new(true, false, window, cx));
        // The browser gets a button for the Camelot wheel: picking a key
        // lists the tracks that mix with it.
        browser.update(cx, |browser, cx| browser.show_wheel(cx));
        cx.subscribe(&browser, |this, _, event: &LibraryEvent, cx| match event {
            LibraryEvent::Load { id, .. } => this.append_track(*id, cx),
            LibraryEvent::Selected { id } => this.play_track(*id, cx),
            LibraryEvent::Changed => {}
        })
        .detach();

        let mut view = Self {
            library,
            error,
            flows: Vec::new(),
            flow: None,
            clips: Vec::new(),
            selected: None,
            drag: None,
            stretch: false,
            zoom: 3.,
            lanes: Rc::default(),
            name,
            player: None,
            playing: None,
            cues: Vec::new(),
            wave: Rc::default(),
            key_open: false,
            key_dismissed: None,
            focus: cx.focus_handle(),
            browser,
        };
        view.reload(cx);
        view
    }

    /// Re-reads the database, after another view may have changed it.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.reload(cx);
        self.browser.update(cx, |browser, cx| browser.refresh(cx));
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(library) = &self.library else {
            return;
        };
        let loaded = (|| {
            let flows = library.flows()?;
            let open = self
                .flow
                .filter(|id| flows.iter().any(|flow| flow.id == *id));
            let clips = match open {
                Some(id) => library.flow_clips(id)?,
                None => Vec::new(),
            };
            Ok::<_, djcraft_library::Error>((flows, open, clips))
        })();
        match loaded {
            Ok((flows, open, clips)) => {
                self.flows = flows;
                self.flow = open;
                self.clips = clips;
                if !self.clips.iter().any(|clip| Some(clip.id) == self.selected) {
                    self.selected = None;
                }
                // The player's cue points may have been edited or re-analysed.
                if let Some(track) = &self.playing {
                    self.cues = library.cues(track.id).unwrap_or_default();
                }
            }
            Err(error) => self.error = Some(error.to_string().into()),
        }
        cx.notify();
    }

    /// Runs a change against the library, reports any failure, and reloads.
    fn change(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&Library) -> djcraft_library::Result<()>,
    ) {
        if let Some(library) = &self.library
            && let Err(error) = change(library)
        {
            self.error = Some(error.to_string().into());
        }
        self.reload(cx);
    }

    /// The open flow, saving the new one first if that is what is open.
    fn ensure_flow(&mut self, cx: &mut Context<Self>) -> Option<i64> {
        if self.flow.is_none() {
            let typed = self.name.read(cx).value().trim().to_string();
            let name = if typed.is_empty() {
                UNTITLED.to_string()
            } else {
                typed
            };
            let mut created = None;
            self.change(cx, |library| {
                created = Some(library.create_flow(&name)?.id);
                Ok(())
            });
            self.flow = created;
        }
        self.flow
    }

    /// Gives the open flow the name in the name box.
    fn rename(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        if name.is_empty() {
            return;
        }
        match self.flow {
            Some(flow) => self.change(cx, |library| library.rename_flow(flow, &name)),
            None => {
                self.ensure_flow(cx);
                self.reload(cx);
            }
        }
    }

    /// Opens a saved flow, or a new one for `None`.
    fn open_flow(&mut self, flow: Option<i64>, window: &mut Window, cx: &mut Context<Self>) {
        self.flow = flow;
        self.selected = None;
        let name = self
            .flows
            .iter()
            .find(|saved| Some(saved.id) == flow)
            .map(|saved| saved.name.clone())
            .unwrap_or_default();
        self.name
            .update(cx, |input, cx| input.set_value(name, window, cx));
        self.reload(cx);
    }

    /// Sends an action to the player's one deck.
    fn deck(&mut self, action: DeckAction) {
        if let Some(player) = &mut self.player {
            let _ = player.apply(Role::Host, Action::Deck { deck: 0, action });
        }
    }

    /// Puts a track in the player, ready to hear, with its cue points.
    fn play_track(&mut self, track: i64, cx: &mut Context<Self>) {
        if self
            .playing
            .as_ref()
            .is_some_and(|playing| playing.id == track)
        {
            return;
        }
        let Some(library) = &self.library else {
            return;
        };
        let Ok(Some(found)) = library.track(track) else {
            return;
        };
        if self.player.is_none() {
            match Engine::new() {
                Ok(mut player) => {
                    player.set_cache_dir(crate::settings::waveform_dir());
                    self.player = Some(player);
                }
                Err(error) => {
                    self.error = Some(error.to_string().into());
                    cx.notify();
                    return;
                }
            }
        }
        self.cues = library.cues(track).unwrap_or_default();
        self.deck(DeckAction::Load {
            path: found.path.to_string_lossy().into_owned(),
        });
        self.playing = Some(found);
        cx.notify();
    }

    /// Stops the player, for when the editor goes out of view.
    pub fn pause(&mut self) {
        self.deck(DeckAction::Pause);
    }

    /// Saves the player track's cue points and shows them everywhere.
    fn save_cues(&mut self, cues: Vec<CuePoint>, cx: &mut Context<Self>) {
        let (Some(track), Some(library)) = (&self.playing, &mut self.library) else {
            return;
        };
        if let Err(error) = library.set_cues(track.id, &cues) {
            self.error = Some(error.to_string().into());
        }
        self.reload(cx);
    }

    /// Adds a cue point where the player is, taking on the energy of the
    /// section it divides.
    fn add_cue(&mut self, seconds: f64, cx: &mut Context<Self>) {
        if self.cues.len() >= MAX_CUES
            || self
                .cues
                .iter()
                .any(|cue| (cue.seconds - seconds).abs() < 0.5)
        {
            return;
        }
        let energy = self
            .cues
            .iter()
            .rev()
            .find(|cue| cue.seconds <= seconds)
            .map(|cue| cue.energy)
            .or(self.playing.as_ref().and_then(|track| track.energy))
            .unwrap_or(5);
        let mut cues = self.cues.clone();
        cues.push(CuePoint { seconds, energy });
        self.save_cues(cues, cx);
    }

    /// Removes the cue point the player is at or has most recently passed.
    fn delete_cue(&mut self, seconds: f64, cx: &mut Context<Self>) {
        if let Some(index) = self
            .cues
            .iter()
            .rposition(|cue| cue.seconds <= seconds + 0.05)
        {
            let mut cues = self.cues.clone();
            cues.remove(index);
            self.save_cues(cues, cx);
        }
    }

    /// Jumps the player to the cue point before or after where it is.
    fn jump_to_cue(&mut self, seconds: f64, forwards: bool) {
        let target = if forwards {
            self.cues.iter().find(|cue| cue.seconds > seconds + 0.05)
        } else {
            // A second's grace, so pressing twice steps back twice.
            self.cues
                .iter()
                .rev()
                .find(|cue| cue.seconds < seconds - 1.)
        };
        if let Some(seconds) = target.map(|cue| cue.seconds) {
            self.deck(DeckAction::Seek { seconds });
        }
    }

    fn end_seconds(&self) -> f64 {
        self.clips.iter().map(FlowClip::end).fold(0., f64::max)
    }

    /// Adds the whole of a track to the end of the flow.
    fn append_track(&mut self, track: i64, cx: &mut Context<Self>) {
        self.append_track_from(track, 0., cx);
    }

    /// Adds a track after the last clip, on the next of the first two lanes,
    /// overlapping the clip before it by a typical transition's length. The
    /// new clip begins `offset` seconds into the track.
    fn append_track_from(&mut self, track: i64, offset: f64, cx: &mut Context<Self>) {
        let Some(flow) = self.ensure_flow(cx) else {
            return;
        };
        let last = self.clips.iter().max_by(|a, b| a.end().total_cmp(&b.end()));
        let (lane, start) = match last {
            None => (0, 0.),
            Some(last) => {
                let grid = self
                    .library
                    .as_ref()
                    .and_then(|library| library.beat_grid(track).ok().flatten());
                let overlap = match grid {
                    Some(grid) => OVERLAP_BARS * bar_seconds(grid),
                    None => DEFAULT_OVERLAP_SECONDS,
                };
                // Never more than half the clip being mixed out of.
                let overlap = overlap.min(last.duration() / 2.);
                ((last.lane + 1) % 2, last.end() - overlap)
            }
        };
        self.change(cx, |library| {
            let clip = library.add_clip(flow, track, lane, start)?;
            if offset > 0. {
                // The library keeps the trim within the track's length.
                let placement = ClipPlacement {
                    lane,
                    start,
                    offset,
                    length: f64::MAX,
                    speed: 1.,
                };
                library.place_clip(clip, placement)?;
            }
            Ok(())
        });
    }

    /// The timeline time and lane under a point on screen.
    fn locate(&self, position: Point<Pixels>) -> (f64, usize) {
        let lanes = self.lanes.get();
        let x = (position.x - lanes.origin.x).as_f32();
        let y = (position.y - lanes.origin.y).as_f32();
        let lane = ((y / LANE_HEIGHT).floor().max(0.) as usize).min(FLOW_LANES - 1);
        ((x / self.zoom) as f64, lane)
    }

    fn drop_track(&mut self, track: i64, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(flow) = self.ensure_flow(cx) else {
            return;
        };
        let (time, lane) = self.locate(position);
        let start = (time.max(0.) * 10.).round() / 10.;
        self.change(cx, |library| {
            library.add_clip(flow, track, lane, start).map(|_| ())
        });
    }

    /// Takes hold of a block. `other_mode` (the Alt key) swaps cutting and
    /// stretching for this one drag.
    fn begin_drag(
        &mut self,
        clip: i64,
        grip: Grip,
        other_mode: bool,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(found) = self.clips.iter().find(|c| c.id == clip) else {
            return;
        };
        let stretch = self.stretch != other_mode;
        let mode = match (grip, stretch) {
            (Grip::Body, _) => DragMode::Move,
            (Grip::Start, false) => DragMode::TrimStart,
            (Grip::End, false) => DragMode::TrimEnd,
            (Grip::Start, true) => DragMode::StretchStart,
            (Grip::End, true) => DragMode::StretchEnd,
        };
        let (time, _) = self.locate(position);
        self.drag = Some(ClipDrag {
            clip,
            mode,
            grabbed_at: time,
            original: found.placement(),
        });
        self.selected = Some(clip);
        let track = found.track.id;
        self.play_track(track, cx);
        cx.notify();
    }

    /// Follows the pointer while a clip is held. Changes stay in memory
    /// until the button is released.
    fn continue_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = &self.drag else {
            return;
        };
        let (time, lane) = self.locate(position);
        let (clip_id, mode, original) = (drag.clip, drag.mode, drag.original);
        let delta = time - drag.grabbed_at;
        let original_end = original.start + original.length / original.speed;
        let others = || self.clips.iter().filter(|clip| clip.id != clip_id);
        let edges: Vec<f64> = others().flat_map(|clip| [clip.start, clip.end()]).collect();
        let neighbour_bpms: Vec<f64> = others()
            .filter(|clip| clip.start < original_end && clip.end() > original.start)
            .filter_map(FlowClip::bpm)
            .collect();
        let Some(clip) = self.clips.iter().find(|clip| clip.id == clip_id) else {
            return;
        };
        let placement = dragged_placement(
            mode,
            original,
            delta,
            lane,
            &DragContext {
                track_seconds: clip.track.duration_seconds,
                grid: clip.grid,
                cues: &clip.cues,
                bpm: clip.track.bpm,
                edges: &edges,
                neighbour_bpms: &neighbour_bpms,
                reach: (SNAP_PIXELS / self.zoom) as f64,
            },
        );
        if let Some(clip) = self.clips.iter_mut().find(|clip| clip.id == clip_id) {
            clip.lane = placement.lane;
            clip.start = placement.start;
            clip.offset = placement.offset;
            clip.length = placement.length;
            clip.speed = placement.speed;
        }
        cx.notify();
    }

    fn end_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        let placement = self
            .clips
            .iter()
            .find(|clip| clip.id == drag.clip)
            .map(FlowClip::placement);
        if let Some(placement) = placement.filter(|placement| *placement != drag.original) {
            self.change(cx, |library| library.place_clip(drag.clip, placement));
        } else {
            cx.notify();
        }
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let selected = self.selected;
        let open = self.flow;
        let view = cx.weak_entity();
        let flows: Vec<(i64, SharedString)> = self
            .flows
            .iter()
            .map(|flow| {
                (
                    flow.id,
                    format!("{} ({})", flow.name, flow.clip_count).into(),
                )
            })
            .collect();
        let picker = Button::new("pick-flow")
            .outline()
            .small()
            .icon(IconName::Workflow)
            .label(if open.is_some() {
                "Saved flow"
            } else {
                "New flow"
            })
            .dropdown_caret(true)
            .tooltip("Open a saved flow, or start a new one")
            .dropdown_menu(move |menu, _, _| {
                let item = |label: SharedString, flow: Option<i64>| {
                    let view = view.clone();
                    PopupMenuItem::new(label).checked(flow == open).on_click(
                        move |_, window, cx| {
                            view.update(cx, |view, cx| view.open_flow(flow, window, cx))
                                .ok();
                        },
                    )
                };
                let mut menu = menu.item(item("New flow".into(), None));
                if !flows.is_empty() {
                    menu = menu.separator();
                }
                for (id, label) in &flows {
                    menu = menu.item(item(label.clone(), Some(*id)));
                }
                menu
            });
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(picker)
            .child(div().w(px(240.)).child(Input::new(&self.name)))
            .child(div().text_sm().text_color(muted).child(format!(
                "{} · {}",
                match self.clips.len() {
                    1 => "1 track".to_string(),
                    count => format!("{count} tracks"),
                },
                clock(self.end_seconds())
            )))
            .child(div().flex_1())
            .child(
                Button::new("stretch-mode")
                    .outline()
                    .small()
                    .icon(IconName::StretchHorizontal)
                    .label("Stretch")
                    .tooltip(
                        "On: dragging a block's edge changes its tempo. Off: it cuts the track. \
                         Hold Alt while dragging for the other one. (Shift+M)",
                    )
                    .selected(self.stretch)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.stretch = !this.stretch;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("remove-clip")
                    .outline()
                    .small()
                    .icon(IconName::Trash)
                    .label("Remove")
                    .tooltip("Remove the selected track from this flow (Delete)")
                    .disabled(selected.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(clip) = selected {
                            this.change(cx, |library| library.remove_clip(clip));
                        }
                    })),
            )
            .child(
                Button::new("delete-flow")
                    .ghost()
                    .small()
                    .icon(IconName::Minus)
                    .tooltip("Delete this flow (tracks stay in the library)")
                    .disabled(open.is_none())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(flow) = open {
                            this.change(cx, |library| library.delete_flow(flow));
                            this.open_flow(None, window, cx);
                        }
                    })),
            )
            .child(
                Button::new("zoom-out")
                    .ghost()
                    .small()
                    .icon(IconName::ZoomOut)
                    .tooltip("Show more of the set")
                    .disabled(self.zoom <= MIN_ZOOM)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.zoom = (this.zoom / 1.5).max(MIN_ZOOM);
                        cx.notify();
                    })),
            )
            .child(
                Button::new("zoom-in")
                    .ghost()
                    .small()
                    .icon(IconName::ZoomIn)
                    .tooltip("Show more detail")
                    .disabled(self.zoom >= MAX_ZOOM)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.zoom = (this.zoom * 1.5).min(MAX_ZOOM);
                        cx.notify();
                    })),
            )
    }

    /// The block the player's track was picked from, if it is still the
    /// selected one. Key changes made in the player apply to it.
    fn playing_clip(&self) -> Option<&FlowClip> {
        let track = self.playing.as_ref()?.id;
        self.clips
            .iter()
            .find(|clip| Some(clip.id) == self.selected && clip.track.id == track)
    }

    /// The player's key. Clicking it opens a panel with the key on the
    /// Camelot wheel and buttons either side to shift it by a semitone.
    fn key_chip(&self, track: &Track, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted, background, tile) = (
            theme.border,
            theme.muted_foreground,
            theme.background,
            theme.secondary,
        );
        let clip = self.playing_clip();
        let shift = clip.map_or(0, |clip| clip.key_shift);
        let clip_id = clip.map(|clip| clip.id);
        let key = track.camelot.map(|key| key.shifted(shift));
        let label = match (key, shift) {
            (None, _) => "—".to_string(),
            (Some(key), 0) => key.to_string(),
            (Some(key), shift) => format!("{key} ({shift:+})"),
        };
        let step = |id: &'static str, icon: IconName, by: i32, tooltip: &'static str| {
            Button::new(id)
                .outline()
                .icon(icon)
                .tooltip(tooltip)
                .disabled(clip_id.is_none() || key.is_none())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(clip) = clip_id {
                        this.change(cx, |library| library.set_clip_key_shift(clip, shift + by));
                    }
                }))
        };
        let note = match (clip_id, key, shift) {
            (_, None, _) => "This track's key is not known.".to_string(),
            (None, ..) => "Select a block to shift its key.".to_string(),
            (Some(_), _, 0) => "The track's own key.".to_string(),
            (Some(_), _, 1) => "Up 1 semitone.".to_string(),
            (Some(_), _, -1) => "Down 1 semitone.".to_string(),
            (Some(_), _, shift) if shift > 0 => format!("Up {shift} semitones."),
            (Some(_), _, shift) => format!("Down {} semitones.", -shift),
        };

        div()
            .relative()
            .flex()
            .items_center()
            .gap_2()
            .child(div().text_xs().text_color(muted).child("KEY"))
            .child(
                div()
                    .id("player-key")
                    .px_2()
                    .rounded_sm()
                    .border_1()
                    .border_color(border)
                    .text_sm()
                    .when_some(key, |this, key| {
                        this.bg(key_colour(key, 0.85)).text_color(gpui_kit::white())
                    })
                    .hover(|style| style.border_color(muted))
                    .child(label)
                    .on_click(cx.listener(|this, _, _, cx| {
                        // A click here while the panel is open has already
                        // closed it, as a click outside; do not reopen it.
                        let just_dismissed = this
                            .key_dismissed
                            .take()
                            .is_some_and(|at| at.elapsed() < Duration::from_millis(400));
                        this.key_open = !just_dismissed;
                        cx.notify();
                    })),
            )
            .when(self.key_open, |this| {
                // Painted last, so it floats over the timeline.
                this.child(deferred(
                    div()
                        .id("key-panel")
                        .absolute()
                        // Upwards: there is more room above the player.
                        .bottom(px(30.))
                        .left_0()
                        .occlude()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .p_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(border)
                        .bg(background)
                        .shadow_lg()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(step("key-down", IconName::Minus, -1, "Down a semitone"))
                                .child(wheel_display(key, muted))
                                .child(step("key-up", IconName::Plus, 1, "Up a semitone")),
                        )
                        .child(
                            div()
                                .px_2()
                                .rounded_sm()
                                .bg(tile)
                                .text_xs()
                                .text_color(muted)
                                .child(note),
                        )
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.key_open = false;
                            this.key_dismissed = Some(Instant::now());
                            cx.notify();
                        })),
                ))
            })
    }

    /// The player: one track's whole waveform with its cue points flagged,
    /// and controls to hear it and edit the cue points. Nothing is shown
    /// until a track has been picked.
    fn player(&self, deck: Option<&DeckState>, cx: &mut Context<Self>) -> Option<Div> {
        let theme = cx.theme();
        let (border, muted, accent, foreground, tile) = (
            theme.border,
            theme.muted_foreground,
            theme.primary,
            theme.foreground,
            theme.secondary,
        );
        let panel = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .border_1()
            .border_color(border)
            .rounded_lg();
        let (track, deck) = (self.playing.as_ref()?, deck?);
        let position = deck.position_seconds;
        let duration = deck
            .track
            .as_ref()
            .map_or(track.duration_seconds, |info| info.duration_seconds)
            .max(0.001);
        let playing = deck.playing;
        let audio = self
            .player
            .as_ref()
            .and_then(|player| player.track(0))
            .cloned();
        let wave_bounds = self.wave.clone();

        let flags = self.cues.iter().enumerate().map(|(index, cue)| {
            let seconds = cue.seconds;
            div()
                .id(("cue-flag", index))
                .absolute()
                .left(relative((seconds / duration).clamp(0., 1.) as f32))
                .top_0()
                .h_full()
                .border_l_1()
                .border_color(foreground.opacity(0.8))
                .child(
                    div()
                        .px_1()
                        .rounded_br_sm()
                        .bg(foreground.opacity(0.8))
                        .text_size(px(10.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.background)
                        .whitespace_nowrap()
                        .child(format!("CUE {} · {}", index + 1, cue.energy)),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.deck(DeckAction::Seek { seconds });
                        cx.notify();
                    }),
                )
        });

        let waveform = div()
            .id("player-wave")
            .relative()
            .flex_1()
            .h(px(PLAYER_WAVE_HEIGHT))
            .rounded_md()
            .bg(tile.opacity(0.5))
            .overflow_hidden()
            .when_some(audio, |this, audio| {
                this.child(overview(
                    audio,
                    position / duration,
                    wave_bounds,
                    accent,
                    muted,
                    foreground,
                ))
            })
            .children(flags)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    let area = this.wave.get();
                    let width = area.size.width.as_f32().max(1.);
                    let fraction =
                        ((event.position.x - area.origin.x).as_f32() / width).clamp(0., 1.);
                    this.deck(DeckAction::Seek {
                        seconds: fraction as f64 * duration,
                    });
                    cx.notify();
                }),
            );

        let chip = |label: &'static str, value: String| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().text_xs().text_color(muted).child(label))
                .child(
                    div()
                        .px_2()
                        .rounded_sm()
                        .border_1()
                        .border_color(border)
                        .text_sm()
                        .child(value),
                )
        };
        let unknown = || "—".to_string();
        let panel = panel
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        Button::new("player-play")
                            .primary()
                            .icon(if playing {
                                IconName::Pause
                            } else {
                                IconName::Play
                            })
                            .tooltip(if playing { "Pause" } else { "Play" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.deck(if playing {
                                    DeckAction::Pause
                                } else {
                                    DeckAction::Play
                                });
                                cx.notify();
                            })),
                    )
                    .child(waveform),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(match track.artist.as_str() {
                                "" => track.title.clone(),
                                artist => format!("{artist} - {}", track.title),
                            }),
                    )
                    .child(self.key_chip(track, cx))
                    .child(chip(
                        "ENERGY",
                        track
                            .energy
                            .map_or_else(unknown, |energy| energy.to_string()),
                    ))
                    .child(chip(
                        "BPM",
                        track.bpm.map_or_else(unknown, |bpm| {
                            crate::settings::format_bpm(bpm, crate::library_view::bpm_decimals(cx))
                        }),
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().text_xs().text_color(muted).child("CUE POINTS"))
                            .child(
                                Button::new("cue-back")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::ChevronLeft)
                                    .tooltip("Previous cue point")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.jump_to_cue(position, false);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("cue-forward")
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::ChevronRight)
                                    .tooltip("Next cue point")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.jump_to_cue(position, true);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("insert-from-here")
                                    .outline()
                                    .xsmall()
                                    .icon(IconName::ListPlus)
                                    .label("Insert from here")
                                    .tooltip(
                                        "Add this track to the end of the flow, starting where the player is",
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(track) = this.playing.as_ref().map(|t| t.id) {
                                            this.append_track_from(track, position, cx);
                                        }
                                    })),
                            )
                            .child(
                                Button::new("cue-add")
                                    .outline()
                                    .xsmall()
                                    .label("Add cue")
                                    .tooltip("Add a cue point where the player is")
                                    .disabled(self.cues.len() >= MAX_CUES)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.add_cue(position, cx)
                                    })),
                            )
                            .child(
                                Button::new("cue-delete")
                                    .outline()
                                    .xsmall()
                                    .label("Delete cue")
                                    .tooltip(
                                        "Remove the cue point the player is at or has just passed",
                                    )
                                    .disabled(
                                        !self.cues.iter().any(|cue| cue.seconds <= position + 0.05),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.delete_cue(position, cx)
                                    })),
                            ),
                    )
                    .child(div().text_sm().text_color(muted).child(format!(
                        "{} / {}",
                        clock(position),
                        clock(duration)
                    ))),
            );
        Some(panel)
    }

    /// Minute marks along the top of the timeline.
    fn ruler(&self, seconds: f64, cx: &Context<Self>) -> Div {
        let muted = cx.theme().muted_foreground;
        // Keep labels at least about 60 pixels apart.
        let step = [30., 60., 120., 300., 600.]
            .into_iter()
            .find(|step| step * self.zoom >= 60.)
            .unwrap_or(600.) as f64;
        let marks = (0..=(seconds / step) as usize).map(|index| {
            let at = index as f64 * step;
            div()
                .absolute()
                .left(px(at as f32 * self.zoom))
                .top_0()
                .h_full()
                .pl_1()
                .border_l_1()
                .border_color(muted.opacity(0.4))
                .text_xs()
                .text_color(muted)
                .child(clock(at))
        });
        div().relative().h(px(RULER_HEIGHT)).children(marks)
    }

    /// A note at each point where one track comes in over another: how far
    /// apart their tempos are and whether their keys mix.
    fn hints(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let (good, bad, muted) = (theme.success, theme.warning, theme.muted_foreground);
        let notes = self.clips.iter().filter_map(|incoming| {
            // The track being mixed out of: the latest one still playing
            // (or only just finished) on another lane when this one starts.
            let outgoing = self
                .clips
                .iter()
                .filter(|other| {
                    other.lane != incoming.lane
                        && other.start < incoming.start
                        && other.end() > incoming.start - 1.
                })
                .max_by(|a, b| a.start.total_cmp(&b.start))?;

            let tempo = match (outgoing.bpm(), incoming.bpm()) {
                (Some(from), Some(to)) => {
                    let change = (to / from - 1.) * 100.;
                    Some((format!("{change:+.1}% BPM"), change.abs() <= 6.))
                }
                _ => None,
            };
            let keys = match (outgoing.camelot(), incoming.camelot()) {
                (Some(from), Some(to)) => {
                    let mixes = from.compatible().contains(&to);
                    Some((format!("{from} → {to}"), mixes))
                }
                _ => None,
            };
            // Energy of each track at the moment the new one comes in.
            let leaving = energy_at(
                outgoing,
                outgoing.offset + (incoming.start - outgoing.start) * outgoing.speed,
            );
            let arriving = energy_at(incoming, incoming.offset);
            let energy = match (leaving, arriving) {
                (Some(from), Some(to)) => Some(format!("energy {from} → {to}")),
                _ => None,
            };
            let part = |part: Option<(String, bool)>, unknown: &'static str| match part {
                Some((text, fits)) => div().text_color(if fits { good } else { bad }).child(text),
                None => div().text_color(muted).child(unknown),
            };
            Some(
                div()
                    .absolute()
                    .left(px(incoming.start as f32 * self.zoom))
                    .top_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pl_1()
                    .border_l_1()
                    .border_color(muted.opacity(0.6))
                    .text_xs()
                    .whitespace_nowrap()
                    .child(part(tempo, "tempo unknown"))
                    .child(part(keys, "key unknown"))
                    .children(energy.map(|energy| div().text_color(muted).child(energy))),
            )
        });
        div().relative().h(px(HINTS_HEIGHT)).children(notes)
    }

    /// A clip's cue points as numbered lines, with a strip along the bottom
    /// whose brightness follows the energy of each section.
    fn cue_marks(&self, clip: &FlowClip) -> Vec<Div> {
        let visible = |seconds: f64| seconds >= clip.offset && seconds < clip.offset + clip.length;
        // Track time to pixels along the block, which a stretch rescales.
        let x =
            |seconds: f64| px((((seconds - clip.offset) / clip.speed) as f32 * self.zoom).max(0.));
        let mut marks = Vec::new();
        for (index, cue) in clip.cues.iter().enumerate() {
            // The section runs to the next cue, or the end of the track.
            let until = clip
                .cues
                .get(index + 1)
                .map_or(clip.track.duration_seconds, |next| next.seconds);
            let (from, to) = (
                cue.seconds.max(clip.offset),
                until.min(clip.offset + clip.length),
            );
            if to > from {
                marks.push(
                    div()
                        .absolute()
                        .left(x(from))
                        .bottom_0()
                        .w(px(((to - from) / clip.speed) as f32 * self.zoom))
                        .h(px(4.))
                        .bg(gpui_kit::white().opacity(cue.energy as f32 / 10. * 0.75)),
                );
            }
            if visible(cue.seconds) {
                marks.push(
                    div()
                        .absolute()
                        .left(x(cue.seconds))
                        .top_0()
                        .h_full()
                        .pl(px(2.))
                        .border_l_1()
                        .border_color(gpui_kit::white().opacity(0.7))
                        .text_size(px(9.))
                        .text_color(gpui_kit::white().opacity(0.8))
                        .child((index + 1).to_string()),
                );
            }
        }
        marks
    }

    fn clip_block(&self, clip: &FlowClip, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let id = clip.id;
        let selected = self.selected == Some(id);
        let stretching = self.stretch;
        let colour = match clip.camelot() {
            Some(key) => key_colour(key, 0.85),
            None => theme.secondary,
        };
        let tempo = clip.bpm().map(|bpm| {
            if (clip.speed - 1.).abs() < 0.0005 {
                format!("{bpm:.0} BPM")
            } else {
                format!("{bpm:.0} BPM ({:+.1}%)", (clip.speed - 1.) * 100.)
            }
        });
        let detail = [
            tempo,
            clip.camelot().map(|key| match clip.key_shift {
                0 => key.to_string(),
                shift => format!("{key} ({shift:+})"),
            }),
            clip.track.energy.map(|energy| format!("energy {energy}")),
            Some(clock(clip.duration())),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");

        // The grab area on one end of the block. It reaches a little past
        // the block's edge, so the edge itself is easy to catch.
        let handle = |grip: Grip| {
            let name = if grip == Grip::Start {
                "clip-start"
            } else {
                "clip-end"
            };
            div()
                .id((name, id as usize))
                .absolute()
                .top_0()
                .h_full()
                .w(px(HANDLE_WIDTH))
                .map(|this| match grip {
                    Grip::Start => this.left(px(-HANDLE_WIDTH / 3.)),
                    _ => this.right(px(-HANDLE_WIDTH / 3.)),
                })
                .map(|this| {
                    if stretching {
                        this.cursor_ew_resize()
                    } else {
                        this.cursor_col_resize()
                    }
                })
                .flex()
                .justify_center()
                .py_2()
                .child(
                    div()
                        .w(px(3.))
                        .h_full()
                        .rounded_full()
                        .bg(gpui_kit::white().opacity(if selected { 0.7 } else { 0.3 })),
                )
                .hover(|style| style.bg(gpui_kit::white().opacity(0.25)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        window.focus(&this.focus, cx);
                        this.begin_drag(id, grip, event.modifiers.alt, event.position, cx);
                    }),
                )
        };

        div()
            .id(("clip", id as usize))
            .absolute()
            .left(px(clip.start as f32 * self.zoom))
            .top(px(clip.lane as f32 * LANE_HEIGHT + 3.))
            .w(px((clip.duration() as f32 * self.zoom).max(2.)))
            .h(px(LANE_HEIGHT - 6.))
            .child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .px_3()
                    .rounded_md()
                    .overflow_hidden()
                    .bg(colour)
                    .border_2()
                    .border_color(if selected { theme.foreground } else { colour })
                    .text_color(gpui_kit::white())
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(clip.track.title.clone()),
                    )
                    .child(div().text_xs().truncate().child(detail))
                    .children(self.cue_marks(clip)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    window.focus(&this.focus, cx);
                    this.begin_drag(id, Grip::Body, false, event.position, cx);
                }),
            )
            .child(handle(Grip::Start))
            .child(handle(Grip::End))
    }

    fn timeline(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, lane_colour, drop_colour) = (theme.border, theme.secondary, theme.accent);
        let seconds = self.end_seconds() + TAIL_SECONDS;
        let width = px(seconds as f32 * self.zoom);
        let bounds = self.lanes.clone();

        let lanes = div()
            .relative()
            .h(px(FLOW_LANES as f32 * LANE_HEIGHT))
            .children((0..FLOW_LANES).map(|lane| {
                div()
                    .absolute()
                    .left_0()
                    .top(px(lane as f32 * LANE_HEIGHT + 1.))
                    .w_full()
                    .h(px(LANE_HEIGHT - 2.))
                    .rounded_sm()
                    .bg(lane_colour.opacity(0.5))
                    .drag_over::<DraggedTrack>(move |style, _, _, _| style.bg(drop_colour))
                    .on_drop(cx.listener(|this, dragged: &DraggedTrack, window, cx| {
                        this.drop_track(dragged.id, window.mouse_position(), cx);
                    }))
            }))
            // Remembers where the lanes are, to turn pointer positions into
            // times and lanes.
            .child(
                canvas(move |area, _, _| bounds.set(area), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .children(
                self.clips
                    .iter()
                    .map(|clip| self.clip_block(clip, cx).into_any_element())
                    .collect::<Vec<_>>(),
            );

        div()
            .id("flow-timeline")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let key = event.keystroke.key.as_str();
                if let ("backspace" | "delete", Some(clip)) = (key, this.selected) {
                    this.change(cx, |library| library.remove_clip(clip));
                } else if key == "m" && event.keystroke.modifiers.shift {
                    this.stretch = !this.stretch;
                    cx.notify();
                }
            }))
            .overflow_x_scroll()
            .border_1()
            .border_color(border)
            .rounded_lg()
            .p_2()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                // A release outside the window is never reported, so a drag
                // also ends when the pointer comes back with the button up.
                if this.drag.is_some() && event.pressed_button != Some(MouseButton::Left) {
                    this.end_drag(cx);
                } else {
                    this.continue_drag(event.position, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end_drag(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.end_drag(cx)),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.selected = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w(width)
                    .child(self.ruler(seconds, cx))
                    .child(self.hints(cx))
                    .child(lanes),
            )
    }
}

/// Where a held clip should be, `delta` seconds of timeline from where it
/// was grabbed.
fn dragged_placement(
    mode: DragMode,
    original: ClipPlacement,
    delta: f64,
    lane: usize,
    context: &DragContext,
) -> ClipPlacement {
    let duration = original.length / original.speed;
    let end = original.start + duration;
    // Where a cut should land in the track: on a cue point if one is close,
    // otherwise on a bar. `reach` is in timeline seconds.
    let snap = |seconds: f64| {
        snap_in_track(
            seconds,
            context.grid,
            context.cues,
            context.reach * original.speed,
        )
    };
    let mut placement = original;
    match mode {
        DragMode::Move => {
            let mut start = (original.start + delta).max(0.);
            // Butt up against another clip's edge when close to one.
            let nearest = context
                .edges
                .iter()
                .flat_map(|edge| [*edge - start, *edge - (start + duration)])
                .min_by(|a, b| a.abs().total_cmp(&b.abs()));
            if let Some(shift) = nearest.filter(|shift| shift.abs() <= context.reach) {
                start = (start + shift).max(0.);
            }
            placement.start = start;
            placement.lane = lane;
        }
        // Cutting moves one end through the track; the other end stays
        // where it is on the timeline.
        DragMode::TrimStart => {
            let end_in_track = original.offset + original.length;
            let offset = snap(original.offset + delta * original.speed)
                .clamp(0., (end_in_track - MIN_CLIP_SECONDS).max(0.))
                // The clip cannot start before the timeline does.
                .max(original.offset - original.start * original.speed);
            placement.offset = offset;
            placement.start = original.start + (offset - original.offset) / original.speed;
            placement.length = end_in_track - offset;
        }
        DragMode::TrimEnd => {
            let end_in_track = snap(original.offset + original.length + delta * original.speed)
                .clamp(
                    original.offset + MIN_CLIP_SECONDS,
                    context
                        .track_seconds
                        .max(original.offset + MIN_CLIP_SECONDS),
                );
            placement.length = end_in_track - original.offset;
        }
        // Stretching keeps the same music and changes how long it takes.
        DragMode::StretchEnd => {
            placement.speed = snap_speed(original.length / (duration + delta).max(0.01), context);
        }
        DragMode::StretchStart => {
            let speed = snap_speed(original.length / (duration - delta).max(0.01), context);
            placement.speed = speed;
            placement.start = (end - original.length / speed).max(0.);
        }
    }
    placement
}

/// The speed a stretch should settle on: the tempo of an overlapping clip
/// if it is close to one, otherwise a whole number of BPM. A track with no
/// known tempo moves in steps of half a percent.
fn snap_speed(speed: f64, context: &DragContext) -> f64 {
    let speed = speed.clamp(MIN_CLIP_SPEED, MAX_CLIP_SPEED);
    let snapped = match context.bpm {
        Some(bpm) => {
            let tempo = speed * bpm;
            let neighbour = context
                .neighbour_bpms
                .iter()
                .copied()
                .filter(|other| (other - tempo).abs() <= TEMPO_MAGNET_BPM)
                .min_by(|a, b| (a - tempo).abs().total_cmp(&(b - tempo).abs()));
            neighbour.unwrap_or_else(|| tempo.round()) / bpm
        }
        None => (speed * 200.).round() / 200.,
    };
    snapped.clamp(MIN_CLIP_SPEED, MAX_CLIP_SPEED)
}

/// The length of one bar, taking four beats to the bar.
fn bar_seconds(grid: TempoResult) -> f64 {
    grid.bar_seconds()
}

/// The nearest bar line to a position in a track. Without a beat grid the
/// position is returned to the nearest tenth of a second.
fn snap_to_bar(seconds: f64, grid: Option<TempoResult>) -> f64 {
    match grid {
        Some(grid) => {
            let (first, bar) = (grid.first_bar_seconds(), bar_seconds(grid));
            first + ((seconds - first) / bar).round() * bar
        }
        None => (seconds * 10.).round() / 10.,
    }
}

/// Where a trimmed end should land in its track: on a cue point if one is
/// within `reach` seconds, otherwise on the nearest bar.
fn snap_in_track(seconds: f64, grid: Option<TempoResult>, cues: &[CuePoint], reach: f64) -> f64 {
    cues.iter()
        .map(|cue| cue.seconds)
        .filter(|cue| (cue - seconds).abs() <= reach)
        .min_by(|a, b| (a - seconds).abs().total_cmp(&(b - seconds).abs()))
        .unwrap_or_else(|| snap_to_bar(seconds, grid))
}

/// The energy of a clip's track at a time within the track: that of the
/// section it falls in, or of the whole track if it has no cue points.
fn energy_at(clip: &FlowClip, seconds: f64) -> Option<u8> {
    clip.cues
        .iter()
        .rev()
        .find(|cue| cue.seconds <= seconds + 0.01)
        .map(|cue| cue.energy)
        .or(clip.track.energy)
}

fn clock(seconds: f64) -> String {
    let seconds = seconds.max(0.).round() as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

impl Render for FlowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(error) = self.error.take() {
            window.defer(cx, move |window, cx| {
                window.push_notification(Notification::error(error), cx);
            });
        }
        let deck = self
            .player
            .as_mut()
            .and_then(|player| player.state().decks.into_iter().next());
        // The player's position comes from the engine, so keep drawing
        // while it moves or its track is still loading.
        let moving = |deck: &DeckState| {
            deck.playing || deck.loading || deck.track.as_ref().is_some_and(|t| !t.fully_loaded)
        };
        if deck.as_ref().is_some_and(moving) {
            window.request_animation_frame();
        }
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .gap_3()
            .child(self.toolbar(cx))
            .child(self.timeline(cx))
            .children(self.player(deck.as_ref(), cx))
            .child(div().flex().flex_1().min_h_0().child(self.browser.clone()))
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that would bring in GPUI's own `test` attribute.
    use super::{DragContext, DragMode, clock, dragged_placement, snap_in_track, snap_to_bar};
    use djcraft_library::TempoResult;
    use djcraft_library::{ClipPlacement, CuePoint};

    #[test]
    fn positions_snap_to_bar_lines() {
        let grid = Some(TempoResult {
            bpm: 120.,
            first_beat_seconds: 0.5,
            downbeat: 0,
            confidence: 1.,
        });
        // At 120 BPM a bar lasts two seconds: 0.5, 2.5, 4.5, ...
        assert_eq!(snap_to_bar(0., grid), 0.5);
        assert_eq!(snap_to_bar(1.6, grid), 2.5);
        assert_eq!(snap_to_bar(3.4, grid), 2.5);
        assert_eq!(snap_to_bar(3.6, grid), 4.5);
        assert_eq!(snap_to_bar(7.26, None), 7.3);
        // When the bar starts on the second beat, lines fall half a second on.
        let later = grid.map(|grid| TempoResult {
            downbeat: 1,
            ..grid
        });
        assert_eq!(snap_to_bar(0.9, later), 1.0);
        assert_eq!(snap_to_bar(3.4, later), 3.0);
        assert_eq!(clock(125.4), "2:05");

        // A cue point within reach wins over the bar line.
        let cues = [CuePoint {
            seconds: 3.2,
            energy: 5,
        }];
        assert_eq!(snap_in_track(3.0, grid, &cues, 0.5), 3.2);
        assert_eq!(snap_in_track(3.0, grid, &cues, 0.1), 2.5);
    }

    /// A four-minute clip of a 120 BPM track, placed one minute in.
    fn clip() -> ClipPlacement {
        ClipPlacement {
            lane: 0,
            start: 60.,
            offset: 0.,
            length: 240.,
            speed: 1.,
        }
    }

    fn context<'a>(edges: &'a [f64], neighbour_bpms: &'a [f64]) -> DragContext<'a> {
        DragContext {
            track_seconds: 240.,
            grid: Some(TempoResult {
                bpm: 120.,
                first_beat_seconds: 0.,
                downbeat: 0,
                confidence: 1.,
            }),
            cues: &[],
            bpm: Some(120.),
            edges,
            neighbour_bpms,
            reach: 1.,
        }
    }

    #[test]
    fn dragging_an_edge_cuts_the_track() {
        let none = context(&[], &[]);
        // Left edge dragged 8.6 s right: the clip starts later, and further
        // into the track, on a bar line (bars are 2 s at 120 BPM).
        let cut = dragged_placement(DragMode::TrimStart, clip(), 8.6, 0, &none);
        assert_eq!(
            (cut.start, cut.offset, cut.length, cut.speed),
            (68., 8., 232., 1.)
        );
        // Right edge dragged 31 s left: it ends earlier; the start stays.
        let cut = dragged_placement(DragMode::TrimEnd, clip(), -31., 0, &none);
        assert_eq!((cut.start, cut.offset, cut.length), (60., 0., 210.));
        // Neither edge can go past the ends of the track.
        let cut = dragged_placement(DragMode::TrimStart, clip(), -50., 0, &none);
        assert_eq!((cut.start, cut.offset, cut.length), (60., 0., 240.));
        let cut = dragged_placement(DragMode::TrimEnd, clip(), 50., 0, &none);
        assert_eq!(cut.length, 240.);
    }

    #[test]
    fn stretching_an_edge_changes_the_tempo() {
        let none = context(&[], &[]);
        // Right edge pulled in by about 4 s: the same music in less time is
        // faster, settling on a whole BPM (122), and the start does not move.
        let fast = dragged_placement(DragMode::StretchEnd, clip(), -4., 0, &none);
        assert!((fast.speed * 120. - 122.).abs() < 1e-9);
        assert_eq!((fast.start, fast.offset, fast.length), (60., 0., 240.));
        // Left edge pulled out: slower, and the end stays where it was.
        let slow = dragged_placement(DragMode::StretchStart, clip(), -10., 0, &none);
        assert!((slow.speed * 120. - 115.).abs() < 1e-9);
        assert!((slow.start + slow.length / slow.speed - 300.).abs() < 1e-9);

        // Close to an overlapping clip's tempo, it locks onto it exactly.
        let beside = context(&[], &[121.5]);
        let matched = dragged_placement(DragMode::StretchEnd, clip(), -4., 0, &beside);
        assert!((matched.speed * 120. - 121.5).abs() < 1e-9);

        // A stretch cannot go beyond half or double speed.
        let limit = dragged_placement(DragMode::StretchEnd, clip(), -239., 0, &none);
        assert_eq!(limit.speed, 2.);
    }

    #[test]
    fn a_moved_clip_snaps_to_other_clips() {
        let beside = context(&[400.], &[]);
        // Its end (start + 240) comes within reach of an edge at 400.
        let moved = dragged_placement(DragMode::Move, clip(), 99.5, 2, &beside);
        assert_eq!((moved.start, moved.lane), (160., 2));
        let free = dragged_placement(DragMode::Move, clip(), 50., 1, &beside);
        assert_eq!(free.start, 110.);
    }
}
