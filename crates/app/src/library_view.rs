use std::cmp::Ordering;
use std::collections::HashMap;
use std::f32::consts::TAU;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::{Duration, Instant};

use djcraft_analysis::TempoRange;
use djcraft_library::lookup::{self, Provider, TrackMatch};
use djcraft_library::{
    Camelot, FolderWatcher, ImportSummary, Library, Playlist, Source, SourceKind, TempoResult,
    Track, TrackEdit, TrackQuery, read_artwork, read_playlists,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Selectable, Sizable, WindowExt,
    button::{Button, ButtonRounded, ButtonVariants},
    input::{Input, InputEvent, InputState},
    menu::{DropdownMenu, PopupMenu, PopupMenuItem},
    notification::Notification,
    table::{Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState},
    tooltip::Tooltip,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// How often the track list is refreshed while a scan is running.
const SCAN_REFRESH: Duration = Duration::from_millis(400);
/// How often it is refreshed while tracks are being analysed.
const ANALYSIS_REFRESH: Duration = Duration::from_secs(2);
/// How often watched folders are asked whether they changed.
const WATCH_POLL: Duration = Duration::from_secs(1);
/// How long a folder must be left alone before it is scanned, so that a
/// copy in progress is scanned once, at its end.
const WATCH_QUIET: Duration = Duration::from_secs(2);

/// A section of the library, chosen from the rail on the left.
#[derive(Clone, Copy, PartialEq)]
enum Section {
    Library,
    Playlists,
    Flow,
    History,
}

impl Section {
    const ALL: [Section; 4] = [
        Section::Library,
        Section::Playlists,
        Section::Flow,
        Section::History,
    ];

    fn icon_and_label(self) -> (IconName, &'static str) {
        match self {
            Section::Library => (IconName::Library, "Library"),
            Section::Playlists => (IconName::ListMusic, "Playlists"),
            Section::Flow => (IconName::Workflow, "Flow"),
            Section::History => (IconName::Clock, "History"),
        }
    }
}

/// Where the tracks shown in the library come from.
#[derive(Clone, Copy, PartialEq)]
enum Origin {
    /// Every source together.
    All,
    Local,
    AppleMusic,
    Spotify,
    /// Not a source: the page for adding more of them.
    Plugins,
}

impl Origin {
    /// The sources shown as tabs.
    const ALL: [Origin; 4] = [
        Origin::All,
        Origin::Local,
        Origin::AppleMusic,
        Origin::Spotify,
    ];

    fn icon_and_label(self) -> (IconName, &'static str) {
        match self {
            Origin::All => (IconName::Layers, "All"),
            Origin::Local => (IconName::HardDrive, "Local"),
            Origin::AppleMusic => (IconName::Music, "Apple Music"),
            Origin::Spotify => (IconName::Disc3, "Spotify"),
            Origin::Plugins => (IconName::Plus, "Plugins"),
        }
    }
}

/// The details that can be edited in the info panel.
#[derive(Clone, Copy)]
enum EditField {
    Title,
    Artist,
    Album,
    Genre,
    Bpm,
    Key,
}

impl EditField {
    const ALL: [EditField; 6] = [
        EditField::Title,
        EditField::Artist,
        EditField::Album,
        EditField::Genre,
        EditField::Bpm,
        EditField::Key,
    ];

    fn label(self) -> &'static str {
        match self {
            EditField::Title => "Title",
            EditField::Artist => "Artist",
            EditField::Album => "Album",
            EditField::Genre => "Genre",
            EditField::Bpm => "BPM",
            EditField::Key => "Key",
        }
    }
}

/// A tempo without needless decimals: 128, 127.5.
/// How many decimals tempos are shown with, from the settings.
pub fn bpm_decimals(cx: &App) -> u8 {
    cx.try_global::<crate::settings::Settings>()
        .map_or(1, |settings| settings.bpm_decimals)
}

/// A tempo for an edit box: as precise as it is, without trailing zeros.
fn format_bpm(bpm: f64) -> String {
    let text = format!("{bpm:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[derive(Clone, Copy)]
enum Field {
    Title,
    Artist,
    Album,
    Genre,
    Bpm,
    Key,
    Energy,
    Time,
    Source,
}

const COLUMNS: [(Field, &str, f32); 9] = [
    (Field::Title, "Title", 260.),
    (Field::Artist, "Artist", 170.),
    (Field::Album, "Album", 170.),
    (Field::Genre, "Genre", 110.),
    (Field::Bpm, "BPM", 70.),
    (Field::Key, "Key", 60.),
    (Field::Energy, "Energy", 80.),
    (Field::Time, "Time", 70.),
    (Field::Source, "Source", 110.),
];

/// What the library asks the rest of the app to do.
pub enum LibraryEvent {
    /// A track was chosen (double-clicked, or sent to a deck from its menu).
    /// On the decks that loads it onto `deck`, or onto whichever is free.
    Load {
        id: i64,
        path: PathBuf,
        deck: Option<usize>,
    },
    /// A track was picked out in one of the compact browsers.
    Selected { id: i64 },
    /// A scan or analysis moved on, so the database may hold different
    /// tracks, or new results for them.
    Changed,
}

/// A track being dragged out of a track list.
#[derive(Clone)]
pub struct DraggedTrack {
    pub id: i64,
    pub title: SharedString,
}

impl Render for DraggedTrack {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .rounded_md()
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().border)
            .text_sm()
            .child(self.title.clone())
    }
}

const WHEEL_SIZE: f32 = 204.;
const WHEEL_LABEL: f32 = 24.;
/// Inner and outer edge of each ring of the wheel.
const WHEEL_MINOR: (f32, f32) = (44., 71.);
const WHEEL_MAJOR: (f32, f32) = (73., 100.);

/// Where a key sits round the wheel: twelve o'clock is 12, and each number
/// is a twelfth of a turn on.
fn wheel_angle(key: Camelot) -> f32 {
    (key.number % 12) as f32 * (TAU / 12.) - TAU / 4.
}

/// The top-left corner of a key's label, within the wheel.
fn wheel_label_corner(key: Camelot) -> (Pixels, Pixels) {
    let (inner, outer) = if key.major { WHEEL_MAJOR } else { WHEEL_MINOR };
    let radius = (inner + outer) / 2.;
    let angle = wheel_angle(key);
    (
        px(WHEEL_SIZE / 2. + radius * angle.cos() - WHEEL_LABEL / 2.),
        px(WHEEL_SIZE / 2. + radius * angle.sin() - WHEEL_LABEL / 2.),
    )
}

/// The wheel's 24 coloured segments. With a key lit, it and the keys that
/// mix with it stand out and the rest are dimmed.
fn wheel_segments(lit: Option<[Camelot; 4]>) -> impl IntoElement {
    let strength = move |key: Camelot| match lit {
        None => 0.85,
        Some(lit) if lit[0] == key => 1.,
        Some(lit) if lit.contains(&key) => 0.8,
        Some(_) => 0.18,
    };
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let centre = bounds.origin + point(px(WHEEL_SIZE / 2.), px(WHEEL_SIZE / 2.));
            let at = |radius: f32, angle: f32| {
                centre + point(px(radius * angle.cos()), px(radius * angle.sin()))
            };
            for key in Camelot::all() {
                let (inner, outer) = if key.major { WHEEL_MAJOR } else { WHEEL_MINOR };
                // A sliver is left between neighbours.
                let (from, to) = (
                    wheel_angle(key) - TAU / 24. + 0.02,
                    wheel_angle(key) + TAU / 24. - 0.02,
                );
                let steps = 8;
                let sweep = |step: usize| from + (to - from) * step as f32 / steps as f32;
                let mut path = PathBuilder::fill();
                path.move_to(at(outer, from));
                for step in 1..=steps {
                    path.line_to(at(outer, sweep(step)));
                }
                for step in (0..=steps).rev() {
                    path.line_to(at(inner, sweep(step)));
                }
                path.close();
                if let Ok(path) = path.build() {
                    window.paint_path(path, key_colour(key, strength(key)));
                }
            }
        },
    )
    .absolute()
    .size_full()
}

