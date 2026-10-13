use std::io;
use std::path::{Path, PathBuf};

use djcraft_library::lookup::Provider;
use gpui_kit::App;
use gpui_kit::component::{Theme, ThemeMode};
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
    /// Where "Look up" searches for a track's details.
    pub info_provider: Provider,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::default(),
            start_tab: StartTab::default(),
            scan_on_startup: true,
            info_provider: Provider::default(),
        }
    }
}

impl Settings {
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
            info_provider: Provider::Itunes,
        };
        settings.save(&dir).unwrap();
        assert_eq!(Settings::load(&dir), settings);

        // A file with only some settings, and one this version doesn't know.
        std::fs::write(dir.join(FILE_NAME), r#"{"theme":"system","future":1}"#).unwrap();
        let partial = Settings::load(&dir);
        assert_eq!(partial.theme, ThemeChoice::System);
        assert!(partial.scan_on_startup);

        std::fs::write(dir.join(FILE_NAME), "not json").unwrap();
        assert_eq!(Settings::load(&dir), Settings::default());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
