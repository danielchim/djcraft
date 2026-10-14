//! Track details from online music databases.
//!
//! Nothing here runs unless the user asks for a lookup, and results are only
//! suggestions: they are not written anywhere until the user saves them.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// MusicBrainz asks every client to identify itself.
const USER_AGENT: &str = concat!(
    "djcraft/",
    env!("CARGO_PKG_VERSION"),
    " (open-source DJ software)"
);
/// Results shown for one search.
const MAX_MATCHES: usize = 10;

/// An online database that track details can be looked up in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// The community-run music encyclopedia. Wide coverage, including
    /// obscure releases; albums are often compilations and genres are rare.
    #[default]
    MusicBrainz,
    /// Apple's store catalogue. Clean album names and a genre for every
    /// track, but only music that Apple sells.
    Itunes,
    /// Identifies a track by its sound, whatever its tags say. Needs an
    /// application key, and is used with [`identify`] rather than
    /// [`search`].
    AcoustId,
}

impl Provider {
    /// Those that search by artist and title.
    pub const ALL: [Provider; 2] = [Provider::MusicBrainz, Provider::Itunes];

    pub fn label(self) -> &'static str {
        match self {
            Provider::MusicBrainz => "MusicBrainz",
            Provider::Itunes => "iTunes Store",
            Provider::AcoustId => "AcoustID (by sound)",
        }
    }
}

/// Searches a provider for tracks matching an artist and title. Either may
/// be empty, but not both.
///
/// Tags are often untidy: a track number or "Artist - Title" in the title, a
/// mix name, several artists. The search uses a tidied version (see
/// [`search_terms`]), retrying without the artist if that finds nothing, and
/// the results are ranked by how well they match the text as given, mix
/// name included. Makes one to three network requests.
pub fn search(provider: Provider, artist: &str, title: &str) -> Result<Vec<TrackMatch>> {
    let (wanted_artist, wanted_title) = split_artist(artist, title);
    let (query_artist, query_title) = search_terms(artist, title);
    if query_title.is_empty() {
        return Err(Error::Lookup(
            "nothing to search for: the track has no title".into(),
        ));
    }
    let find = |artist: &str| match provider {
        Provider::MusicBrainz => musicbrainz(artist, &query_title),
        Provider::Itunes => itunes(artist, &query_title),
        Provider::AcoustId => Err(Error::Lookup(
            "AcoustID identifies tracks by their sound, not by name.".into(),
        )),
    };
    let mut found = find(&query_artist)?;
    if found.is_empty() && !query_artist.is_empty() {
        // The artist may be spelled differently there; the ranking below
        // still favours the right one.
        if provider == Provider::MusicBrainz {
            std::thread::sleep(MUSICBRAINZ_INTERVAL);
        }
        found = find("")?;
    }
    Ok(rank(found, &wanted_artist, &wanted_title))
}

/// The credentials some providers need, as the user entered them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Keys {
    /// An AcoustID application key, from acoustid.org.
    pub acoustid: String,
}