/// A Camelot wheel that only shows a key: it and the keys that mix with it
/// are lit, and its name is in the middle.
pub fn wheel_display(key: Option<Camelot>, muted: Hsla) -> Div {
    let lit = key.map(|key| key.compatible());
    let labels = Camelot::all().map(|label| {
        let (left, top) = wheel_label_corner(label);
        let dimmed = lit.is_some_and(|lit| !lit.contains(&label));
        div()
            .absolute()
            .left(left)
            .top(top)
            .size(px(WHEEL_LABEL))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(10.))
            .font_weight(FontWeight::BOLD)
            .text_color(gpui_kit::white().opacity(if dimmed { 0.45 } else { 1. }))
            .child(label.to_string())
    });
    let hub = div()
        .absolute()
        .left(px(WHEEL_SIZE / 2. - 40.))
        .top(px(WHEEL_SIZE / 2. - 20.))
        .w(px(80.))
        .h(px(40.))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .map(|this| match key {
            Some(key) => this
                .child(div().font_weight(FontWeight::BOLD).child(key.to_string()))
                .child(div().text_size(px(10.)).text_color(muted).child(key.name())),
            None => this.child(div().text_xs().text_color(muted).child("No key")),
        });
    div()
        .relative()
        .flex_none()
        .size(px(WHEEL_SIZE))
        .child(wheel_segments(lit))
        .children(labels)
        .child(hub)
}

/// The colour that stands for a key: hue by its number round the wheel,
/// darker for minor keys.
pub fn key_colour(key: Camelot, strength: f32) -> Hsla {
    let lightness = if key.major { 0.52 } else { 0.38 };
    hsla((key.number - 1) as f32 / 12., 0.62, lightness, strength)
}

struct TrackTable {
    view: WeakEntity<LibraryView>,
    tracks: Vec<Track>,
    sort: Option<(usize, ColumnSort)>,
    playlists: Vec<Playlist>,
    /// The playlist being shown, if any.
    playlist: Option<i64>,
}

impl TrackTable {
    fn new(view: WeakEntity<LibraryView>) -> Self {
        Self {
            view,
            tracks: Vec::new(),
            sort: None,
            playlists: Vec::new(),
            playlist: None,
        }
    }

    /// Replaces the rows, keeping the user's chosen sort order.
    fn set_tracks(&mut self, tracks: Vec<Track>, playlists: Vec<Playlist>, playlist: Option<i64>) {
        self.tracks = tracks;
        self.playlists = playlists;
        self.playlist = playlist;
        self.apply_sort();
    }

    fn sorted_by_user(&self) -> bool {
        matches!(
            self.sort,
            Some((_, ColumnSort::Ascending | ColumnSort::Descending))
        )
    }

    fn apply_sort(&mut self) {
        // An unsorted playlist stays in playlist order.
        if self.playlist.is_some() && !self.sorted_by_user() {
            return;
        }
        let text = |a: &str, b: &str| a.to_lowercase().cmp(&b.to_lowercase());
        // Tracks with no value sort after those with one.
        let optional = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(a), Some(b)) => a.total_cmp(&b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        let (field, descending) = match self.sort {
            Some((column, ColumnSort::Ascending)) => (COLUMNS[column].0, false),
            Some((column, ColumnSort::Descending)) => (COLUMNS[column].0, true),
            _ => (Field::Artist, false),
        };
        self.tracks.sort_by(|a, b| {
            let order = match field {
                Field::Title => text(&a.title, &b.title),
                Field::Artist => text(&a.artist, &b.artist),
                Field::Album => text(&a.album, &b.album),
                Field::Genre => text(&a.genre, &b.genre),
                Field::Bpm => optional(a.bpm, b.bpm),
                Field::Energy => optional(a.energy.map(f64::from), b.energy.map(f64::from)),
                // Everything is local for now, so fall back to the folder.
                Field::Source => text(&a.source_name, &b.source_name),
                // Wheel order, with keys that could not be read last.
                Field::Key => match (a.camelot, b.camelot) {
                    (Some(a), Some(b)) => a.cmp(&b),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => text(
                        a.key.as_deref().unwrap_or(""),
                        b.key.as_deref().unwrap_or(""),
                    ),
                },
                Field::Time => a.duration_seconds.total_cmp(&b.duration_seconds),
            };
            let order = if descending { order.reverse() } else { order };
            order.then_with(|| text(&a.title, &b.title))
        });
    }
}

impl TableDelegate for TrackTable {
    fn columns_count(&self, _: &App) -> usize {
        COLUMNS.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.tracks.len()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let (field, name, width) = COLUMNS[col_ix];
        let column = Column::new(name, name).width(px(width)).sortable();
        match field {
            Field::Bpm | Field::Energy | Field::Time => column.text_right(),
            _ => column,
        }
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        self.sort = Some((col_ix, sort));
        if self.playlist.is_some() && !self.sorted_by_user() {
            // Back to playlist order, which only the database knows.
            let view = self.view.clone();
            cx.defer(move |cx| {
                view.update(cx, |view, cx| view.reload(cx)).ok();
            });
        } else {
            self.apply_sort();
        }
        cx.notify();
    }

    fn context_menu(
        &mut self,
        row_ix: usize,
        menu: PopupMenu,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let Some(track) = self.tracks.get(row_ix) else {
            return menu;
        };
        let track_id = track.id;
        let item = |label: SharedString,
                    action: fn(&mut LibraryView, i64, i64, &mut Context<LibraryView>),
                    argument: i64| {
            let view = self.view.clone();
            PopupMenuItem::new(label).on_click(move |_, _, cx| {
                view.update(cx, |view, cx| action(view, track_id, argument, cx))
                    .ok();
            })
        };
        let mut menu = menu
            .item(item("Load to Deck 1".into(), LibraryView::load_to_deck, 0))
            .item(item("Load to Deck 2".into(), LibraryView::load_to_deck, 1));
        if let Some(playlist) = self.playlist {
            menu = menu.separator().item(item(
                "Remove from playlist".into(),
                LibraryView::remove_from_playlist,
                playlist,
            ));
        }
        let others: Vec<_> = self
            .playlists
            .iter()
            .filter(|playlist| Some(playlist.id) != self.playlist)
            .collect();
        if !others.is_empty() {
            menu = menu.separator().label("Add to playlist");
            for playlist in others {
                menu = menu.item(item(
                    playlist.name.clone().into(),
                    LibraryView::add_to_playlist,
                    playlist.id,
                ));
            }
        }
        menu
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let row = div().id(("row", row_ix));
        // Rows can be dragged out, onto a Flow lane for one.
        match self.tracks.get(row_ix) {
            Some(track) => row.on_drag(
                DraggedTrack {
                    id: track.id,
                    title: track.title.clone().into(),
                },
                |dragged, _, _, cx| cx.new(|_| dragged.clone()),
            ),
            None => row,
        }
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let track = &self.tracks[row_ix];
        let decimals = bpm_decimals(cx);
        let text: SharedString = match COLUMNS[col_ix].0 {
            Field::Title => track.title.clone().into(),
            Field::Artist => track.artist.clone().into(),
            Field::Album => track.album.clone().into(),
            Field::Genre => track.genre.clone().into(),
            // Where the track comes from; folders and loose files are local.
            Field::Source => match track.source_kind {
                SourceKind::Folder | SourceKind::Files => "Local".into(),
            },
            Field::Energy => track
                .energy
                .map(|energy| energy.to_string())
                .unwrap_or_default()
                .into(),
            Field::Bpm => track
                .bpm
                .map(|bpm| crate::settings::format_bpm(bpm, decimals))
                .unwrap_or_default()
                .into(),
            // Camelot notation when the tag can be understood, else as tagged.
            Field::Key => match track.camelot {
                Some(key) => key.to_string().into(),
                None => track.key.clone().unwrap_or_default().into(),
            },
            Field::Time => {
                let seconds = track.duration_seconds.round() as u64;
                format!("{}:{:02}", seconds / 60, seconds % 60).into()
            }
        };
        div().truncate().child(text)
    }
}

