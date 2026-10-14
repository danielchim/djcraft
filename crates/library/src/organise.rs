//! Organising the library: automatic groups of tracks, by genre, energy
//! and decade.

use std::collections::HashMap;

use crate::{Library, Result, TrackQuery};

/// A range of energy levels, for grouping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EnergyBand {
    /// 1 to 3.
    Low,
    /// 4 to 6.
    Medium,
    /// 7 to 10.
    High,
}

impl EnergyBand {
    pub const ALL: [EnergyBand; 3] = [EnergyBand::Low, EnergyBand::Medium, EnergyBand::High];

    fn range(self) -> (u8, u8) {
        match self {
            EnergyBand::Low => (1, 3),
            EnergyBand::Medium => (4, 6),
            EnergyBand::High => (7, 10),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EnergyBand::Low => "Low energy (1–3)",
            EnergyBand::Medium => "Medium energy (4–6)",
            EnergyBand::High => "High energy (7–10)",
        }
    }
}

/// An automatic group of tracks, made from their tags and analysis.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    Genre(String),
    Energy(EnergyBand),
    /// The decade's first year, such as 1990.
    Decade(i32),
}

impl Group {
    pub fn label(&self) -> String {
        match self {
            Group::Genre(genre) => genre.clone(),
            Group::Energy(band) => band.label().to_string(),
            Group::Decade(decade) => format!("{decade}s"),
        }
    }

    /// An SQL condition over `tracks t`, with parameters numbered from
    /// `first`, and the values for them.
    pub(crate) fn sql(&self, first: usize) -> (String, Vec<rusqlite::types::Value>) {
        match self {
            Group::Genre(genre) => (
                format!(" AND TRIM(t.genre) = ?{first} COLLATE NOCASE"),
                vec![genre.clone().into()],
            ),
            Group::Energy(band) => {
                let (low, high) = band.range();
                (
                    format!(" AND t.energy BETWEEN ?{first} AND ?{}", first + 1),
                    vec![(low as i64).into(), (high as i64).into()],
                )
            }
            Group::Decade(decade) => (
                format!(" AND t.year BETWEEN ?{first} AND ?{}", first + 1),
                vec![(*decade as i64).into(), (*decade as i64 + 9).into()],
            ),
        }
    }
}

/// A group and how many tracks it has.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupCount {
    pub group: Group,
    pub count: usize,
}

impl Library {
    /// The automatic groups the library's tracks fall into: genres, energy
    /// bands and decades, each with its number of tracks. Groups with no
    /// tracks are left out.
    pub fn groups(&self) -> Result<Vec<GroupCount>> {
        let tracks = self.tracks(&TrackQuery::default())?;
        let mut genres: HashMap<String, (String, usize)> = HashMap::new();
        let mut bands: HashMap<EnergyBand, usize> = HashMap::new();
        let mut decades: HashMap<i32, usize> = HashMap::new();
        for track in &tracks {
            let genre = track.genre.trim();
            if !genre.is_empty() {
                // Spelled however its first track spells it.
                genres
                    .entry(genre.to_lowercase())
                    .or_insert_with(|| (genre.to_string(), 0))
                    .1 += 1;
            }
            if let Some(energy) = track.energy {
                let band = EnergyBand::ALL
                    .into_iter()
                    .find(|band| (band.range().0..=band.range().1).contains(&energy));
                if let Some(band) = band {
                    *bands.entry(band).or_default() += 1;
                }
            }
            if let Some(year) = track.year {
                *decades.entry(year / 10 * 10).or_default() += 1;
            }
        }
        let mut groups = Vec::new();
        let mut genres: Vec<_> = genres.into_values().collect();
        genres.sort_by_key(|(name, _)| name.to_lowercase());
        groups.extend(genres.into_iter().map(|(name, count)| GroupCount {
            group: Group::Genre(name),
            count,
        }));
        groups.extend(EnergyBand::ALL.into_iter().filter_map(|band| {
            bands.get(&band).map(|count| GroupCount {
                group: Group::Energy(band),
                count: *count,
            })
        }));
        let mut decades: Vec<_> = decades.into_iter().collect();
        decades.sort();
        groups.extend(decades.into_iter().map(|(decade, count)| GroupCount {
            group: Group::Decade(decade),
            count,
        }));
        Ok(groups)
    }
}