/// Identifies a track from its acoustic fingerprint (as made by
/// `djcraft_analysis::fingerprint`) and its length in seconds. The best
/// matches come first. Makes one network request.
pub fn identify(
    api_key: &str,
    fingerprint: &str,
    duration_seconds: u32,
) -> Result<Vec<TrackMatch>> {
    if api_key.trim().is_empty() {
        return Err(Error::Lookup(
            "AcoustID needs an application key. Add one in Settings, Library.".into(),
        ));
    }
    let body = get(Provider::AcoustId, || {
        // AcoustID explains a refusal (a bad key, say) in the body of an
        // error reply, so error replies are read like any other.
        ureq::get("https://api.acoustid.org/v2/lookup")
            .config()
            .timeout_global(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .build()
            .query("client", api_key.trim())
            .query("meta", "recordings releasegroups compress")
            .query("duration", duration_seconds.to_string())
            .query("fingerprint", fingerprint)
    })?;
    parse_acoustid(&body)
}

#[derive(Deserialize)]
struct AcoustIdResponse {
    status: String,
    #[serde(default)]
    error: Option<AcoustIdError>,
    #[serde(default)]
    results: Vec<AcoustIdResult>,
}

#[derive(Deserialize)]
struct AcoustIdError {
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct AcoustIdResult {
    #[serde(default)]
    score: f64,
    #[serde(default)]
    recordings: Vec<AcoustIdRecording>,
}

#[derive(Deserialize)]
struct AcoustIdRecording {
    #[serde(default)]
    title: String,
    #[serde(default)]
    artists: Vec<ArtistCredit>,
    #[serde(default)]
    releasegroups: Vec<AcoustIdGroup>,
}

#[derive(Deserialize)]
struct AcoustIdGroup {
    #[serde(default)]
    title: String,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    secondarytypes: Vec<String>,
}

fn parse_acoustid(body: &str) -> Result<Vec<TrackMatch>> {
    let response: AcoustIdResponse = serde_json::from_str(body)
        .map_err(|e| Error::Lookup(format!("AcoustID reply was not understood: {e}")))?;
    if response.status != "ok" {
        let message = response.error.map(|e| e.message).unwrap_or_default();
        return Err(Error::Lookup(format!(
            "AcoustID refused the lookup: {message}"
        )));
    }
    let mut found = Vec::new();
    for result in response.results {
        for recording in result.recordings {
            if recording.title.is_empty() {
                continue;
            }
            // The artist's own album, single or EP, before compilations.
            let original = |group: &&AcoustIdGroup| {
                group.secondarytypes.is_empty()
                    && matches!(group.kind.as_deref(), Some("Album" | "Single" | "EP"))
            };
            let group = recording
                .releasegroups
                .iter()
                .find(original)
                .or(recording.releasegroups.first());
            found.push(TrackMatch {
                title: recording.title,
                artist: recording
                    .artists
                    .iter()
                    .map(|credit| format!("{}{}", credit.name, credit.joinphrase))
                    .collect(),
                album: group.map(|group| group.title.clone()).unwrap_or_default(),
                year: None,
                genre: None,
                score: (result.score * 100.0).round().clamp(0.0, 100.0) as u8,
                compilation: !group.is_some_and(|group| original(&group)),
                provider: Provider::AcoustId,
            });
        }
    }
    found.sort_by_key(|candidate| std::cmp::Reverse(candidate.score));
    let mut seen = std::collections::HashSet::new();
    found.retain(|candidate| {
        seen.insert((
            words(&candidate.title),
            words(&candidate.artist),
            words(&candidate.album),
        ))
    });
    found.truncate(MAX_MATCHES);
    Ok(found)
}

/// The artist and title to search for, tidied from what a file's tags say:
/// a leading track number, "Artist - Title" in the title, mix names and
/// featured artists are taken out, and only the first of several artists
/// is kept.
pub fn search_terms(artist: &str, title: &str) -> (String, String) {
    let (artist, title) = split_artist(artist, title);
    (main_artist(&artist), bare_title(&title))
}

/// One candidate for what a track is.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackMatch {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: Option<String>,
    pub genre: Option<String>,
    /// How well it matches what was searched for, 0 to 100.
    pub score: u8,
    /// The album is a compilation, DJ mix or similar rather than a release
    /// of the artist's own.
    pub compilation: bool,
    pub provider: Provider,
}

/// MusicBrainz allows one request a second from each client.
const MUSICBRAINZ_INTERVAL: Duration = Duration::from_millis(1100);
/// Results asked of a provider: more than are shown, so that duplicates and
/// poor matches can be dropped. MusicBrainz lists a track once for every
/// compilation it is on, so the original release can be far down its list.
const ITUNES_FETCHED: usize = 25;
const MUSICBRAINZ_FETCHED: usize = 100;
/// Taken off the score of a match whose album is a compilation, so that the
/// original release of the same track comes first.
const COMPILATION_PENALTY: u8 = 8;

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    recordings: Vec<Recording>,
}

#[derive(Deserialize)]
struct Recording {
    #[serde(default)]
    title: String,
    #[serde(default, rename = "artist-credit")]
    artists: Vec<ArtistCredit>,
    #[serde(default, rename = "first-release-date")]
    first_release: String,
    #[serde(default)]
    releases: Vec<Release>,
    #[serde(default)]
    tags: Vec<Tag>,
}

#[derive(Deserialize)]
struct ArtistCredit {
    #[serde(default)]
    name: String,
    #[serde(default)]
    joinphrase: String,
}

