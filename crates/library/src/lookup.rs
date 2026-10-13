//! Track details from online music databases.
//!
//! Nothing here runs unless the user asks for a lookup, and results are only
//! suggestions: they are not written anywhere until the user saves them.

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// MusicBrainz asks every client to identify itself.
const USER_AGENT: &str = concat!(
    "djcraft/",
    env!("CARGO_PKG_VERSION"),
    " (open-source DJ software)"
);
const MAX_MATCHES: usize = 5;

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
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::MusicBrainz, Provider::Itunes];

    pub fn label(self) -> &'static str {
        match self {
            Provider::MusicBrainz => "MusicBrainz",
            Provider::Itunes => "iTunes Store",
        }
    }
}

/// Searches a provider for tracks matching an artist and title. Either may
/// be empty, but not both. Makes one network request.
pub fn search(provider: Provider, artist: &str, title: &str) -> Result<Vec<TrackMatch>> {
    match provider {
        Provider::MusicBrainz => musicbrainz(artist, title),
        Provider::Itunes => itunes(artist, title),
    }
}

/// One candidate for what a track is.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackMatch {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: Option<String>,
    pub genre: Option<String>,
    /// How well it matched the search, 0 to 100, if the provider says.
    pub score: Option<u8>,
}

#[derive(Deserialize)]
struct Response {
    #[serde(default)]
    recordings: Vec<Recording>,
}

#[derive(Deserialize)]
struct Recording {
    #[serde(default)]
    score: u8,
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
    request: ureq::RequestBuilder<ureq::typestate::WithoutBody>,
) -> Result<String> {
    let name = provider.label();
    request
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| Error::Lookup(format!("{name} request failed: {e}")))?
        .body_mut()
        .read_to_string()
        .map_err(|e| Error::Lookup(format!("{name} reply could not be read: {e}")))
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
    release_date: String,
    #[serde(default)]
    primary_genre_name: Option<String>,
}

fn itunes(artist: &str, title: &str) -> Result<Vec<TrackMatch>> {
    let term = format!("{} {}", artist.trim(), title.trim());
    if title.trim().is_empty() {
        return Err(Error::Lookup(
            "nothing to search for: the track has no title".into(),
        ));
    }
    let request = ureq::get("https://itunes.apple.com/search")
        .query("term", term.trim())
        .query("entity", "song")
        .query("limit", MAX_MATCHES.to_string());
    parse_itunes(&get(Provider::Itunes, request)?)
}

fn parse_itunes(body: &str) -> Result<Vec<TrackMatch>> {
    let response: ItunesResponse = serde_json::from_str(body)
        .map_err(|e| Error::Lookup(format!("iTunes Store reply was not understood: {e}")))?;
    Ok(response
        .results
        .into_iter()
        .take(MAX_MATCHES)
        .map(|song| TrackMatch {
            title: song.track_name,
            artist: song.artist_name,
            album: song.collection_name,
            year: year_of(&song.release_date),
            genre: song.primary_genre_name.filter(|genre| !genre.is_empty()),
            score: None,
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
    let request = ureq::get("https://musicbrainz.org/ws/2/recording")
        .query("query", &query)
        .query("fmt", "json")
        .query("limit", MAX_MATCHES.to_string());
    parse_musicbrainz(&get(Provider::MusicBrainz, request)?)
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
        .take(MAX_MATCHES)
        .map(|recording| TrackMatch {
            title: recording.title,
            artist: recording
                .artists
                .iter()
                .map(|credit| format!("{}{}", credit.name, credit.joinphrase))
                .collect(),
            album: recording
                .releases
                .first()
                .map(|release| release.title.clone())
                .unwrap_or_default(),
            year: year_of(&recording.first_release),
            genre: recording
                .tags
                .iter()
                .max_by_key(|tag| tag.count)
                .map(|tag| tag.name.clone()),
            score: Some(recording.score),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn reads_a_musicbrainz_reply() {
        let body = r#"{"created":"2026-01-01T00:00:00Z","count":2,"offset":0,"recordings":[
            {"id":"x","score":100,"title":"Turn Up the Speakers","length":343000,
             "artist-credit":[{"name":"Afrojack","joinphrase":" & ","artist":{"id":"a"}},
                              {"name":"Martin Garrix","artist":{"id":"b"}}],
             "first-release-date":"2014-08-25",
             "releases":[{"id":"r","title":"Turn Up the Speakers","date":"2014-08-25"}],
             "tags":[{"count":1,"name":"edm"},{"count":3,"name":"big room house"}]},
            {"id":"y","score":62,"title":"Speakers"}
        ]}"#;
        let matches = parse_musicbrainz(body).unwrap();
        assert_eq!(
            matches[0],
            TrackMatch {
                title: "Turn Up the Speakers".into(),
                artist: "Afrojack & Martin Garrix".into(),
                album: "Turn Up the Speakers".into(),
                year: Some("2014".into()),
                genre: Some("big room house".into()),
                score: Some(100),
            }
        );
        assert_eq!(matches[1].artist, "");
        assert_eq!(matches[1].year, None);
        assert!(parse_musicbrainz("<html>busy</html>").is_err());
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
                score: None,
            }]
        );
        assert!(itunes("Someone", " ").is_err());
    }
}
