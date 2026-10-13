//! djcraft control protocol, version 1.
//!
//! Every user action is an [`Action`] sent to the session host, which checks it
//! against the sender's [`Role`] and applies it to the engine. The host then
//! broadcasts the resulting [`SessionState`]. The desktop UI and remote clients
//! use the same messages.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

/// Zero-based deck index.
pub type DeckId = u8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Owns the session and renders the audio.
    Host,
    /// Can control decks, but not the session or the master output.
    CoDj,
    /// Receives state only.
    Viewer,
}

impl Role {
    pub fn allows(self, action: &Action) -> bool {
        match (self, action) {
            (Role::Host, _) => true,
            (Role::CoDj, Action::Deck { .. }) => true,
            (Role::CoDj, Action::Mixer { .. }) => false,
            (Role::Viewer, _) => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum Action {
    Deck { deck: DeckId, action: DeckAction },
    Mixer { action: MixerAction },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DeckAction {
    /// Load a track from a path on the host.
    Load { path: String },
    Play,
    Pause,
    Seek { seconds: f64 },
    /// Playback speed as a ratio of the original tempo; 1.0 is unchanged.
    SetTempo { ratio: f64 },
    /// Keep the original pitch when the tempo changes.
    SetKeyLock { on: bool },
    SetVolume { level: f32 },
    /// Drive playback from an external speed signal (jog wheel, timecode).
    /// `Some(speed)` overrides the transport, with 1.0 as normal forward
    /// speed and negative values as reverse. `None` hands control back.
    Scratch { speed: Option<f64> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MixerAction {
    SetMasterGain { level: f32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClientMessage {
    pub version: u32,
    /// Chosen by the client; echoed back if the action is rejected.
    pub seq: u64,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    State { state: SessionState },
    Rejected { seq: u64, reason: Rejection },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    NotPermitted,
    UnknownDeck,
    InvalidValue,
    /// The engine could not take the action right now; retry.
    Busy,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub decks: Vec<DeckState>,
    pub master_gain: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeckState {
    pub track: Option<TrackInfo>,
    /// A load has started but nothing is playable yet.
    pub loading: bool,
    /// Set when the last load failed.
    pub error: Option<String>,
    pub playing: bool,
    pub position_seconds: f64,
    pub tempo: f64,
    pub key_lock: bool,
    pub volume: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackInfo {
    pub title: String,
    /// An estimate until the track is fully loaded.
    pub duration_seconds: f64,
    /// How much of the track has been decoded and can be played.
    pub loaded_seconds: f64,
    pub fully_loaded: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn play() -> Action {
        Action::Deck {
            deck: 0,
            action: DeckAction::Play,
        }
    }

    fn master() -> Action {
        Action::Mixer {
            action: MixerAction::SetMasterGain { level: 0.5 },
        }
    }

    #[test]
    fn roles_gate_actions() {
        assert!(Role::Host.allows(&play()));
        assert!(Role::Host.allows(&master()));
        assert!(Role::CoDj.allows(&play()));
        assert!(!Role::CoDj.allows(&master()));
        assert!(!Role::Viewer.allows(&play()));
        assert!(!Role::Viewer.allows(&master()));
    }

    #[test]
    fn client_message_wire_format() {
        let message = ClientMessage {
            version: PROTOCOL_VERSION,
            seq: 7,
            action: Action::Deck {
                deck: 1,
                action: DeckAction::Scratch { speed: Some(-0.5) },
            },
        };
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(
            json,
            r#"{"version":1,"seq":7,"action":{"target":"deck","deck":1,"action":{"op":"scratch","speed":-0.5}}}"#
        );
        assert_eq!(serde_json::from_str::<ClientMessage>(&json).unwrap(), message);
    }

    #[test]
    fn server_message_round_trips() {
        let message = ServerMessage::State {
            state: SessionState {
                decks: vec![DeckState {
                    track: Some(TrackInfo {
                        title: "a".into(),
                        duration_seconds: 1.5,
                        loaded_seconds: 1.5,
                        fully_loaded: true,
                    }),
                    tempo: 1.0,
                    ..Default::default()
                }],
                master_gain: 1.0,
            },
        };
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(serde_json::from_str::<ServerMessage>(&json).unwrap(), message);
    }
}
