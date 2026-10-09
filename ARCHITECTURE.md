# Architecture

pomodors is a single binary with a server that owns one timer, and clients that
display it. They talk over plain TCP using newline-delimited JSON. There is no HTTP.

## Modules

```mermaid
flowchart TB
    main["main.rs<br/>CLI parsing (clap), host / serve / join"]
    server["server.rs<br/>Timer state machine, TCP accept loop, broadcast"]
    client["client.rs<br/>Full-screen TUI (crossterm), key handling"]
    protocol["protocol.rs<br/>ClientMsg, Snapshot, Phase"]
    font["font.rs<br/>3×5 block digit font"]

    main --> server
    main --> client
    server --> protocol
    client --> protocol
    client --> font
```

`host` runs both halves in one process: it spawns `server::run` as a tokio task
and then runs `client::run` against it over loopback. It uses the same code path
as a remote `join`.

## Server

```mermaid
flowchart LR
    subgraph server process
        T["Arc&lt;Mutex&lt;Timer&gt;&gt;"]
        W[["watch::Sender&lt;Snapshot&gt;"]]
        tick["expiry ticker<br/>(every 100 ms)"]
        acc["accept loop"]
        h1["connection task 1"]
        h2["connection task 2"]
    end

    tick -->|"tick()"| T
    h1 -->|"apply(msg)"| T
    h2 -->|"apply(msg)"| T
    T -->|"publish()"| W
    W -->|subscribe| h1
    W -->|subscribe| h2
    acc -->|spawn| h1
    acc -->|spawn| h2
    h1 <-->|TCP| c1((client))
    h2 <-->|TCP| c2((client))
```

- **`Timer`** holds the phase, round, connected users and the last action. Time is
  stored as `remaining` plus an optional `started_at: Instant`. Pausing folds the
  elapsed time into `remaining`, so the server never has to count down itself.
- **`publish()`** takes a `Snapshot` of the timer and writes it to a
  `tokio::sync::watch` channel. A watch channel only keeps the latest value, so a
  slow client skips stale states instead of building up a backlog.
- **Connection tasks** each `select!` on two things: lines from their socket
  (commands to apply) and changes on the watch channel (snapshots to send).
- **The expiry ticker** checks every 100 ms whether a running phase has reached
  zero, advances it, and publishes.

The mutex is a `std::sync::Mutex` and is never held across an `.await`.

## Protocol

Every message is one JSON object followed by `\n`.

| Direction | Message | Example |
| --- | --- | --- |
| client → server | `hello` | `{"type":"hello","name":"alice"}` |
| client → server | `toggle` | `{"type":"toggle"}` |
| client → server | `reset` | `{"type":"reset"}` |
| client → server | `skip` | `{"type":"skip"}` |
| server → client | snapshot | `{"phase":"work","remaining_ms":1499000,"running":true,"round":1,"rounds":4,"users":["alice","bob"],"last_action":"alice started"}` |

The server only sends a snapshot when something changes. Clients count down
locally between updates using `remaining_ms` and the time they received the snapshot.

```mermaid
sequenceDiagram
    participant A as alice (client)
    participant S as server
    participant B as bob (client)

    A->>S: hello {name: alice}
    S-->>A: snapshot (users: [alice])
    B->>S: hello {name: bob}
    S-->>A: snapshot (users: [alice, bob])
    S-->>B: snapshot (users: [alice, bob])

    A->>S: toggle
    S-->>A: snapshot (running, "alice started")
    S-->>B: snapshot (running, "alice started")

    Note over A,B: both count down locally, no traffic

    S->>S: ticker sees remaining = 0
    S-->>A: snapshot (short_break, "focus finished")
    S-->>B: snapshot (short_break, "focus finished")
    Note over A,B: clients beep on phase change

    A-xS: disconnect
    S-->>B: snapshot (users: [bob], "alice left")
```

## Timer phases

```mermaid
stateDiagram-v2
    [*] --> Work
    Work --> ShortBreak: done / skip<br/>(round < rounds)
    Work --> LongBreak: done / skip<br/>(round = rounds)
    ShortBreak --> Work: done / skip<br/>(round += 1)
    LongBreak --> Work: done / skip<br/>(round = 1)
```

Independently of the phase, the timer is either running or paused:

- **toggle** switches between them.
- **reset** restores the current phase's full duration and pauses.
- **skip** advances the phase and keeps the current running state.
- **Natural expiry** advances the phase and pauses, unless `--auto-start` is set.

## Client

```mermaid
flowchart TB
    subgraph "client::run select! loop"
        net["server lines → update state<br/>(beep if phase changed)"]
        keys["crossterm EventStream → send ClientMsg<br/>or toggle digits / quit"]
        timer["100 ms interval"]
    end
    net --> draw
    keys --> draw
    timer --> draw
    draw["Screen::draw()<br/>build lines → compare to last frame → redraw if changed"]
```

- `TerminalGuard` enables raw mode, enters the alternate screen and hides the
  cursor. It undoes all of this on drop, and a panic hook does the same so a crash
  doesn't leave the terminal broken.
- `Screen::build` turns the state into a list of styled lines. `draw` only writes
  to the terminal when that list (or the terminal size) has changed. That keeps
  output to about one redraw per second and avoids flicker.
- Big digits come from `font::render`, which draws each pixel of a 3×5 bitmap as
  `██`. If the result is wider than the terminal, it falls back to plain text.
