use std::io;
use std::path::{Path, PathBuf};

use djcraft_analysis::TempoRange;
use djcraft_library::lookup::Provider;
use djcraft_library::{KeepTagged, Placement};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "settings.json";

/// Where djcraft keeps its library and settings. DJCRAFT_DATA_DIR points
/// the app somewhere else, for development and testing.
pub fn data_dir() -> PathBuf {
    std::env::var_os("DJCRAFT_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::data_dir().map(|dir| dir.join("djcraft")))
        .unwrap_or_else(std::env::temp_dir)
}

/// Where track waveforms are kept: beside the library database, so they
/// travel with it and the system does not clear them out.
pub fn waveform_dir() -> PathBuf {
    data_dir().join("waveforms")
}

/// Moves waveforms from the system cache folder, where earlier versions
/// kept them, to [`waveform_dir`]. Best effort: any that cannot be moved
/// are made again when needed. A test library (DJCRAFT_DATA_DIR) is left
/// out of it.
pub fn move_old_waveforms() {
    let new = waveform_dir();
    if std::env::var_os("DJCRAFT_DATA_DIR").is_some() || new.exists() {
        return;
    }
    let Some(old) = dirs::cache_dir().map(|dir| dir.join("djcraft").join("waveforms")) else {
        return;
    };
    if old.is_dir() && std::fs::create_dir_all(data_dir()).is_ok() {
        let _ = std::fs::rename(old, new);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    /// Follow the operating system's light or dark setting.
    System,
    #[default]
    Dark,
    Light,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 3] = [ThemeChoice::System, ThemeChoice::Dark, ThemeChoice::Light];

    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::System => "Match system",
            ThemeChoice::Dark => "Dark",
            ThemeChoice::Light => "Light",
        }
    }
}

/// The tab shown when the app opens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartTab {
    Library,
    #[default]
    Live,
    Radio,
}

impl StartTab {
    pub const ALL: [StartTab; 3] = [StartTab::Library, StartTab::Live, StartTab::Radio];

    pub fn label(self) -> &'static str {
        match self {
            StartTab::Library => "Library",
            StartTab::Live => "Live",
            StartTab::Radio => "Radio",
        }
    }
}

/// The user's preferences. Every field has a default, so a settings file
/// from an older version, or with a setting missing, still loads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    pub start_tab: StartTab,
    /// Check the library's folders for changes when the app opens.
    pub scan_on_startup: bool,
    /// Pick up changes to the library's folders while the app is open.
    pub watch_folders: bool,
    /// Where "Look up" searches for a track's details.
    pub info_provider: Provider,
    /// The tempos analysis may report a track at. A beat at 70 BPM is also
    /// a beat at 140; the range decides which is meant.
    pub tempo_min_bpm: u16,
    pub tempo_max_bpm: u16,
    /// Decimal places a tempo is shown with, 0 to 2. The tempo itself is
    /// kept exactly.
    pub bpm_decimals: u8,
    /// List a file's tagged tempo instead of the measured one.
    pub keep_tagged_tempo: bool,
    /// List a file's tagged key instead of the detected one.
    pub keep_tagged_key: bool,
    /// Where Tag tools write a track's key.
    pub key_placement: Placement,
    /// Where Tag tools write a track's energy.
    pub energy_placement: Placement,
    /// The user's own AcoustID application key, for looking tracks up by
    /// their sound.
    pub acoustid_key: String,
    /// The user's own Discogs personal access token.
    pub discogs_token: String,
    /// Columns left out of track lists, by name.
    pub hidden_columns: Vec<String>,
}

impl Global for Settings {}

/// Choices for the lowest and highest tempo. Every lowest is below every
/// highest, so no choice leaves an empty range.
pub const MIN_BPM_CHOICES: [u16; 7] = [50, 60, 70, 80, 90, 100, 110];
pub const MAX_BPM_CHOICES: [u16; 9] = [120, 130, 140, 150, 160, 170, 180, 200, 220];
pub const BPM_DECIMAL_CHOICES: [u8; 3] = [0, 1, 2];

pub fn bpm_label(bpm: u16) -> &'static str {
    match bpm {
        50 => "50 BPM",
        60 => "60 BPM",
        70 => "70 BPM",
        80 => "80 BPM",
        90 => "90 BPM",
        100 => "100 BPM",
        110 => "110 BPM",
        120 => "120 BPM",
        130 => "130 BPM",
        140 => "140 BPM",
        150 => "150 BPM",
        160 => "160 BPM",
        170 => "170 BPM",
        180 => "180 BPM",
        200 => "200 BPM",
        220 => "220 BPM",
        _ => "Other",
    }
}

