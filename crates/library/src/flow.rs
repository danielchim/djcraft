//! Flows: sets planned on a timeline.
//!
//! A flow is a number of lanes running side by side, like decks. Each clip
//! is a section of one track placed on a lane at a time. Where clips on
//! different lanes overlap, one track is being mixed into the other.

use rusqlite::{OptionalExtension, params};

use crate::{Camelot, CuePoint, Error, Library, Result, TempoResult, Track, TrackQuery};

/// As many lanes as there are decks to play them on.
pub const FLOW_LANES: usize = 4;
/// The shortest a clip can be trimmed to, in seconds.
pub const MIN_CLIP_SECONDS: f64 = 1.0;
/// How far a clip may be slowed down or sped up.
pub const MIN_CLIP_SPEED: f64 = 0.5;
pub const MAX_CLIP_SPEED: f64 = 2.0;
/// How far a clip's key may be shifted either way, in semitones.
pub const MAX_KEY_SHIFT: i32 = 12;

#[derive(Clone, Debug, PartialEq)]
pub struct Flow {
    pub id: i64,
    pub name: String,
    pub clip_count: usize,
}

/// A section of a track on the timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct FlowClip {
    pub id: i64,
    /// 0 to `FLOW_LANES - 1`.
    pub lane: usize,
    /// When the clip starts, in seconds from the start of the flow.
    pub start: f64,
    /// Where in the track the clip begins, in seconds.
    pub offset: f64,
    /// How much of the track the clip uses, in seconds of the track.
    pub length: f64,
    /// How fast the clip plays: 1.0 is the track's own tempo, 1.05 is 5%
    /// faster. A faster clip takes up less of the timeline.
    pub speed: f64,
    /// How many semitones the clip is shifted up (or, if negative, down).
    pub key_shift: i32,
    pub track: Track,
    /// The track's measured beat grid, for snapping to bars.
    pub grid: Option<TempoResult>,
    /// The track's cue points, in track time.
    pub cues: Vec<CuePoint>,
}

impl FlowClip {
    /// How long the clip lasts on the timeline, in seconds.
    pub fn duration(&self) -> f64 {
        self.length / self.speed
    }

    pub fn end(&self) -> f64 {
        self.start + self.duration()
    }

    /// The clip's tempo as played: the track's tempo times the clip's speed.
    pub fn bpm(&self) -> Option<f64> {
        self.track.bpm.map(|bpm| bpm * self.speed)
    }

    /// The clip's key as played: the track's key, shifted.
    pub fn camelot(&self) -> Option<Camelot> {
        self.track.camelot.map(|key| key.shifted(self.key_shift))
    }

    pub fn placement(&self) -> ClipPlacement {
        ClipPlacement {
            lane: self.lane,
            start: self.start,
            offset: self.offset,
            length: self.length,
            speed: self.speed,
        }
    }
}

/// Where a clip sits; everything about a clip that can be edited.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipPlacement {
    pub lane: usize,
    pub start: f64,
    pub offset: f64,
    pub length: f64,
    pub speed: f64,
}

impl ClipPlacement {
    /// Brings a placement within what the track and timeline allow.
    fn fitted(self, track_seconds: f64) -> Self {
        let offset = self
            .offset
            .clamp(0.0, (track_seconds - MIN_CLIP_SECONDS).max(0.0));
        Self {
            lane: self.lane.min(FLOW_LANES - 1),
            start: self.start.max(0.0),
            speed: if self.speed.is_finite() {
                self.speed.clamp(MIN_CLIP_SPEED, MAX_CLIP_SPEED)
            } else {
                1.0
            },
            offset,
            length: self.length.clamp(
                MIN_CLIP_SECONDS.min(track_seconds),
                (track_seconds - offset).max(0.0),
            ),
        }
    }
}

