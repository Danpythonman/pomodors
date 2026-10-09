//! Full-screen terminal client that connects to a timer server.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Color, Print, SetAttribute, SetForegroundColor};
use crossterm::{cursor, execute, queue, terminal};
use futures::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;

use crate::font;
use crate::protocol::{ClientMsg, Phase, Snapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Digits {
    /// Plain text, e.g. 24:59
    Normal,
    /// Large block digits
    Big,
}

pub struct Options {
    pub addr: String,
    pub name: String,
    pub digits: Digits,
}

/// Puts the terminal into full-screen raw mode and restores it on drop.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide)?;
        Ok(TerminalGuard)
    }

    fn restore() {
        let _ = execute!(io::stdout(), cursor::Show, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        Self::restore();
    }
}

async fn send(writer: &mut OwnedWriteHalf, msg: &ClientMsg) -> Result<()> {
    let mut out = serde_json::to_vec(msg)?;
    out.push(b'\n');
    writer.write_all(&out).await?;
    Ok(())
}

pub async fn run(opts: Options) -> Result<()> {
    let stream = TcpStream::connect(&opts.addr)
        .await
        .with_context(|| format!("could not connect to {}", opts.addr))?;
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    send(&mut writer, &ClientMsg::Hello { name: opts.name }).await?;

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        TerminalGuard::restore();
        default_hook(info);
    }));
    let _guard = TerminalGuard::enter()?;

    let mut events = EventStream::new();
    let mut redraw = tokio::time::interval(Duration::from_millis(100));
    let mut screen = Screen { digits: opts.digits, state: None, last_frame: None };

    loop {
        tokio::select! {
            _ = redraw.tick() => screen.draw(false)?,
            line = lines.next_line() => {
                let Some(line) = line? else { bail!("server closed the connection") };
                let snap: Snapshot = serde_json::from_str(&line)?;
                if screen.state.as_ref().is_some_and(|(old, _)| old.phase != snap.phase) {
                    execute!(io::stdout(), Print('\x07'))?;
                }
                screen.state = Some((snap, Instant::now()));
                screen.draw(false)?;
            }
            event = events.next() => {
                let Some(event) = event else { break };
                match event? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        let msg = match key.code {
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                            KeyCode::Char('q') | KeyCode::Esc => break,
                            KeyCode::Char(' ') | KeyCode::Char('p') => Some(ClientMsg::Toggle),
                            KeyCode::Char('r') => Some(ClientMsg::Reset),
                            KeyCode::Char('s') => Some(ClientMsg::Skip),
                            KeyCode::Char('b') => {
                                screen.digits = match screen.digits {
                                    Digits::Big => Digits::Normal,
                                    Digits::Normal => Digits::Big,
                                };
                                screen.draw(false)?;
                                None
                            }
                            _ => None,
                        };
                        if let Some(msg) = msg {
                            send(&mut writer, &msg).await?;
                        }
                    }
                    Event::Resize(..) => screen.draw(true)?,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    text: String,
    color: Color,
    bold: bool,
}

impl Line {
    fn new(text: impl Into<String>, color: Color) -> Self {
        Line { text: text.into(), color, bold: false }
    }

    fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
}

struct Screen {
    digits: Digits,
    /// Latest server state and when we received it, for local interpolation.
    state: Option<(Snapshot, Instant)>,
    last_frame: Option<(Vec<Line>, (u16, u16))>,
}

impl Screen {
    fn build(&self, width: u16) -> Vec<Line> {
        let Some((snap, received)) = &self.state else {
            return vec![Line::new("Connecting…", Color::Grey)];
        };

        let mut remaining = Duration::from_millis(snap.remaining_ms);
        if snap.running {
            remaining = remaining.saturating_sub(received.elapsed());
        }
        // Round up so a fresh 25 minute timer shows 25:00, not 24:59.
        let secs = remaining.as_millis().div_ceil(1000) as u64;
        let time = format!("{:02}:{:02}", secs / 60, secs % 60);

        let color = match snap.phase {
            Phase::Work => Color::Red,
            Phase::ShortBreak => Color::Green,
            Phase::LongBreak => Color::Cyan,
        };

        let mut lines = vec![Line::new(snap.phase.label(), color).bold(), Line::new("", color)];

        let big = font::render(&time);
        let fits = big[0].chars().count() <= width as usize;
        if self.digits == Digits::Big && fits {
            lines.extend(big.into_iter().map(|row| Line::new(row, color)));
        } else {
            lines.push(Line::new(time, color).bold());
        }

        let status = if snap.running { "▶ running" } else { "⏸ paused" };
        let round = match snap.phase {
            Phase::LongBreak => "cycle complete".to_string(),
            _ => format!("pomodoro {}/{}", snap.round, snap.rounds),
        };
        lines.push(Line::new("", color));
        lines.push(Line::new(format!("{status}  ·  {round}"), Color::White));
        if let Some(action) = &snap.last_action {
            lines.push(Line::new(action.clone(), Color::DarkGrey));
        }
        lines.push(Line::new("", color));
        lines.push(Line::new(
            format!("{} connected: {}", snap.users.len(), snap.users.join(", ")),
            Color::Grey,
        ));
        lines
    }

    /// Redraw the screen if anything visible changed (or `force` is set).
    fn draw(&mut self, force: bool) -> Result<()> {
        let size = terminal::size()?;
        let lines = self.build(size.0);
        if !force && self.last_frame.as_ref() == Some(&(lines.clone(), size)) {
            return Ok(());
        }

        let (width, height) = size;
        let mut out = io::stdout();
        queue!(out, terminal::Clear(terminal::ClearType::All))?;

        let top = height.saturating_sub(lines.len() as u16) / 2;
        for (i, line) in lines.iter().enumerate() {
            put_centered(&mut out, top + i as u16, width, line)?;
        }

        let help = Line::new(
            "space start/pause · r reset · s skip · b big digits · q quit",
            Color::DarkGrey,
        );
        put_centered(&mut out, height.saturating_sub(1), width, &help)?;
        out.flush()?;

        self.last_frame = Some((lines, size));
        Ok(())
    }
}

fn put_centered(out: &mut impl Write, row: u16, width: u16, line: &Line) -> Result<()> {
    let text: String = line.text.chars().take(width as usize).collect();
    let col = width.saturating_sub(text.chars().count() as u16) / 2;
    queue!(out, cursor::MoveTo(col, row), SetForegroundColor(line.color))?;
    if line.bold {
        queue!(out, SetAttribute(Attribute::Bold))?;
    }
    queue!(out, Print(text), SetAttribute(Attribute::Reset))?;
    Ok(())
}
