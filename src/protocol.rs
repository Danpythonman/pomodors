//! Wire protocol: newline-delimited JSON over TCP.

use serde::{Deserialize, Serialize};

pub const DEFAULT_PORT: u16 = 7878;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Work,
    ShortBreak,
    LongBreak,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Work => "FOCUS",
            Phase::ShortBreak => "SHORT BREAK",
            Phase::LongBreak => "LONG BREAK",
        }
    }
}

/// Messages sent from a client to the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello { name: String },
    Toggle,
    Reset,
    Skip,
}

/// The full shared timer state, broadcast to every client whenever it changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub phase: Phase,
    /// Milliseconds left in the current phase at the moment the snapshot was taken.
    pub remaining_ms: u64,
    pub running: bool,
    /// 1-based pomodoro number within the current cycle.
    pub round: u32,
    pub rounds: u32,
    pub users: Vec<String>,
    pub last_action: Option<String>,
}
