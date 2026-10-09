//! The shared timer server. One timer, many connected clients.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use crate::protocol::{ClientMsg, Phase, Snapshot};

#[derive(Debug, Clone)]
pub struct Config {
    pub work: Duration,
    pub short_break: Duration,
    pub long_break: Duration,
    /// Number of work sessions before a long break.
    pub rounds: u32,
    /// Start the next phase automatically when one finishes.
    pub auto_start: bool,
}

#[derive(Debug)]
struct Timer {
    cfg: Config,
    phase: Phase,
    round: u32,
    /// Time left as of `started_at` (or right now, if paused).
    remaining: Duration,
    started_at: Option<Instant>,
    users: BTreeMap<u64, String>,
    last_action: Option<String>,
}

impl Timer {
    fn new(cfg: Config) -> Self {
        Timer {
            remaining: cfg.work,
            cfg,
            phase: Phase::Work,
            round: 1,
            started_at: None,
            users: BTreeMap::new(),
            last_action: None,
        }
    }

    fn duration_of(&self, phase: Phase) -> Duration {
        match phase {
            Phase::Work => self.cfg.work,
            Phase::ShortBreak => self.cfg.short_break,
            Phase::LongBreak => self.cfg.long_break,
        }
    }

    fn remaining(&self) -> Duration {
        match self.started_at {
            Some(t) => self.remaining.saturating_sub(t.elapsed()),
            None => self.remaining,
        }
    }

    fn running(&self) -> bool {
        self.started_at.is_some()
    }

    fn toggle(&mut self) {
        match self.started_at.take() {
            Some(t) => self.remaining = self.remaining.saturating_sub(t.elapsed()),
            None => self.started_at = Some(Instant::now()),
        }
    }

    /// Restart the current phase from its full duration, paused.
    fn reset(&mut self) {
        self.remaining = self.duration_of(self.phase);
        self.started_at = None;
    }

    /// Move to the next phase in the work/break cycle.
    fn advance(&mut self, keep_running: bool) {
        self.phase = match self.phase {
            Phase::Work if self.round >= self.cfg.rounds => Phase::LongBreak,
            Phase::Work => Phase::ShortBreak,
            Phase::ShortBreak => {
                self.round += 1;
                Phase::Work
            }
            Phase::LongBreak => {
                self.round = 1;
                Phase::Work
            }
        };
        self.remaining = self.duration_of(self.phase);
        self.started_at = keep_running.then(Instant::now);
    }

    /// Advance the phase if the running timer has expired. Returns true if it did.
    fn tick(&mut self) -> bool {
        if !self.running() || !self.remaining().is_zero() {
            return false;
        }
        let finished = self.phase;
        self.advance(self.cfg.auto_start);
        self.last_action = Some(format!("{} finished", finished.label().to_lowercase()));
        true
    }

    fn apply(&mut self, id: u64, msg: ClientMsg) {
        let who = self.users.get(&id).cloned().unwrap_or_else(|| "someone".into());
        let action = match msg {
            ClientMsg::Hello { name } => {
                self.users.insert(id, name.clone());
                format!("{name} joined")
            }
            ClientMsg::Toggle => {
                self.toggle();
                format!("{who} {}", if self.running() { "started" } else { "paused" })
            }
            ClientMsg::Reset => {
                self.reset();
                format!("{who} reset the timer")
            }
            ClientMsg::Skip => {
                let running = self.running();
                self.advance(running);
                format!("{who} skipped to {}", self.phase.label().to_lowercase())
            }
        };
        self.last_action = Some(action);
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            phase: self.phase,
            remaining_ms: self.remaining().as_millis() as u64,
            running: self.running(),
            round: self.round,
            rounds: self.cfg.rounds,
            users: self.users.values().cloned().collect(),
            last_action: self.last_action.clone(),
        }
    }
}

type Shared = Arc<Mutex<Timer>>;

fn publish(timer: &Shared, tx: &watch::Sender<Snapshot>) {
    let snap = timer.lock().unwrap().snapshot();
    tx.send_replace(snap);
}

/// Serve the shared timer on `listener` until the process exits.
pub async fn run(listener: TcpListener, cfg: Config) -> Result<()> {
    let timer: Shared = Arc::new(Mutex::new(Timer::new(cfg)));
    let (tx, _) = watch::channel(timer.lock().unwrap().snapshot());
    let tx = Arc::new(tx);

    // Watch for phase expiry. Clients interpolate the countdown themselves,
    // so we only need to publish when something actually changes.
    {
        let (timer, tx) = (timer.clone(), tx.clone());
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            loop {
                interval.tick().await;
                let changed = timer.lock().unwrap().tick();
                if changed {
                    publish(&timer, &tx);
                }
            }
        });
    }

    let mut next_id = 0u64;
    loop {
        let (stream, _) = listener.accept().await?;
        let id = next_id;
        next_id += 1;
        let (timer, tx) = (timer.clone(), tx.clone());
        tokio::spawn(async move {
            // A misbehaving or disconnected client only affects itself.
            let _ = handle_client(stream, id, &timer, &tx).await;
            let left = timer.lock().unwrap().users.remove(&id);
            if let Some(name) = left {
                timer.lock().unwrap().last_action = Some(format!("{name} left"));
            }
            publish(&timer, &tx);
        });
    }
}

async fn handle_client(
    stream: TcpStream,
    id: u64,
    timer: &Shared,
    tx: &watch::Sender<Snapshot>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut rx = tx.subscribe();
    rx.mark_changed();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                let msg: ClientMsg = serde_json::from_str(&line)?;
                timer.lock().unwrap().apply(id, msg);
                publish(timer, tx);
            }
            changed = rx.changed() => {
                changed?;
                let mut out = serde_json::to_vec(&*rx.borrow_and_update())?;
                out.push(b'\n');
                writer.write_all(&out).await?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            work: Duration::from_secs(25 * 60),
            short_break: Duration::from_secs(5 * 60),
            long_break: Duration::from_secs(15 * 60),
            rounds: 2,
            auto_start: false,
        }
    }

    #[test]
    fn cycles_through_phases() {
        let mut t = Timer::new(cfg());
        let phases: Vec<_> = (0..5)
            .map(|_| {
                t.advance(false);
                (t.phase, t.round)
            })
            .collect();
        assert_eq!(
            phases,
            [
                (Phase::ShortBreak, 1),
                (Phase::Work, 2),
                (Phase::LongBreak, 2),
                (Phase::Work, 1),
                (Phase::ShortBreak, 1),
            ]
        );
    }

    #[test]
    fn toggle_and_reset() {
        let mut t = Timer::new(cfg());
        t.apply(0, ClientMsg::Hello { name: "ann".into() });
        t.apply(0, ClientMsg::Toggle);
        assert!(t.running());
        assert_eq!(t.last_action.as_deref(), Some("ann started"));
        t.apply(0, ClientMsg::Reset);
        assert!(!t.running());
        assert_eq!(t.remaining(), cfg().work);
    }

    #[test]
    fn expiry_advances_phase() {
        let mut t = Timer::new(cfg());
        t.remaining = Duration::ZERO;
        t.toggle();
        assert!(t.tick());
        assert_eq!(t.phase, Phase::ShortBreak);
        assert!(!t.running());
        assert!(!t.tick());
    }
}