#[derive(Deserialize)]
struct Release {
    #[serde(default)]
    title: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    date: String,
    #[serde(default, rename = "release-group")]
    group: Option<ReleaseGroup>,
}

#[derive(Deserialize)]
struct ReleaseGroup {
    #[serde(default, rename = "primary-type")]
    primary_type: Option<String>,
    #[serde(default, rename = "secondary-types")]
    secondary_types: Vec<String>,
}

#[derive(Deserialize)]
struct Tag {
    #[serde(default)]
    count: i64,
    #[serde(default)]
    name: String,
}

fn get(
    provider: Provider,
    request: impl Fn() -> ureq::RequestBuilder<ureq::typestate::WithoutBody>,
) -> Result<String> {
    let name = provider.label();
    let send = || request().header("User-Agent", USER_AGENT).call();
    let response = match send() {
        // Busy, or asked to slow down: wait a moment and try once more.
        Err(ureq::Error::StatusCode(429 | 503)) => {
            std::thread::sleep(MUSICBRAINZ_INTERVAL * 2);
            send()
        }
        other => other,
    };
    let describe = |e: ureq::Error| match e {
        ureq::Error::StatusCode(429 | 503) => {
            format!("{name} is busy. Try again in a few seconds.")
        }
        ureq::Error::StatusCode(401 | 403) => {
            format!("{name} did not accept the key in Settings. Check it and try again.")
        }
        ureq::Error::StatusCode(code) => format!("{name} answered with an error ({code})."),
        ureq::Error::Timeout(_) => format!("{name} did not answer in time."),
        other => format!("{name} could not be reached: {other}"),
    };
    response
        .map_err(|e| Error::Lookup(describe(e)))?
        .body_mut()
        .read_to_string()
        .map_err(|e| Error::Lookup(format!("{name} reply could not be read: {e}")))
}

fn agent_request(url: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
    ureq::get(url)
        .config()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
}

#[derive(Deserialize)]
struct ItunesResponse {
    #[serde(default)]
    results: Vec<ItunesSong>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItunesSong {
    #[serde(default)]
    track_name: String,
    #[serde(default)]
    artist_name: String,
    #[serde(default)]
    collection_name: String,
    #[serde(default)]
    collection_artist_name: Option<String>,
    #[serde(default)]
    release_date: String,
    #[serde(default)]
    primary_genre_name: Option<String>,
}

fn itunes(artist: &str, title: &str) -> Result<Vec<TrackMatch>> {
    let term = format!("{artist} {title}");
    let body = get(Provider::Itunes, || {
        agent_request("https://itunes.apple.com/search")
            .query("term", term.trim())
            .query("entity", "song")
            .query("limit", ITUNES_FETCHED.to_string())
    })?;
    parse_itunes(&body)
}

fn parse_itunes(body: &str) -> Result<Vec<TrackMatch>> {
    let response: ItunesResponse = serde_json::from_str(body)
        .map_err(|e| Error::Lookup(format!("iTunes Store reply was not understood: {e}")))?;
    Ok(response
        .results
        .into_iter()
        .filter(|song| !song.track_name.is_empty())
        .map(|song| TrackMatch {
            title: song.track_name,
            artist: song.artist_name,
            album: song.collection_name,
            year: year_of(&song.release_date),
            genre: song.primary_genre_name.filter(|genre| !genre.is_empty()),
            score: 0,
            compilation: song.collection_artist_name.as_deref() == Some("Various Artists"),
            provider: Provider::Itunes,
        })
        .collect())
}

/// The year at the start of a date such as `2014-08-25`.
fn year_of(date: &str) -> Option<String> {
    date.get(..4)
        .filter(|year| year.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_string)
}

fn musicbrainz(artist: &str, title: &str) -> Result<Vec<TrackMatch>> {
    let query = search_query(artist, title)
        .ok_or_else(|| Error::Lookup("nothing to search for: the track has no title".into()))?;
    let body = get(Provider::MusicBrainz, || {
        agent_request("https://musicbrainz.org/ws/2/recording")
            .query("query", &query)
            .query("fmt", "json")
            .query("limit", MUSICBRAINZ_FETCHED.to_string())
    })?;
    parse_musicbrainz(&body)
}

/// A Lucene query for the recording search, with user text quoted.
fn search_query(artist: &str, title: &str) -> Option<String> {
    let quoted = |text: &str| format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""));
    let (artist, title) = (artist.trim(), title.trim());
    match (artist.is_empty(), title.is_empty()) {
        (_, true) => None,
        (true, false) => Some(format!("recording:{}", quoted(title))),
        (false, false) => Some(format!(
            "recording:{} AND artist:{}",
            quoted(title),
            quoted(artist)
        )),
    }
}

