use std::cmp::Ordering;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::Duration;

use djcraft_library::lookup::{self, Provider, TrackMatch};
use djcraft_library::{
    ImportSummary, Library, Playlist, Source, SourceKind, Track, TrackEdit, TrackQuery,
    read_artwork, read_playlists,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Selectable, Sizable,
    button::{Button, ButtonRounded, ButtonVariants},
    input::{Input, InputEvent, InputState},
    menu::{PopupMenu, PopupMenuItem},
    table::{Column, ColumnSort, DataTable, TableDelegate, TableEvent, TableState},
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// How often the track list is refreshed while a scan is running.
const SCAN_REFRESH: Duration = Duration::from_millis(400);

/// A section of the library, chosen from the rail on the left.
#[derive(Clone, Copy, PartialEq)]
enum Section {
    Library,
    Playlists,
    Camelot,
    History,
}

impl Section {
    const ALL: [Section; 4] = [
        Section::Library,
        Section::Playlists,
        Section::Camelot,
        Section::History,
    ];

    fn icon_and_label(self) -> (IconName, &'static str) {
        match self {
            Section::Library => (IconName::Library, "Library"),
            Section::Playlists => (IconName::ListMusic, "Playlists"),
            Section::Camelot => (IconName::CircleDashed, "Camelot"),
            Section::History => (IconName::Clock, "History"),
        }
    }
}

/// Where the tracks shown in the library come from.
#[derive(Clone, Copy, PartialEq)]
enum Origin {
    Local,
    AppleMusic,
    Spotify,
    /// Not a source: the page for adding more of them.
    Plugins,
}

impl Origin {
    /// The sources shown as tabs.
    const ALL: [Origin; 3] = [Origin::Local, Origin::AppleMusic, Origin::Spotify];

    fn icon_and_label(self) -> (IconName, &'static str) {
        match self {
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
    Time,
}

const COLUMNS: [(Field, &str, f32); 7] = [
    (Field::Title, "Title", 260.),
    (Field::Artist, "Artist", 170.),
    (Field::Album, "Album", 170.),
    (Field::Genre, "Genre", 110.),
    (Field::Bpm, "BPM", 70.),
    (Field::Key, "Key", 60.),
    (Field::Time, "Time", 70.),
];

/// What the library asks the rest of the app to do.
pub enum LibraryEvent {
    /// Load a track onto the given deck, or onto whichever deck is free.
    Load { path: PathBuf, deck: Option<usize> },
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
                Field::Key => text(
                    a.key.as_deref().unwrap_or(""),
                    b.key.as_deref().unwrap_or(""),
                ),
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
            Field::Bpm | Field::Time => column.text_right(),
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

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        _: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let track = &self.tracks[row_ix];
        let text: SharedString = match COLUMNS[col_ix].0 {
            Field::Title => track.title.clone().into(),
            Field::Artist => track.artist.clone().into(),
            Field::Album => track.album.clone().into(),
            Field::Genre => track.genre.clone().into(),
            Field::Bpm => track
                .bpm
                .map(|bpm| format!("{bpm:.1}"))
                .unwrap_or_default()
                .into(),
            Field::Key => track.key.clone().unwrap_or_default().into(),
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
            |this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    let name = input.read(cx).value().trim().to_string();
                    if !name.is_empty() {
                        input.update(cx, |input, cx| input.set_value("", window, cx));
                        this.create_playlist(&name, cx);
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
                                path: track.path,
                                deck: None,
                            });
                        }
                    }
                    TableEvent::SelectRow(row) if !this.compact => {
                        if let Some(track) = track(row) {
                            this.inspect(track, window, cx);
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
            section: Section::Library,
            origin: Origin::Local,
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
        };
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
        view
    }

    /// Re-reads the sources and the visible tracks from the database.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(library) = &self.library else {
            return;
        };
        let in_playlists = self.section == Section::Playlists;
        let query = TrackQuery {
            source: self.selected.filter(|_| !in_playlists),
            playlist: self.playlist.filter(|_| in_playlists),
            search: self.search.read(cx).value().to_string(),
        };
        let loaded = (|| {
            let tracks = if in_playlists && query.playlist.is_none() {
                Vec::new()
            } else {
                library.tracks(&query)?
            };
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
        let bpm = track.bpm.map(format_bpm).unwrap_or_default();
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
                        Ok(source) => this.scan(source.id, cx),
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
                    .children(Origin::ALL.map(|origin| {
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
            .child(Input::new(&self.new_playlist))
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
                    ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_color(cx.theme().danger).child(error))
            })
            .when_some(self.notice.clone(), |this, notice| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(notice),
                )
            })
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                    Origin::Local => self.local(cx),
                    Origin::AppleMusic => self.placeholder("Apple Music is not connected yet.", cx),
                    Origin::Spotify => self.placeholder("Spotify is not connected yet.", cx),
                    Origin::Plugins => self.plugins(cx),
                }),
            Section::Playlists => self.playlists(cx),
            Section::Camelot => {
                self.placeholder("The Camelot wheel arrives with key analysis.", cx)
            }
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
