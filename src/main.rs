mod client;
mod font;
mod protocol;
mod server;

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use tokio::net::TcpListener;

use client::Digits;
use protocol::DEFAULT_PORT;

/// A shared pomodoro timer for the terminal.
///
/// One person hosts (or runs a headless server) and anyone else can join.
/// Everyone sees the same countdown and anyone can start, pause, reset or skip it.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a server and join it from this terminal. The server stops when you quit.
    Host {
        /// Address to listen on. Use 0.0.0.0:7878 to let others on your network join.
        #[arg(short, long, default_value_t = format!("127.0.0.1:{DEFAULT_PORT}"))]
        addr: String,
        #[command(flatten)]
        timer: TimerArgs,
        #[command(flatten)]
        display: DisplayArgs,
    },
    /// Run a headless server that others can join.
    Serve {
        /// Address to listen on. Use 0.0.0.0:7878 to let others on your network join.
        #[arg(short, long, default_value_t = format!("127.0.0.1:{DEFAULT_PORT}"))]
        addr: String,
        #[command(flatten)]
        timer: TimerArgs,
    },
    /// Join a running timer.
    Join {
        /// Server address to connect to.
        #[arg(default_value_t = format!("127.0.0.1:{DEFAULT_PORT}"))]
        addr: String,
        #[command(flatten)]
        display: DisplayArgs,
    },
}

#[derive(Args)]
struct TimerArgs {
    /// Work session length (e.g. 25, 25m, 90s, 1h; bare numbers are minutes).
    #[arg(short, long, default_value = "25m", value_parser = parse_duration)]
    work: Duration,
    /// Short break length.
    #[arg(long, default_value = "5m", value_parser = parse_duration)]
    short_break: Duration,
    /// Long break length.
    #[arg(long, default_value = "15m", value_parser = parse_duration)]
    long_break: Duration,
    /// Work sessions before a long break.
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u32).range(1..))]
    rounds: u32,
    /// Start the next phase automatically instead of pausing between phases.
    #[arg(long)]
    auto_start: bool,
}

#[derive(Args)]
struct DisplayArgs {
    /// Name shown to other users. Defaults to $USER.
    #[arg(short, long)]
    name: Option<String>,
    /// How to draw the countdown.
    #[arg(short, long, value_enum, default_value_t = Digits::Big)]
    digits: Digits,
}

impl TimerArgs {
    fn into_config(self) -> server::Config {
        server::Config {
            work: self.work,
            short_break: self.short_break,
            long_break: self.long_break,
            rounds: self.rounds,
            auto_start: self.auto_start,
        }
    }
}

impl DisplayArgs {
    fn into_options(self, addr: String) -> client::Options {
        let name = self
            .name
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_else(|| "anonymous".into());
        client::Options { addr, name, digits: self.digits }
    }
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, unit) = match s.find(|c: char| !c.is_ascii_digit()) {
        Some(i) => s.split_at(i),
        None => (s, "m"),
    };
    let n: u64 = num.parse().map_err(|_| format!("invalid duration: {s:?}"))?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        _ => return Err(format!("unknown unit {unit:?} (use s, m or h)")),
    };
    if secs == 0 {
        return Err("duration must be greater than zero".into());
    }
    Ok(Duration::from_secs(secs))
}

async fn bind(addr: &str) -> Result<TcpListener> {
    TcpListener::bind(addr).await.with_context(|| format!("could not listen on {addr}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Serve { addr, timer } => {
            let listener = bind(&addr).await?;
            eprintln!("pomodors server listening on {}", listener.local_addr()?);
            server::run(listener, timer.into_config()).await
        }
        Command::Host { addr, timer, display } => {
            let listener = bind(&addr).await?;
            let mut local = listener.local_addr()?;
            tokio::spawn(server::run(listener, timer.into_config()));
            // Can't connect to 0.0.0.0 / [::], so use loopback in that case.
            if local.ip().is_unspecified() {
                local.set_ip(match local {
                    SocketAddr::V4(_) => Ipv4Addr::LOCALHOST.into(),
                    SocketAddr::V6(_) => Ipv6Addr::LOCALHOST.into(),
                });
            }
            client::run(display.into_options(local.to_string())).await
        }
        Command::Join { addr, display } => client::run(display.into_options(addr)).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("25"), Ok(Duration::from_secs(25 * 60)));
        assert_eq!(parse_duration("90s"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_duration("1h"), Ok(Duration::from_secs(3600)));
        assert!(parse_duration("0").is_err());
        assert!(parse_duration("5d").is_err());
        assert!(parse_duration("abc").is_err());
    }
}