pub fn bpm_decimals_label(decimals: u8) -> &'static str {
    match decimals {
        0 => "Whole numbers (128)",
        1 => "One decimal (128.0)",
        _ => "Two decimals (127.98)",
    }
}

/// A tempo as the user has chosen to see it.
pub fn format_bpm(bpm: f64, decimals: u8) -> String {
    format!("{bpm:.*}", decimals.min(2) as usize)
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::default(),
            start_tab: StartTab::default(),
            scan_on_startup: true,
            watch_folders: true,
            info_provider: Provider::default(),
            tempo_min_bpm: TempoRange::default().min_bpm as u16,
            tempo_max_bpm: TempoRange::default().max_bpm as u16,
            bpm_decimals: 1,
            keep_tagged_tempo: KeepTagged::default().tempo,
            keep_tagged_key: KeepTagged::default().key,
            key_placement: Placement::KeyField,
            energy_placement: Placement::Comment,
            acoustid_key: String::new(),
            discogs_token: String::new(),
            hidden_columns: vec!["Year".into()],
        }
    }
}

impl Settings {
    /// The tempo range analysis uses. A hand-edited file with the lowest at
    /// or above the highest falls back to the default range.
    pub fn tempo_range(&self) -> TempoRange {
        if self.tempo_min_bpm == 0 || self.tempo_min_bpm >= self.tempo_max_bpm {
            return TempoRange::default();
        }
        TempoRange {
            min_bpm: self.tempo_min_bpm as f64,
            max_bpm: self.tempo_max_bpm as f64,
        }
    }

    /// The keys online lookups use.
    pub fn lookup_keys(&self) -> djcraft_library::lookup::Keys {
        djcraft_library::lookup::Keys {
            acoustid: self.acoustid_key.trim().to_string(),
            discogs_token: self.discogs_token.trim().to_string(),
        }
    }

    pub fn keep_tagged(&self) -> KeepTagged {
        KeepTagged {
            tempo: self.keep_tagged_tempo,
            key: self.keep_tagged_key,
        }
    }

    /// Reads the settings file, falling back to defaults if it is missing
    /// or cannot be understood.
    pub fn load(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join(FILE_NAME))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        std::fs::write(dir.join(FILE_NAME), text)
    }

    pub fn apply_theme(&self, cx: &mut App) {
        match self.theme {
            ThemeChoice::System => Theme::sync_system_appearance(None, cx),
            ThemeChoice::Dark => Theme::change(ThemeMode::Dark, None, cx),
            ThemeChoice::Light => Theme::change(ThemeMode::Light, None, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_tolerate_old_files() {
        let dir = std::env::temp_dir().join(format!("djcraft-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(Settings::load(&dir), Settings::default());

        let settings = Settings {
            theme: ThemeChoice::Light,
            start_tab: StartTab::Library,
            scan_on_startup: false,
            watch_folders: false,
            info_provider: Provider::Itunes,
            tempo_min_bpm: 90,
            tempo_max_bpm: 200,
            bpm_decimals: 2,
            keep_tagged_tempo: true,
            keep_tagged_key: false,
            key_placement: Placement::TitlePrefix,
            energy_placement: Placement::Grouping,
            acoustid_key: "key".into(),
            discogs_token: "token".into(),
            hidden_columns: vec!["Album".into()],
        };
        settings.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), settings);

        // A file with only some settings, and one this version doesn't know.
        std::fs::write(dir.join(FILE_NAME), r#"{"theme":"system","future":1}"#).unwrap();
        let partial = Settings::load(&dir);
        assert_eq!(partial.theme, ThemeChoice::System);
        assert!(partial.scan_on_startup);
        assert!(partial.watch_folders);
        assert_eq!(partial.tempo_range(), TempoRange::default());

        std::fs::write(dir.join(FILE_NAME), "not json").unwrap();
        assert_eq!(Settings::load(&dir), Settings::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tempo_settings_make_sense_whatever_the_file_says() {
        let backwards = Settings {
            tempo_min_bpm: 150,
            tempo_max_bpm: 120,
            ..Settings::default()
        };
        assert_eq!(backwards.tempo_range(), TempoRange::default());
        for min in MIN_BPM_CHOICES {
            assert_ne!(bpm_label(min), "Other");
            for max in MAX_BPM_CHOICES {
                assert!(min < max);
                assert_ne!(bpm_label(max), "Other");
            }
        }
        assert_eq!(format_bpm(127.976, 0), "128");
        assert_eq!(format_bpm(127.976, 1), "128.0");
        assert_eq!(format_bpm(127.976, 2), "127.98");
        assert_eq!(format_bpm(127.976, 9), "127.98");
    }
}