#[derive(Default)]
struct ScanCounters {
    done: AtomicUsize,
    total: AtomicUsize,
}

/// The Library tab: sources on the left, every track from them on the right.
pub struct LibraryView {
    db_path: PathBuf,
    /// `None` if the database could not be opened; `error` says why.
    library: Option<Library>,
    error: Option<SharedString>,
    /// The outcome of the last import.
    notice: Option<SharedString>,
    /// The small browser under the decks, not the full Library tab.
    compact: bool,
    /// Whether a compact browser offers the Camelot wheel, from a button
    /// beside its track count.
    wheel_in_browser: bool,
    /// Whether that wheel is showing.
    wheel_open: bool,
    /// When the wheel was last closed by a click outside it.
    wheel_dismissed: Option<Instant>,
    section: Section,
    origin: Origin,
    sources: Vec<Source>,
    /// The source being shown, or `None` for all tracks.
    selected: Option<i64>,
    playlists: Vec<Playlist>,
    /// The playlist open in the Playlists section.
    playlist: Option<i64>,
    new_playlist: Entity<InputState>,
    search: Entity<InputState>,
    table: Entity<TableState<TrackTable>>,
    /// The track shown in the info panel.
    inspected: Option<Track>,
    /// One text box per [`EditField`], in that order.
    fields: [Entity<InputState>; 6],
    artwork: Option<Arc<Image>>,
    matches: Vec<TrackMatch>,
    looking_up: bool,
    provider: Provider,
    /// Whether the info panel's details can be changed. Off until asked.
    editing: bool,
    running_scans: usize,
    scan_counters: Arc<ScanCounters>,
    /// Watches the folders for changes; `None` when that is switched off.
    watcher: Option<FolderWatcher>,
    /// The tempo range tracks are analysed with.
    tempo_range: TempoRange,
    /// Set when the library must be analysed again once the pass under way
    /// has stopped.
    reanalyse: bool,
    /// The key picked on the Camelot wheel.
    wheel_key: Option<Camelot>,
    key_counts: HashMap<Camelot, usize>,
    /// The Flow editor; the compact browsers have none.
    flow: Option<Entity<crate::flow_view::FlowView>>,
    /// Whether tracks are being analysed in the background.
    analysing: bool,
    analysis: Arc<crate::analysis::Progress>,
}