fn parse_musicbrainz(body: &str) -> Result<Vec<TrackMatch>> {
    let response: Response = serde_json::from_str(body)
        .map_err(|e| Error::Lookup(format!("MusicBrainz reply was not understood: {e}")))?;
    Ok(response
        .recordings
        .into_iter()
        .filter(|recording| !recording.title.is_empty())
        .map(|recording| {
            let release = original_release(&recording.releases);
            let year = year_of(&recording.first_release)
                .or_else(|| release.and_then(|release| year_of(&release.date)));
            TrackMatch {
                title: recording.title,
                artist: recording
                    .artists
                    .iter()
                    .map(|credit| format!("{}{}", credit.name, credit.joinphrase))
                    .collect(),
                album: release
                    .map(|release| release.title.clone())
                    .unwrap_or_default(),
                year,
                genre: recording
                    .tags
                    .iter()
                    .max_by_key(|tag| tag.count)
                    .map(|tag| tag.name.clone()),
                score: 0,
                compilation: !release.is_some_and(is_original),
                provider: Provider::MusicBrainz,
            }
        })
        .collect())
}

/// The release a recording most likely first came out on: an official
/// album, single or EP of the artist's own rather than a compilation, a DJ
/// mix or a soundtrack, and the earliest of those.
fn original_release(releases: &[Release]) -> Option<&Release> {
    releases.iter().min_by_key(|release| {
        let group = release.group.as_ref();
        let official = release.status.as_deref() == Some("Official");
        let plain = group.is_some_and(|group| group.secondary_types.is_empty());
        // Undated releases sort last.
        let date = if release.date.is_empty() {
            "9999".to_string()
        } else {
            release.date.clone()
        };
        (!official, !plain, !own_release_type(release), date)
    })
}

/// An official album, single or EP with no secondary type such as
/// compilation, DJ mix, soundtrack or live.
fn is_original(release: &Release) -> bool {
    release.status.as_deref() == Some("Official")
        && release
            .group
            .as_ref()
            .is_some_and(|group| group.secondary_types.is_empty())
        && own_release_type(release)
}

fn own_release_type(release: &Release) -> bool {
    release
        .group
        .as_ref()
        .and_then(|group| group.primary_type.as_deref())
        .is_some_and(|kind| matches!(kind, "Album" | "Single" | "EP"))
}

/// Words in brackets that mark a version of a track rather than its name.
const VERSION_WORDS: &[&str] = &[
    "mix",
    "edit",
    "remix",
    "version",
    "remaster",
    "remastered",
    "feat",
    "ft",
    "featuring",
    "original",
    "extended",
    "radio",
    "club",
    "dub",
    "vip",
    "bootleg",
    "free",
    "download",
    "explicit",
    "clean",
    "instrumental",
    "acapella",
    "rework",
    "flip",
    "live",
    "intro",
];

/// Separators between several artists, after the first one.
const ARTIST_SEPARATORS: &[&str] = &[
    ",",
    " & ",
    " and ",
    " x ",
    " vs ",
    " vs. ",
    " feat ",
    " feat. ",
    " ft ",
    " ft. ",
    " featuring ",
    " with ",
    ";",
    "/",
];

/// Takes the artist out of a title written "Artist - Title" when the artist
/// tag is empty, and a leading track number off the title.
fn split_artist(artist: &str, title: &str) -> (String, String) {
    let title = without_track_number(&title.replace('_', " "));
    let artist = artist.trim();
    if artist.is_empty()
        && let Some((left, right)) = title.split_once(" - ")
        && !left.trim().is_empty()
        && !right.trim().is_empty()
    {
        return (left.trim().to_string(), right.trim().to_string());
    }
    (artist.to_string(), title.trim().to_string())
}