impl Library {
    pub fn create_flow(&self, name: &str) -> Result<Flow> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.conn.execute(
            "INSERT INTO flows (name, created_at) VALUES (?1, ?2)",
            params![name, now],
        )?;
        Ok(Flow {
            id: self.conn.last_insert_rowid(),
            name: name.to_string(),
            clip_count: 0,
        })
    }

    /// All flows, oldest first.
    pub fn flows(&self) -> Result<Vec<Flow>> {
        let mut statement = self.conn.prepare(
            "SELECT f.id, f.name, COUNT(c.id)
             FROM flows f LEFT JOIN flow_clips c ON c.flow_id = f.id
             GROUP BY f.id ORDER BY f.created_at, f.id",
        )?;
        let flows = statement.query_map([], |row| {
            Ok(Flow {
                id: row.get(0)?,
                name: row.get(1)?,
                clip_count: row.get::<_, i64>(2)? as usize,
            })
        })?;
        Ok(flows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn rename_flow(&self, id: i64, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE flows SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(())
    }

    /// Deletes a flow. Its tracks stay in the library.
    pub fn delete_flow(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM flows WHERE id = ?1", [id])?;
        Ok(())
    }

    /// A flow's clips, in the order they start.
    pub fn flow_clips(&self, flow: i64) -> Result<Vec<FlowClip>> {
        let mut statement = self.conn.prepare(
            "SELECT id, track_id, lane, start, offset, length, speed, key_shift FROM flow_clips
             WHERE flow_id = ?1 ORDER BY start, lane, id",
        )?;
        let rows = statement
            .query_map([flow], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)? as usize,
                    row.get::<_, f64>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, f64>(6)?,
                    row.get::<_, i32>(7)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut clips = Vec::with_capacity(rows.len());
        for (id, track_id, lane, start, offset, length, speed, key_shift) in rows {
            let Some(track) = self.track(track_id)? else {
                continue;
            };
            clips.push(FlowClip {
                id,
                lane,
                start,
                offset,
                length,
                speed,
                key_shift,
                grid: self.beat_grid(track_id)?,
                cues: self.cues(track_id)?,
                track,
            });
        }
        Ok(clips)
    }

    /// One track by id.
    pub fn track(&self, id: i64) -> Result<Option<Track>> {
        let query = TrackQuery {
            id: Some(id),
            ..Default::default()
        };
        Ok(self.tracks(&query)?.into_iter().next())
    }

    /// Places the whole of a track on a lane, starting at `start` seconds.
    /// Returns the new clip's id.
    pub fn add_clip(&self, flow: i64, track: i64, lane: usize, start: f64) -> Result<i64> {
        let track = self
            .track(track)?
            .ok_or_else(|| Error::Import("that track is no longer in the library".into()))?;
        let placement = ClipPlacement {
            lane,
            start,
            offset: 0.0,
            length: track.duration_seconds,
            speed: 1.0,
        }
        .fitted(track.duration_seconds);
        self.conn.execute(
            "INSERT INTO flow_clips (flow_id, track_id, lane, start, offset, length)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                flow,
                track.id,
                placement.lane as i64,
                placement.start,
                placement.offset,
                placement.length
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Moves or trims a clip. The placement is kept within the track's
    /// length and the timeline's lanes.
    pub fn place_clip(&self, clip: i64, placement: ClipPlacement) -> Result<()> {
        let track_seconds: Option<f64> = self
            .conn
            .query_row(
                "SELECT t.duration FROM flow_clips c JOIN tracks t ON t.id = c.track_id
                 WHERE c.id = ?1",
                [clip],
                |row| row.get(0),
            )
            .optional()?;
        let Some(track_seconds) = track_seconds else {
            return Ok(());
        };
        let placement = placement.fitted(track_seconds);
        self.conn.execute(
            "UPDATE flow_clips SET lane = ?2, start = ?3, offset = ?4, length = ?5, speed = ?6
             WHERE id = ?1",
            params![
                clip,
                placement.lane as i64,
                placement.start,
                placement.offset,
                placement.length,
                placement.speed
            ],
        )?;
        Ok(())
    }

    /// Shifts a clip's key by a number of semitones from the track's own.
    pub fn set_clip_key_shift(&self, clip: i64, semitones: i32) -> Result<()> {
        self.conn.execute(
            "UPDATE flow_clips SET key_shift = ?2 WHERE id = ?1",
            params![clip, semitones.clamp(-MAX_KEY_SHIFT, MAX_KEY_SHIFT)],
        )?;
        Ok(())
    }

    pub fn remove_clip(&self, clip: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM flow_clips WHERE id = ?1", [clip])?;
        Ok(())
    }
}