impl LibraryView {
    /// `compact` makes the track browser shown under the decks: the same
    /// library, without the rail, source tabs or editing controls.
    ///
    /// With `scan`, every folder is checked for changes straight away.
    pub fn new(compact: bool, scan: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let db_path = crate::settings::data_dir().join("library.db");
        let (library, error) = match Library::open(&db_path) {
            Ok(library) => (Some(library), None),
            Err(error) => (None, Some(error.to_string().into())),
        };
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.reload(cx);
            }
        })
        .detach();
        let new_playlist = cx.new(|cx| InputState::new(window, cx).placeholder("New playlist"));
        cx.subscribe_in(
            &new_playlist,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    // Enter in the New playlist dialog is the same as Create.
                    if this.create_named_playlist(cx) {
                        window.close_dialog(cx);
                    }
                }
            },
        )
        .detach();
        let view = cx.weak_entity();
        let table = cx.new(|cx| TableState::new(TrackTable::new(view), window, cx));
        cx.subscribe_in(
            &table,
            window,
            |this, table, event: &TableEvent, window, cx| {
                let track = |row: &usize| table.read(cx).delegate().tracks.get(*row).cloned();
                match event {
                    TableEvent::DoubleClickedRow(row) => {
                        if let Some(track) = track(row) {
                            cx.emit(LibraryEvent::Load {
                                id: track.id,
                                path: track.path,
                                deck: None,
                            });
                        }
                    }
                    TableEvent::SelectRow(row) => {
                        if let Some(track) = track(row) {
                            if this.compact {
                                cx.emit(LibraryEvent::Selected { id: track.id });
                            } else {
                                this.inspect(track, window, cx);
                            }
                        }
                    }
                    _ => {}
                }
            },
        )
        .detach();
        let fields = EditField::ALL
            .map(|field| cx.new(|cx| InputState::new(window, cx).placeholder(field.label())));

        let mut view = Self {
            db_path,
            library,
            error,
            notice: None,
            compact,
            wheel_in_browser: false,
            wheel_open: false,
            wheel_dismissed: None,
            section: Section::Library,
            origin: Origin::All,
            sources: Vec::new(),
            selected: None,
            playlists: Vec::new(),
            playlist: None,
            new_playlist,
            search,
            table,
            inspected: None,
            fields,
            artwork: None,
            matches: Vec::new(),
            looking_up: false,
            provider: Provider::default(),
            editing: false,
            running_scans: 0,
            scan_counters: Arc::default(),
            watcher: None,
            tempo_range: TempoRange::default(),
            reanalyse: false,
            flow: (!compact).then(|| cx.new(|cx| crate::flow_view::FlowView::new(window, cx))),
            wheel_key: None,
            key_counts: HashMap::new(),
            analysing: false,
            analysis: Arc::default(),
        };
        if !compact && let Some(settings) = cx.try_global::<crate::settings::Settings>().cloned() {
            view.tempo_range = settings.tempo_range();
            // The database holds this too, for every connection to see.
            if let Some(library) = &view.library
                && let Err(error) = library.set_keep_tagged(settings.keep_tagged())
            {
                view.error = Some(error.to_string().into());
            }
        }
        view.show_section(Section::Library, cx);
        // Pick up anything that changed on disk since the last run.
        // The Library tab does this; the browser shares its database.
        let sources = if scan && !compact {
            view.sources.clone()
        } else {
            Vec::new()
        };
        for source in sources {
            view.scan(source.id, cx);
        }
        view.analyse(cx);
        if !compact {
            view.poll_watcher(cx);
        }
        view
    }

    /// Follows a change to the settings that affect the library: how
    /// tracks are listed, and the tempo range they are analysed with.
    pub fn apply_settings(&mut self, settings: &crate::settings::Settings, cx: &mut Context<Self>) {
        self.set_watching(settings.watch_folders);
        if let Some(library) = &self.library
            && library.keep_tagged().ok() != Some(settings.keep_tagged())
            && let Err(error) = library.set_keep_tagged(settings.keep_tagged())
        {
            self.error = Some(error.to_string().into());
        }
        let range = settings.tempo_range();
        if range != self.tempo_range {
            self.tempo_range = range;
            self.reanalyse(cx);
        }
        self.reload(cx);
        cx.emit(LibraryEvent::Changed);
    }

    /// Analyses every track again. A pass under way is stopped first, since
    /// it was started with the old settings.
    fn reanalyse(&mut self, cx: &mut Context<Self>) {
        if self.analysing {
            self.reanalyse = true;
            self.analysis.cancel.store(true, AtomicOrdering::Relaxed);
            return;
        }
        if let Some(library) = &self.library
            && let Err(error) = library.reset_analysis()
        {
            self.error = Some(error.to_string().into());
        }
        self.analyse(cx);
    }

    /// The beat grid of the library track at `path`, if it has one.
    pub fn beat_grid_at(&self, path: &Path) -> Option<TempoResult> {
        let library = self.library.as_ref()?;
        let id = library.track_at(path).ok()??;
        library.beat_grid(id).ok()?
    }

    /// Starts or stops watching the library's folders for changes.
    pub fn set_watching(&mut self, on: bool) {
        if on == self.watcher.is_some() {
            return;
        }
        self.watcher = on.then(FolderWatcher::new);
        for source in self.sources.clone() {
            self.watch(&source);
        }
    }

    fn watch(&mut self, source: &Source) {
        let Some(watcher) = &mut self.watcher else {
            return;
        };
        if source.kind == SourceKind::Folder {
            // A folder that cannot be watched (an unplugged drive, say) is
            // still scanned at start-up.
            let _ = watcher.watch(source.id, Path::new(&source.location));
        }
    }

    /// Scans watched folders once changes to them have settled, for as
    /// long as the view lives.
    fn poll_watcher(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(WATCH_POLL).await;
                let alive = this.update(cx, |this, cx| {
                    // Changes during a scan wait for it to finish.
                    let changed = match &this.watcher {
                        Some(watcher) if this.running_scans == 0 => {
                            watcher.take_changed(WATCH_QUIET)
                        }
                        _ => Vec::new(),
                    };
                    for source_id in changed {
                        this.scan(source_id, cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Analyses tracks that have not been analysed yet, in the background,
    /// showing results as they come in. Does nothing if already under way.
    fn analyse(&mut self, cx: &mut Context<Self>) {
        if self.compact || self.analysing || self.library.is_none() {
            return;
        }
        self.analysing = true;
        let db_path = self.db_path.clone();
        let progress = self.analysis.clone();
        let range = self.tempo_range;
        let work = cx.background_spawn(async move {
            let waveforms = crate::settings::waveform_dir();
            crate::analysis::analyse_pending(&db_path, &waveforms, range, &progress)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| {
                this.analysing = false;
                cx.emit(LibraryEvent::Changed);
                if std::mem::take(&mut this.reanalyse) {
                    this.analysis.cancel.store(false, AtomicOrdering::Relaxed);
                    this.reanalyse(cx);
                    this.reload(cx);
                    return;
                }
                let analysed_some = this.analysis.total.load(AtomicOrdering::Relaxed) > 0;
                match result {
                    // Tracks may have been added while this pass was busy;
                    // go round again until a pass finds nothing to do.
                    Ok(()) if analysed_some => this.analyse(cx),
                    Ok(()) => {}
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                this.reload(cx);
            })
            .ok();
        })
        .detach();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(ANALYSIS_REFRESH).await;
                let analysing = this.update(cx, |this, cx| {
                    this.reload(cx);
                    cx.emit(LibraryEvent::Changed);
                    this.analysing
                });
                if !analysing.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Re-reads the sources and the visible tracks from the database.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(library) = &self.library else {
            return;
        };
        let in_playlists = self.section == Section::Playlists;
        let query = TrackQuery {
            id: None,
            source: self.selected.filter(|_| !in_playlists),
            playlist: self.playlist.filter(|_| in_playlists),
            search: self.search.read(cx).value().to_string(),
            // On the wheel, a chosen key lists everything that mixes with it.
            keys: self
                .wheel_key
                // In a browser the wheel filters whatever is listed.
                .filter(|_| self.section == Section::Library || self.compact)
                .map(|key| key.compatible().to_vec())
                .unwrap_or_default(),
        };
        let loaded = (|| {
            let tracks = if in_playlists && query.playlist.is_none() {
                Vec::new()
            } else {
                library.tracks(&query)?
            };
            self.key_counts = library.key_counts()?;
            Ok::<_, djcraft_library::Error>((library.sources()?, library.playlists()?, tracks))
        })();
        match loaded {
            Ok((sources, playlists, tracks)) => {
                self.sources = sources;
                self.playlists = playlists.clone();
                self.table.update(cx, |table, cx| {
                    table
                        .delegate_mut()
                        .set_tracks(tracks, playlists, query.playlist);
                    cx.notify();
                });
            }
            Err(error) => self.error = Some(error.to_string().into()),
        }
        cx.notify();
    }

    fn show_section(&mut self, section: Section, cx: &mut Context<Self>) {
        self.section = section;
        match (section, &self.flow) {
            (Section::Flow, Some(flow)) => flow.update(cx, |flow, cx| flow.refresh(cx)),
            // Leaving Flow stops its player.
            (_, Some(flow)) => flow.update(cx, |flow, _| flow.pause()),
            _ => {}
        }
        if section == Section::Playlists && self.playlist.is_none() {
            self.playlist = self.playlists.first().map(|playlist| playlist.id);
        }
        self.reload(cx);
    }

    fn create_playlist(&mut self, name: &str, cx: &mut Context<Self>) {
        let Some(library) = &self.library else {
            return;
        };
        match library.create_playlist(name) {
            Ok(playlist) => self.playlist = Some(playlist.id),
            Err(error) => self.error = Some(error.to_string().into()),
        }
        self.reload(cx);
    }

    fn delete_playlist(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(library) = &self.library else {
            return;
        };
        if let Err(error) = library.delete_playlist(id) {
            self.error = Some(error.to_string().into());
        }
        if self.playlist == Some(id) {
            self.playlist = None;
        }
        self.reload(cx);
        if self.playlist.is_none() {
            self.show_section(self.section, cx);
        }
    }

    fn add_to_playlist(&mut self, track: i64, playlist: i64, cx: &mut Context<Self>) {
        let Some(library) = &mut self.library else {
            return;
        };
        if let Err(error) = library.add_to_playlist(playlist, &[track]) {
            self.error = Some(error.to_string().into());
        }
        self.reload(cx);
    }

    fn remove_from_playlist(&mut self, track: i64, playlist: i64, cx: &mut Context<Self>) {
        let Some(library) = &self.library else {
            return;
        };
        if let Err(error) = library.remove_from_playlist(playlist, track) {
            self.error = Some(error.to_string().into());
        }
        self.reload(cx);
    }

    fn load_to_deck(&mut self, track: i64, deck: i64, cx: &mut Context<Self>) {
        let tracks = &self.table.read(cx).delegate().tracks;
        if let Some(track) = tracks.iter().find(|t| t.id == track) {
            cx.emit(LibraryEvent::Load {
                id: track.id,
                path: track.path.clone(),
                deck: Some(deck as usize),
            });
        }
    }

    fn field(&self, field: EditField) -> &Entity<InputState> {
        &self.fields[field as usize]
    }

    fn field_text(&self, field: EditField, cx: &App) -> String {
        self.field(field).read(cx).value().trim().to_string()
    }

    fn set_field(&self, field: EditField, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_string();
        self.field(field)
            .update(cx, |input, cx| input.set_value(text, window, cx));
    }

    /// Opens a track in the info panel.
    fn inspect(&mut self, track: Track, window: &mut Window, cx: &mut Context<Self>) {
        let bpm = track.bpm.map(format_bpm).unwrap_or_default();
        for (field, text) in [
            (EditField::Title, track.title.as_str()),
            (EditField::Artist, track.artist.as_str()),
            (EditField::Album, track.album.as_str()),
            (EditField::Genre, track.genre.as_str()),
            (EditField::Bpm, bpm.as_str()),
            (EditField::Key, track.key.as_deref().unwrap_or("")),
        ] {
            self.set_field(field, text, window, cx);
        }
        self.matches.clear();
        self.artwork = None;
        self.notice = None;
        self.editing = false;

        let (id, path) = (track.id, track.path.clone());
        self.inspected = Some(track);
        cx.spawn(async move |this, cx| {
            let artwork = cx
                .background_spawn(async move { read_artwork(&path) })
                .await
                .and_then(|artwork| {
                    let format = match artwork.data.as_slice() {
                        [0xff, 0xd8, ..] => ImageFormat::Jpeg,
                        [0x89, b'P', b'N', b'G', ..] => ImageFormat::Png,
                        _ => return None,
                    };
                    Some(Arc::new(Image::from_bytes(format, artwork.data)))
                });
            this.update(cx, |this, cx| {
                if this.inspected.as_ref().map(|track| track.id) == Some(id) {
                    this.artwork = artwork;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Writes the info panel's values to the file's tags and the library.
    fn save_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(track), Some(library)) = (&self.inspected, &self.library) else {
            return;
        };
        let id = track.id;
        let bpm_text = self.field_text(EditField::Bpm, cx);
        let bpm = match bpm_text.parse::<f64>() {
            Ok(bpm) if bpm > 0.0 => Some(bpm),
            _ if bpm_text.is_empty() => None,
            _ => {
                self.error = Some("BPM must be a number, such as 128 or 127.5.".into());
                cx.notify();
                return;
            }
        };
        let edit = TrackEdit {
            title: self.field_text(EditField::Title, cx),
            artist: self.field_text(EditField::Artist, cx),
            album: self.field_text(EditField::Album, cx),
            genre: self.field_text(EditField::Genre, cx),
            bpm,
            key: Some(self.field_text(EditField::Key, cx)).filter(|key| !key.is_empty()),
        };
        match library.edit_track(id, &edit) {
            Ok(()) => {
                self.error = None;
                self.reload(cx);
                let saved = self
                    .table
                    .read(cx)
                    .delegate()
                    .tracks
                    .iter()
                    .find(|t| t.id == id);
                if let Some(saved) = saved.cloned() {
                    self.inspect(saved, window, cx);
                }
                self.notice = Some("Saved to the file's tags.".into());
            }
            Err(error) => self.error = Some(error.to_string().into()),
        }
        cx.notify();
    }

    /// Chooses where "Look up" searches.
    pub fn set_provider(&mut self, provider: Provider, cx: &mut Context<Self>) {
        self.provider = provider;
        cx.notify();
    }

    /// Asks the chosen provider what the track in the info panel might be.
    fn look_up(&mut self, cx: &mut Context<Self>) {
        let (mut artist, mut title) = (
            self.field_text(EditField::Artist, cx),
            self.field_text(EditField::Title, cx),
        );
        // Untagged files are often named "Artist - Title".
        if artist.is_empty()
            && let Some((left, right)) = title.clone().split_once(" - ")
        {
            (artist, title) = (left.trim().to_string(), right.trim().to_string());
        }
        let provider = self.provider;
        self.looking_up = true;
        self.matches.clear();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { lookup::search(provider, &artist, &title) })
                .await;
            this.update(cx, |this, cx| {
                this.looking_up = false;
                match result {
                    Ok(matches) => {
                        this.error = None;
                        this.notice = matches
                            .is_empty()
                            .then(|| format!("{} found no match.", provider.label()).into());
                        this.matches = matches;
                    }
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Copies a lookup result into the text boxes. Nothing is saved yet.
    fn apply_match(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(found) = self.matches.get(index).cloned() else {
            return;
        };
        self.set_field(EditField::Title, &found.title, window, cx);
        self.set_field(EditField::Artist, &found.artist, window, cx);
        self.set_field(EditField::Album, &found.album, window, cx);
        if let Some(genre) = &found.genre {
            self.set_field(EditField::Genre, genre, window, cx);
        }
        cx.notify();
    }

    /// Lists the other tracks on the inspected track's album.
    fn show_album(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let album = self.field_text(EditField::Album, cx);
        if !album.is_empty() {
            self.search
                .update(cx, |search, cx| search.set_value(album, window, cx));
        }
    }

    /// The panel on the right: the selected track's details, editable.
    fn info_panel(&self, track: &Track, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, muted, tile) = (theme.border, theme.muted_foreground, theme.secondary);
        let seconds = track.duration_seconds.round() as u64;
        let editing = self.editing;
        let decimals = bpm_decimals(cx);
        let bpm = track
            .bpm
            .map(|bpm| crate::settings::format_bpm(bpm, decimals))
            .unwrap_or_default();
        let labelled = |field: EditField| {
            let label = div().text_xs().text_color(muted).child(field.label());
            let column = div().flex().flex_col().gap_1().child(label);
            if editing {
                return column.child(Input::new(self.field(field)));
            }
            let value = match field {
                EditField::Title => track.title.as_str(),
                EditField::Artist => track.artist.as_str(),
                EditField::Album => track.album.as_str(),
                EditField::Genre => track.genre.as_str(),
                EditField::Bpm => bpm.as_str(),
                EditField::Key => track.key.as_deref().unwrap_or(""),
            };
            column.child(if value.is_empty() {
                div().text_color(muted).child("—")
            } else {
                div().child(value.to_string())
            })
        };
        div()
            .id("track-info")
            .flex()
            .flex_col()
            .w(px(300.))
            .gap_3()
            .p_3()
            .border_1()
            .border_color(border)
            .rounded_lg()
            .overflow_y_scroll()
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::BOLD)
                            .child("Track info"),
                    )
                    .child(
                        Button::new("close-info")
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.inspected = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w_full()
                    .h(px(180.))
                    .rounded_lg()
                    .bg(tile)
                    .overflow_hidden()
                    .text_sm()
                    .text_color(muted)
                    .map(|this| match &self.artwork {
                        Some(artwork) => this.child(
                            img(artwork.clone())
                                .size_full()
                                .object_fit(ObjectFit::Contain),
                        ),
                        None => this.child("No cover art"),
                    }),
            )
            .child(labelled(EditField::Title))
            .child(labelled(EditField::Artist))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap_2()
                    .child(div().flex_1().child(labelled(EditField::Album)))
                    .child(
                        Button::new("show-album")
                            .outline()
                            .icon(IconName::Disc3)
                            .tooltip("Show this album's tracks")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.show_album(window, cx)),
                            ),
                    ),
            )
            .child(labelled(EditField::Genre))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(div().flex_1().child(labelled(EditField::Bpm)))
                    .child(div().flex_1().child(labelled(EditField::Key))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_xs()
                    .text_color(muted)
                    .child(format!("Length {}:{:02}", seconds / 60, seconds % 60))
                    .child(div().child(track.path.to_string_lossy().into_owned())),
            )
            .when(!editing, |this| {
                this.child(
                    div().flex().child(
                        Button::new("edit-info")
                            .outline()
                            .icon(IconName::Pencil)
                            .label("Edit")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.editing = true;
                                cx.notify();
                            })),
                    ),
                )
            })
            .when(editing, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            Button::new("cancel-edit")
                                .outline()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    // Opening the track again discards the edits.
                                    if let Some(track) = this.inspected.clone() {
                                        this.inspect(track, window, cx);
                                    }
                                })),
                        )
                        .child(
                            Button::new("save-info")
                                .primary()
                                .label("Save")
                                .tooltip("Write these details to the file's tags")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.save_edit(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("look-up")
                                .outline()
                                .icon(IconName::Search)
                                .label(if self.looking_up {
                                    "Looking up…"
                                } else {
                                    "Look up"
                                })
                                .tooltip(format!("Search {} for this track", self.provider.label()))
                                .disabled(self.looking_up)
                                .on_click(cx.listener(|this, _, _, cx| this.look_up(cx))),
                        ),
                )
            })
            .children(self.matches.iter().enumerate().map(|(index, found)| {
                let detail = [
                    Some(found.album.clone()).filter(|album| !album.is_empty()),
                    found.year.clone(),
                    found.genre.clone(),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
                div()
                    .id(("match", index))
                    .flex()
                    .flex_col()
                    .p_2()
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .text_sm()
                    .hover(|style| style.bg(tile))
                    .child(format!("{} — {}", found.artist, found.title))
                    .child(div().text_xs().text_color(muted).child(detail))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.apply_match(index, window, cx)),
                    )
            }))
    }

    /// Gives a compact browser a button that pops up the Camelot wheel.
    pub fn show_wheel(&mut self, cx: &mut Context<Self>) {
        self.wheel_in_browser = true;
        cx.notify();
    }

    /// Stops anything this view is playing, for when it goes out of view.
    pub fn pause_players(&mut self, cx: &mut Context<Self>) {
        if let Some(flow) = &self.flow {
            flow.update(cx, |flow, _| flow.pause());
        }
    }

    /// Re-reads the database, after another view may have changed it.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.reload(cx);
    }

    fn select(&mut self, source: Option<i64>, cx: &mut Context<Self>) {
        self.section = Section::Library;
        self.selected = source;
        self.reload(cx);
    }

    fn open_playlist(&mut self, playlist: i64, cx: &mut Context<Self>) {
        self.section = Section::Playlists;
        self.playlist = Some(playlist);
        self.reload(cx);
    }

    /// Asks for playlist files or a VirtualDJ folder and imports them.
    fn import_playlists(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some("Import".into()),
        });
        let db_path = self.db_path.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            // Reading tags for new tracks can take a while.
            let import = cx.background_spawn(async move {
                let mut playlists = Vec::new();
                for path in &paths {
                    playlists.extend(read_playlists(path)?);
                }
                Library::open(&db_path)?.import_playlists(&playlists)
            });
            let result = import.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(summary) => {
                        this.error = None;
                        this.notice = Some(import_notice(summary).into());
                        this.section = Section::Playlists;
                    }
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                this.reload(cx);
                this.analyse(cx);
            })
            .ok();
        })
        .detach();
    }

    fn add_folder(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Add".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            this.update(cx, |this, cx| {
                for path in paths {
                    let Some(library) = &this.library else {
                        return;
                    };
                    match library.add_folder(&path) {
                        Ok(source) => {
                            this.watch(&source);
                            this.scan(source.id, cx);
                        }
                        Err(error) => this.error = Some(error.to_string().into()),
                    }
                }
                this.reload(cx);
            })
            .ok();
        })
        .detach();
    }

    fn remove_source(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(library) = &self.library else {
            return;
        };
        if let Err(error) = library.remove_source(id) {
            self.error = Some(error.to_string().into());
        }
        if let Some(watcher) = &mut self.watcher {
            watcher.unwatch(id);
        }
        if self.selected == Some(id) {
            self.selected = None;
        }
        self.reload(cx);
    }

    /// Scans a source on a background thread, refreshing the list as tracks
    /// come in.
    fn scan(&mut self, source_id: i64, cx: &mut Context<Self>) {
        let db_path = self.db_path.clone();
        let counters = self.scan_counters.clone();
        let scan = cx.background_spawn(async move {
            // The scan gets its own connection; the UI keeps reading on its.
            let mut library = Library::open(&db_path)?;
            library.scan(source_id, |progress| {
                counters.done.store(progress.done, AtomicOrdering::Relaxed);
                counters
                    .total
                    .store(progress.total, AtomicOrdering::Relaxed);
            })
        });
        self.running_scans += 1;
        cx.spawn(async move |this, cx| {
            let result = scan.await;
            this.update(cx, |this, cx| {
                this.running_scans -= 1;
                if let Err(error) = result {
                    this.error = Some(error.to_string().into());
                }
                this.reload(cx);
                cx.emit(LibraryEvent::Changed);
                if let (Section::Flow, Some(flow)) = (this.section, &this.flow) {
                    flow.update(cx, |flow, cx| flow.refresh(cx));
                }
                // Whatever the scan found needs analysing.
                if this.running_scans == 0 {
                    this.analyse(cx);
                }
            })
            .ok();
        })
        .detach();

        if self.running_scans == 1 {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(SCAN_REFRESH).await;
                    let scanning = this.update(cx, |this, cx| {
                        this.reload(cx);
                        this.running_scans > 0
                    });
                    if !scanning.unwrap_or(false) {
                        break;
                    }
                }
            })
            .detach();
        }
    }

    /// The vertical rail that switches between the library's sections.
    fn rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (active, muted, tile) = (theme.foreground, theme.muted_foreground, theme.secondary);
        let item = |section: Section| {
            let selected = self.section == section;
            let (icon, label) = section.icon_and_label();
            div()
                .id(label)
                .relative()
                .flex()
                .flex_col()
                .items_center()
                .gap_1()
                .w_full()
                .py_2()
                .text_xs()
                .text_color(if selected { active } else { muted })
                .when(selected, |this| {
                    this.font_weight(FontWeight::BOLD).child(
                        div()
                            .absolute()
                            .left_0()
                            .top(px(12.))
                            .w(px(3.))
                            .h(px(32.))
                            .rounded_full()
                            .bg(active),
                    )
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(40.))
                        .rounded_lg()
                        .when(selected, |this| this.bg(tile))
                        .child(Icon::new(icon).large()),
                )
                .child(label)
                .hover(|style| style.text_color(active))
                .on_click(cx.listener(move |this, _, _, cx| this.show_section(section, cx)))
        };
        div()
            .flex()
            .flex_col()
            .w(px(84.))
            .gap_1()
            .py_2()
            .border_1()
            .border_color(theme.border)
            .rounded_lg()
            .children(Section::ALL.map(item))
    }

    /// The tabs that choose where tracks come from.
    fn origin_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .p_1()
                .rounded_full()
                .border_1()
                .border_color(cx.theme().border)
                .children(Origin::ALL.map(|origin| {
                    let (icon, label) = origin.icon_and_label();
                    Button::new(label)
                        .ghost()
                        .small()
                        .rounded(ButtonRounded::Size(px(999.)))
                        .icon(icon)
                        .label(label)
                        .selected(self.origin == origin)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.origin = origin;
                            cx.notify();
                        }))
                }))
                .child(
                    Button::new("add-source")
                        .ghost()
                        .small()
                        .rounded(ButtonRounded::Size(px(999.)))
                        .icon(IconName::Plus)
                        .tooltip("Add a source from a plugin")
                        .selected(self.origin == Origin::Plugins)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.origin = Origin::Plugins;
                            cx.notify();
                        })),
                ),
        )
    }

    /// Where source plugins are managed. Nothing can be installed yet: this
    /// is the place the plugin runtime and store will fill in.
    fn plugins(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let (border, muted) = (theme.border, theme.muted_foreground);
        let section = |title: &'static str, body: Div| {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().font_weight(FontWeight::BOLD).child(title))
                .child(body)
        };
        let card = |icon: IconName, name: &'static str, status: &'static str| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .p_3()
                .border_1()
                .border_color(border)
                .rounded_lg()
                .child(Icon::new(icon).large())
                .child(div().flex_1().child(name))
                .child(div().text_sm().text_color(muted).child(status))
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .max_w(px(720.))
            .gap_6()
            .child(section(
                "Sources",
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(Origin::ALL.into_iter().skip(1).map(|origin| {
                        let (icon, name) = origin.icon_and_label();
                        let status = match origin {
                            Origin::Local => "Built in",
                            _ => "Not connected yet",
                        };
                        card(icon, name, status)
                    })),
            ))
            .child(section(
                "Plugin store",
                div()
                    .p_3()
                    .border_1()
                    .border_color(border)
                    .rounded_lg()
                    .text_sm()
                    .text_color(muted)
                    .child("Plugins cannot be installed yet. Sources from the store will be listed here."),
            ))
    }

    fn folders(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (hover, muted) = (theme.secondary, theme.muted_foreground);
        let row = |id: ElementId, icon: IconName, label: SharedString, source: Option<i64>| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_2()
                .h(px(32.))
                .px_2()
                .rounded_md()
                .text_sm()
                .when(self.selected == source, |this| this.bg(hover))
                .hover(|style| style.bg(hover))
                .child(Icon::new(icon).small().text_color(muted))
                .child(div().flex_1().min_w_0().truncate().child(label))
                .on_click(cx.listener(move |this, _, _, cx| this.select(source, cx)))
        };
        div()
            .id("folders")
            // A browser's panel is short; let it scroll once the wheel is in.
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .w(px(220.))
            .gap_1()
            .p_2()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_lg()
            .child(row(
                "all-tracks".into(),
                IconName::Library,
                "All tracks".into(),
                None,
            ))
            .children(self.sources.iter().map(|source| {
                let id = source.id;
                div()
                    .flex()
                    .items_center()
                    .child(div().flex_1().min_w_0().child(row(
                        ("source", id as usize).into(),
                        match source.kind {
                            SourceKind::Folder => IconName::FolderOpen,
                            SourceKind::Files => IconName::Music,
                        },
                        source.name.clone().into(),
                        Some(id),
                    )))
                    .when(!self.compact, |this| {
                        this.child(
                            Button::new(("remove-source", id as usize))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Minus)
                                .tooltip("Remove from library (files are kept)")
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.remove_source(id, cx)),
                                ),
                        )
                    })
            }))
            .when(self.compact, |this| {
                this.child(
                    div()
                        .px_2()
                        .pt_2()
                        .text_xs()
                        .text_color(muted)
                        .child("Playlists"),
                )
                .children(self.playlists.iter().map(|playlist| {
                    let id = playlist.id;
                    div()
                        .id(("browser-playlist", id as usize))
                        .flex()
                        .items_center()
                        .gap_2()
                        .h(px(32.))
                        .px_2()
                        .rounded_md()
                        .text_sm()
                        .when(
                            self.section == Section::Playlists && self.playlist == Some(id),
                            |this| this.bg(hover),
                        )
                        .hover(|style| style.bg(hover))
                        .child(Icon::new(IconName::ListMusic).small().text_color(muted))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(playlist.name.clone()),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.open_playlist(id, cx)))
                }))
            })
            .when(!self.compact, |this| {
                this.child(div().flex_1())
                    .child(self.wheel(cx))
                    .child(
                        Button::new("import-to-library")
                            .outline()
                            .icon(IconName::Download)
                            .label("Import playlists")
                            .tooltip("From a rekordbox XML export, a VirtualDJ folder or M3U files")
                            .on_click(cx.listener(|this, _, _, cx| this.import_playlists(cx))),
                    )
                    .child(
                        Button::new("add-folder")
                            .outline()
                            .icon(IconName::Plus)
                            .label("Add folder")
                            .on_click(cx.listener(|this, _, _, cx| this.add_folder(cx))),
                    )
            })
    }

    fn playlist_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (hover, muted) = (theme.secondary, theme.muted_foreground);
        div()
            .flex()
            .flex_col()
            .w(px(220.))
            .gap_1()
            .p_2()
            .border_1()
            .border_color(theme.border)
            .rounded_lg()
            .children(self.playlists.iter().map(|playlist| {
                let id = playlist.id;
                div()
                    .id(("playlist", id as usize))
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(32.))
                    .px_2()
                    .rounded_md()
                    .text_sm()
                    .when(self.playlist == Some(id), |this| this.bg(hover))
                    .hover(|style| style.bg(hover))
                    .child(Icon::new(IconName::ListMusic).small().text_color(muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(playlist.name.clone()),
                    )
                    .child(
                        div()
                            .text_color(muted)
                            .child(playlist.track_count.to_string()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.playlist = Some(id);
                        this.reload(cx);
                    }))
                    .child(
                        Button::new(("delete-playlist", id as usize))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Minus)
                            .tooltip("Delete playlist (tracks stay in the library)")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.delete_playlist(id, cx);
                            })),
                    )
            }))
            .child(div().flex_1())
            .child(
                Button::new("import-playlists")
                    .outline()
                    .icon(IconName::Download)
                    .label("Import playlists")
                    .tooltip("From a rekordbox XML export, a VirtualDJ folder or M3U files")
                    .on_click(cx.listener(|this, _, _, cx| this.import_playlists(cx))),
            )
            .child(self.export_button(cx))
            .child(
                Button::new("new-playlist")
                    .outline()
                    .icon(IconName::Plus)
                    .label("New playlist")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.new_playlist_dialog(window, cx)),
                    ),
            )
    }

    /// Asks for a name and creates a playlist with it.
    fn new_playlist_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.new_playlist.clone();
        input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        let view = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            dialog
                .title("New playlist")
                .w(px(420.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().text_sm().child("Name"))
                        .child(Input::new(&input)),
                )
                .footer(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("cancel-new-playlist")
                                .outline()
                                .label("Cancel")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("create-new-playlist")
                                .primary()
                                .label("Create")
                                .on_click(move |_, window, cx| {
                                    let created = view
                                        .update(cx, |view, cx| view.create_named_playlist(cx))
                                        .unwrap_or(false);
                                    // An empty name keeps the dialog open.
                                    if created {
                                        window.close_dialog(cx);
                                    }
                                }),
                        ),
                )
        });
    }

    /// Creates a playlist named by the dialog's text box. Returns false, and
    /// does nothing, if no name has been typed.
    fn create_named_playlist(&mut self, cx: &mut Context<Self>) -> bool {
        let name = self.new_playlist.read(cx).value().trim().to_string();
        if name.is_empty() {
            return false;
        }
        self.create_playlist(&name, cx);
        true
    }

    /// A menu of file formats to save the open playlist in.
    fn export_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        Button::new("export-playlist")
            .outline()
            .icon(IconName::Upload)
            .label("Export playlist")
            .dropdown_caret(true)
            .disabled(self.playlist.is_none())
            .dropdown_menu(move |menu, _, _| {
                let item = |label: &'static str, extension: &'static str| {
                    let view = view.clone();
                    PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        view.update(cx, |view, cx| view.export_playlist(extension, cx))
                            .ok();
                    })
                };
                menu.item(item("M3U playlist (most DJ software)", "m3u8"))
                    .item(item("rekordbox XML", "xml"))
            })
    }

    /// Asks where to save, and writes the open playlist there.
    fn export_playlist(&mut self, extension: &'static str, cx: &mut Context<Self>) {
        let Some(playlist) = self.playlist else {
            return;
        };
        let name = self
            .playlists
            .iter()
            .find(|candidate| candidate.id == playlist)
            .map(|found| found.name.clone())
            .unwrap_or_else(|| "Playlist".into());
        // Slashes would be read as folders in a file name.
        let suggested = format!("{}.{extension}", name.replace(['/', ':'], "-"));
        let folder = dirs::document_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        let chosen = cx.prompt_for_new_path(&folder, Some(&suggested));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(mut file))) = chosen.await else {
                return;
            };
            // The format follows the extension, so make sure it is there.
            if file.extension().and_then(|e| e.to_str()) != Some(extension) {
                file.set_extension(extension);
            }
            this.update(cx, |this, cx| {
                let Some(library) = &this.library else {
                    return;
                };
                match library.export_playlist(playlist, &file) {
                    Ok(count) => {
                        let tracks = if count == 1 { "track" } else { "tracks" };
                        this.notice = Some(
                            format!("Exported {count} {tracks} to {}.", file.display()).into(),
                        );
                    }
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The Camelot wheel: major keys on the outer ring, minor on the inner.
    /// Picking a key lights up the keys that mix with it and narrows the
    /// track list to them.
    fn wheel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        const SIZE: f32 = WHEEL_SIZE;
        const LABEL: f32 = WHEEL_LABEL;

        let muted = cx.theme().muted_foreground;
        let picked = self.wheel_key;
        let lit = picked.map(|key| key.compatible());
        let segments = wheel_segments(lit);

        let labels = Camelot::all().map(|key| {
            let (left, top) = wheel_label_corner(key);
            let dimmed = lit.is_some_and(|lit| !lit.contains(&key));
            let count = self.key_counts.get(&key).copied().unwrap_or(0);
            div()
                .id(("wheel-key", key.number as usize * 2 + key.major as usize))
                .absolute()
                .left(left)
                .top(top)
                .size(px(LABEL))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(10.))
                .font_weight(FontWeight::BOLD)
                .text_color(gpui_kit::white().opacity(if dimmed { 0.45 } else { 1. }))
                .child(key.to_string())
                .tooltip(move |window, cx| {
                    let tracks = if count == 1 { "track" } else { "tracks" };
                    Tooltip::new(format!("{} · {count} {tracks}", key.name())).build(window, cx)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.wheel_key = (this.wheel_key != Some(key)).then_some(key);
                    this.reload(cx);
                }))
        });

        let hub = div()
            .absolute()
            .left(px(SIZE / 2. - 40.))
            .top(px(SIZE / 2. - 20.))
            .w(px(80.))
            .h(px(40.))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .map(|this| match picked {
                Some(key) => this
                    .child(div().font_weight(FontWeight::BOLD).child(key.to_string()))
                    .child(div().text_size(px(10.)).text_color(muted).child(key.name())),
                None => this.child(div().text_xs().text_color(muted).child("Any key")),
            });

        div().flex().justify_center().py_2().child(
            div()
                .relative()
                .flex_none()
                .size(px(SIZE))
                .child(segments)
                .children(labels)
                .child(hub),
        )
    }

    /// Local music: folders on the left, their tracks on the right.
    fn local(&self, cx: &mut Context<Self>) -> Div {
        let empty = (self.sources.is_empty() && self.library.is_some())
            .then_some("Add a folder to start your library.");
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .gap_3()
            .child(self.folders(cx))
            .child(self.track_pane(empty, cx))
            .when_some(self.inspected.clone(), |this, track| {
                this.child(self.info_panel(&track, cx))
            })
    }

    /// Playlists on the left, the open playlist's tracks on the right.
    fn playlists(&self, cx: &mut Context<Self>) -> Div {
        let empty = if self.playlists.is_empty() {
            Some("Type a name in the box on the left and press Enter to create a playlist.")
        } else if self.table.read(cx).delegate().tracks.is_empty()
            && self.search.read(cx).value().is_empty()
        {
            Some("Right-click a track in the library to add it to this playlist.")
        } else {
            None
        };
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .gap_3()
            .child(self.playlist_list(cx))
            .child(self.track_pane(empty, cx))
            .when_some(self.inspected.clone(), |this, track| {
                this.child(self.info_panel(&track, cx))
            })
    }

    /// A button showing the key picked on the Camelot wheel, which opens
    /// the wheel in a panel floating below it.
    fn wheel_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, background) = (theme.border, theme.background);
        let label: SharedString = match self.wheel_key {
            Some(key) => key.to_string().into(),
            None => "Key".into(),
        };
        div()
            .relative()
            .child(
                Button::new("open-wheel")
                    .outline()
                    .small()
                    .icon(IconName::CircleDashed)
                    .label(label)
                    .tooltip("Show tracks that mix with a key")
                    .selected(self.wheel_open || self.wheel_key.is_some())
                    .on_click(cx.listener(|this, _, _, cx| {
                        // A click on this button while the wheel is open has
                        // already closed it, as a click outside the wheel;
                        // it must not open it again.
                        let just_dismissed = this
                            .wheel_dismissed
                            .take()
                            .is_some_and(|at| at.elapsed() < Duration::from_millis(400));
                        this.wheel_open = !just_dismissed;
                        cx.notify();
                    })),
            )
            .when(self.wheel_open, |this| {
                // Painted last, so it floats over the track list.
                this.child(deferred(
                    div()
                        .id("wheel-popup")
                        .absolute()
                        .top(px(36.))
                        .right_0()
                        .occlude()
                        .p_2()
                        .rounded_lg()
                        .border_1()
                        .border_color(border)
                        .bg(background)
                        .shadow_lg()
                        .child(self.wheel(cx))
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.wheel_open = false;
                            this.wheel_dismissed = Some(Instant::now());
                            cx.notify();
                        })),
                ))
            })
    }

    /// The search box, status line and track table, or `empty` in place of
    /// the table when there is nothing to list.
    fn track_pane(&self, empty: Option<&'static str>, cx: &mut Context<Self>) -> Div {
        let count = self.table.read(cx).delegate().tracks.len();
        let total = self.scan_counters.total.load(AtomicOrdering::Relaxed);
        let status = if self.running_scans > 0 && total == 0 {
            // Still listing the folder; nothing to count yet.
            "Scanning…".into()
        } else if self.running_scans > 0 {
            let done = self.scan_counters.done.load(AtomicOrdering::Relaxed);
            format!("Scanning {done} of {total}…")
        } else if self.analysing {
            let done = self.analysis.done.load(AtomicOrdering::Relaxed);
            let total = self.analysis.total.load(AtomicOrdering::Relaxed);
            format!("{count} tracks · analysing {done} of {total}…")
        } else if count == 1 {
            "1 track".into()
        } else {
            format!("{count} tracks")
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .w(px(320.))
                            .child(Input::new(&self.search).cleanable(true)),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(status),
                    )
                    .when(self.wheel_in_browser, |this| {
                        this.child(self.wheel_button(cx))
                    }),
            )
            .child(match empty {
                Some(text) => self.placeholder(text, cx),
                None => div()
                    .flex_1()
                    .min_h_0()
                    .child(DataTable::new(&self.table).stripe(true)),
            })
    }

    fn placeholder(&self, text: &'static str, cx: &Context<Self>) -> Div {
        div()
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .text_color(cx.theme().muted_foreground)
            .child(text)
    }
}