/// "01 Title", "01. Title", "1 - Title" → "Title". A number that is part of
/// the name, as in "99 Problems", is kept: only a zero-padded number, or
/// one followed by a dot, dash or underscore, counts as a track number.
fn without_track_number(title: &str) -> String {
    let title = title.trim();
    let digits = title.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 3 || digits == title.len() {
        return title.to_string();
    }
    let rest = &title[digits..];
    let separated = rest.trim_start().starts_with(['.', '-', '_', ')']);
    let padded = title.starts_with('0') && rest.starts_with(' ');
    if separated || padded {
        let rest = rest.trim_start().trim_start_matches(['.', '-', '_', ')']);
        if !rest.trim().is_empty() {
            return rest.trim().to_string();
        }
    }
    title.to_string()
}

/// The first of several credited artists.
fn main_artist(artist: &str) -> String {
    let lower = artist.to_lowercase();
    let cut = ARTIST_SEPARATORS
        .iter()
        .filter_map(|separator| lower.find(separator))
        .filter(|at| *at > 0)
        .min()
        .unwrap_or(artist.len());
    artist[..cut].trim().to_string()
}

/// A title without bracketed version names ("(Original Mix)", "[Radio
/// Edit]") or a trailing "feat. Someone".
fn bare_title(title: &str) -> String {
    let mut bare = String::new();
    let mut rest = title;
    while let Some(open) = rest.find(['(', '[']) {
        let close = if rest[open..].starts_with('(') {
            ')'
        } else {
            ']'
        };
        let Some(length) = rest[open..].find(close) else {
            break;
        };
        let inside = &rest[open + 1..open + length];
        let is_version = words(inside)
            .iter()
            .any(|word| VERSION_WORDS.contains(&word.as_str()));
        bare.push_str(&rest[..open]);
        if !is_version {
            bare.push_str(&rest[open..=open + length]);
        }
        rest = &rest[open + length + 1..];
    }
    bare.push_str(rest);
    let lower = bare.to_lowercase();
    let cut = [" feat. ", " feat ", " ft. ", " ft ", " featuring "]
        .iter()
        .filter_map(|marker| lower.find(marker))
        .min()
        .unwrap_or(bare.len());
    bare[..cut].split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lowercase words, without punctuation, for comparing names.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect()
}