fn import_notice(summary: ImportSummary) -> String {
    let plural = |count: usize, word: &str| {
        if count == 1 {
            format!("1 {word}")
        } else {
            format!("{count} {word}s")
        }
    };
    let mut notice = format!(
        "Imported {} ({} added).",
        plural(summary.playlists, "playlist"),
        plural(summary.tracks, "track"),
    );
    if summary.missing > 0 {
        notice.push_str(&format!(
            " {} not found on this computer.",
            plural(summary.missing, "file")
        ));
    }
    notice
}

impl EventEmitter<LibraryEvent> for LibraryView {}

impl Render for LibraryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Problems and results are shown as toasts, once each.
        let (error, notice) = (self.error.take(), self.notice.take());
        if error.is_some() || notice.is_some() {
            window.defer(cx, move |window, cx| {
                if let Some(error) = error {
                    window.push_notification(Notification::error(error), cx);
                }
                if let Some(notice) = notice {
                    window.push_notification(Notification::info(notice), cx);
                }
            });
        }
        if self.compact {
            return div()
                .flex()
                .size_full()
                .gap_3()
                .child(self.folders(cx))
                .child(self.track_pane(None, cx));
        }
        let content = match self.section {
            Section::Library => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap_3()
                .child(self.origin_tabs(cx))
                .child(match self.origin {
                    // Local folders are the only connected source so far, so
                    // "All" and "Local" list the same tracks.
                    Origin::All | Origin::Local => self.local(cx),
                    Origin::AppleMusic => self.placeholder("Apple Music is not connected yet.", cx),
                    Origin::Spotify => self.placeholder("Spotify is not connected yet.", cx),
                    Origin::Plugins => self.plugins(cx),
                }),
            Section::Playlists => self.playlists(cx),
            Section::Flow => match &self.flow {
                Some(flow) => div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .child(flow.clone()),
                None => self.placeholder("Flow is not available here.", cx),
            },
            Section::History => self.placeholder("Play history is not built yet.", cx),
        };
        div()
            .flex()
            .size_full()
            .gap_3()
            .child(self.rail(cx))
            .child(content)
    }
}