/// How alike two names are, 0.0 to 1.0, by the words they share.
fn similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (words(a), words(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let shared = a.iter().filter(|word| b.contains(word)).count();
    let shared_back = b.iter().filter(|word| a.contains(word)).count();
    (shared + shared_back) as f64 / (a.len() + b.len()) as f64
}

/// Scores each candidate against what was asked for, drops repeats, and
/// keeps the best, best first.
fn rank(found: Vec<TrackMatch>, artist: &str, title: &str) -> Vec<TrackMatch> {
    let mut scored: Vec<TrackMatch> = found
        .into_iter()
        .map(|mut candidate| {
            // The title as tagged, mix name and all, so the right version
            // of a track wins; the bare title as well, so a plain one is
            // not punished for a mix name the tags added.
            let title_score = similarity(title, &candidate.title)
                .max(similarity(&bare_title(title), &candidate.title) * 0.95);
            let score = if artist.trim().is_empty() {
                title_score
            } else {
                // Any credited artist will do: tags and databases often
                // list them in a different order.
                let artist_score = similarity(artist, &candidate.artist)
                    .max(similarity(&main_artist(artist), &candidate.artist));
                0.6 * title_score + 0.4 * artist_score
            };
            candidate.score = (score * 100.0).round() as u8;
            if candidate.compilation {
                candidate.score = candidate.score.saturating_sub(COMPILATION_PENALTY);
            }
            candidate
        })
        .collect();
    // Stable, so equal scores keep the provider's own order.
    scored.sort_by_key(|candidate| std::cmp::Reverse(candidate.score));
    let mut seen = std::collections::HashSet::new();
    scored.retain(|candidate| {
        seen.insert((
            words(&candidate.title),
            words(&candidate.artist),
            words(&candidate.album),
        ))
    });
    scored.truncate(MAX_MATCHES);
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(title: &str, artist: &str, album: &str) -> TrackMatch {
        TrackMatch {
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            year: None,
            genre: None,
            score: 0,
            compilation: false,
            provider: Provider::Itunes,
        }
    }

    #[test]
    fn builds_a_quoted_query() {
        assert_eq!(search_query("", " "), None);
        assert_eq!(
            search_query("", "One").as_deref(),
            Some(r#"recording:"One""#)
        );
        assert_eq!(
            search_query("AC/DC", r#"Say "Hi""#).as_deref(),
            Some(r#"recording:"Say \"Hi\"" AND artist:"AC/DC""#)
        );
    }

    #[test]
    fn tidies_tags_into_search_terms() {
        let terms = |artist: &str, title: &str| {
            let (artist, title) = search_terms(artist, title);
            (artist.leak() as &str, title.leak() as &str)
        };
        assert_eq!(
            terms("Daft Punk", "One More Time (Extended Mix)"),
            ("Daft Punk", "One More Time")
        );
        assert_eq!(
            terms(
                "Afrojack, Martin Garrix",
                "Turn Up The Speakers [Original Mix]"
            ),
            ("Afrojack", "Turn Up The Speakers")
        );
        assert_eq!(
            terms("Avicii feat. Aloe Blacc", "Wake Me Up"),
            ("Avicii", "Wake Me Up")
        );
        assert_eq!(
            terms("", "01 Daft Punk - Around the World"),
            ("Daft Punk", "Around the World")
        );
        assert_eq!(terms("", "07. Losing_It"), ("", "Losing It"));
        assert_eq!(
            terms("Calvin Harris", "Summer ft. Someone"),
            ("Calvin Harris", "Summer")
        );
        // Numbers and brackets that belong to the name stay.
        assert_eq!(terms("Jay-Z", "99 Problems"), ("Jay-Z", "99 Problems"));
        assert_eq!(terms("", "1999"), ("", "1999"));
        assert_eq!(
            terms("Prince", "Purple Rain (Part 2)"),
            ("Prince", "Purple Rain (Part 2)")
        );
        assert_eq!(terms("Simon & Garfunkel", "Mrs. Robinson").0, "Simon");
        assert!(search(Provider::Itunes, "Someone", " ").is_err());
        // A provider that needs a key says so before asking anything online.
        assert!(identify("", "AQAA", 120).is_err());
    }

    #[test]
    fn the_closest_match_comes_first_and_repeats_go() {
        let ranked = rank(
            vec![
                found("ONE MORE TIME", "blink-182", "ONE MORE TIME..."),
                found(
                    "One More Time (Radio Edit)",
                    "Daft Punk",
                    "One More Time - Single",
                ),
                found("One More Time", "Daft Punk", "Discovery"),
                found("One More Time", "Daft Punk", "Discovery"),
            ],
            "Daft Punk",
            "One More Time (Extended Mix)",
        );
        let order: Vec<_> = ranked
            .iter()
            .map(|m| (m.artist.as_str(), m.album.as_str()))
            .collect();
        assert_eq!(
            order,
            [
                ("Daft Punk", "Discovery"),
                ("Daft Punk", "One More Time - Single"),
                ("blink-182", "ONE MORE TIME...")
            ]
        );
        assert!(ranked[0].score > 80, "{}", ranked[0].score);
        assert!(ranked[2].score < 70, "{}", ranked[2].score);

        // A remix in the tags picks out that remix.
        let ranked = rank(
            vec![
                found("Turn Up the Speakers", "AFROJACK & Martin Garrix", "Single"),
                found(
                    "Turn Up The Speakers (Julian Jordan Remix)",
                    "AFROJACK & Martin Garrix",
                    "Remixes",
                ),
            ],
            "Afrojack, Martin Garrix",
            "Turn Up The Speakers (Julian Jordan Remix)",
        );
        assert_eq!(ranked[0].album, "Remixes");

        // Of two copies of the same track, the original release wins.
        let mut on_compilation = found("Losing It", "FISHER", "Hits 2019");
        on_compilation.compilation = true;
        let ranked = rank(
            vec![on_compilation, found("Losing It", "Fisher", "Losing It")],
            "Fisher",
            "Losing It",
        );
        assert_eq!(ranked[0].album, "Losing It");
        assert_eq!(ranked[1].score, ranked[0].score - COMPILATION_PENALTY);
    }

    #[test]
    fn reads_a_musicbrainz_reply_and_prefers_the_original_release() {
        let body = r#"{"created":"2026-01-01T00:00:00Z","count":2,"offset":0,"recordings":[
            {"id":"x","score":100,"title":"Turn Up the Speakers","length":343000,
             "artist-credit":[{"name":"Afrojack","joinphrase":" & ","artist":{"id":"a"}},
                              {"name":"Martin Garrix","artist":{"id":"b"}}],
             "first-release-date":"2014-08-25",
             "releases":[
               {"id":"c","title":"Dance Hits 2014","status":"Official","date":"2014-09-01",
                "release-group":{"primary-type":"Album","secondary-types":["Compilation"]}},
               {"id":"b","title":"Bootleg Tape","status":"Bootleg","date":"2014-08-01",
                "release-group":{"primary-type":"Album"}},
               {"id":"r","title":"Turn Up the Speakers","status":"Official","date":"2014-08-25",
                "release-group":{"primary-type":"Single"}}],
             "tags":[{"count":1,"name":"edm"},{"count":3,"name":"big room house"}]},
            {"id":"y","score":62,"title":"Speakers"},
            {"id":"z","score":50,"title":""}
        ]}"#;
        let matches = parse_musicbrainz(body).unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(
            matches[0],
            TrackMatch {
                title: "Turn Up the Speakers".into(),
                artist: "Afrojack & Martin Garrix".into(),
                album: "Turn Up the Speakers".into(),
                year: Some("2014".into()),
                genre: Some("big room house".into()),
                score: 0,
                compilation: false,
                provider: Provider::MusicBrainz,
            }
        );
        // The second has no release at all, so nothing says it is original.
        assert!(matches[1].compilation);
        assert_eq!(matches[1].artist, "");
        assert_eq!(matches[1].year, None);
        assert!(parse_musicbrainz("<html>busy</html>").is_err());
    }

    #[test]
    fn reads_an_acoustid_reply() {
        let body = r#"{"status":"ok","results":[
            {"id":"a","score":0.95,"recordings":[
                {"id":"r1","title":"Losing It","artists":[{"name":"FISHER"}],
                 "releasegroups":[
                    {"title":"Hits 2019","type":"Album","secondarytypes":["Compilation"]},
                    {"title":"Losing It","type":"Single"}]},
                {"id":"r2","title":"Losing It","artists":[{"name":"FISHER"}],
                 "releasegroups":[{"title":"Hits 2019","type":"Album","secondarytypes":["Compilation"]}]}]},
            {"id":"b","score":0.4,"recordings":[{"id":"r3","title":""}]}
        ]}"#;
        let found = parse_acoustid(body).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(
            (found[0].album.as_str(), found[0].compilation),
            ("Losing It", false)
        );
        assert_eq!(
            (found[1].album.as_str(), found[1].compilation),
            ("Hits 2019", true)
        );
        assert_eq!(found[0].score, 95);
        assert_eq!(found[0].provider, Provider::AcoustId);

        let refused = r#"{"status":"error","error":{"code":4,"message":"invalid API key"}}"#;
        assert!(
            parse_acoustid(refused)
                .unwrap_err()
                .to_string()
                .contains("invalid API key")
        );
    }

    #[test]
    fn reads_an_itunes_reply() {
        let body = r#"{"resultCount":1,"results":[
            {"wrapperType":"track","kind":"song","artistName":"Afrojack & Martin Garrix",
             "collectionName":"Turn Up the Speakers - Single","trackName":"Turn Up the Speakers",
             "releaseDate":"2014-08-25T07:00:00Z","primaryGenreName":"Dance","trackTimeMillis":343000}
        ]}"#;
        assert_eq!(
            parse_itunes(body).unwrap(),
            [TrackMatch {
                title: "Turn Up the Speakers".into(),
                artist: "Afrojack & Martin Garrix".into(),
                album: "Turn Up the Speakers - Single".into(),
                year: Some("2014".into()),
                genre: Some("Dance".into()),
                score: 0,
                compilation: false,
                provider: Provider::Itunes,
            }]
        );
        let various = body.replace(
            r#""kind":"song","#,
            r#""kind":"song","collectionArtistName":"Various Artists","#,
        );
        assert!(parse_itunes(&various).unwrap()[0].compilation);
    }
}
