# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## General guidelines

Behavioral guidelines to reduce common LLM coding mistakes. Merge with project-specific instructions as needed.

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks, use judgment.

### 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

### 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

### 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

### 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:
- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:
```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work") require constant clarification.

## Commands

```sh
cargo build              # build
cargo run                # run
cargo test               # run all tests
cargo test <test_name>   # run a single test
cargo clippy             # lint — must pass with zero warnings (dead_code excepted)
cargo +nightly fmt       # format (nightly required for rustfmt.toml options)
```

After every non-trivial change:
1. Run `cargo +nightly fmt` to format all modified files.
2. Run `cargo clippy` and fix all warnings before considering the work done.

Always run `cargo fmt` **after** all edits for a task are complete — not between intermediate steps. The project uses `[lints.clippy] pedantic = "warn"`. Suppress `dead_code` warnings on unused items that will be wired up in later implementation steps — all other warnings must be resolved in code, not suppressed.

## Architecture

`vidsort` is a Rust CLI daemon that watches for completed torrents and hard-links video files into organised media directories.

### Workflow

Three concurrent stages connected by `tokio::sync::mpsc` channels:

```
[listener task]                [fetcher task]               [pipeline tasks]
FIFO line                      recv TorrentId               recv TorrentInfo
  -> parse TorrentId    ──ch1──>  -> transmission::fetch  ──spawn──> classify
  -> send to ch1                  -> send TorrentInfo to              -> tvdb enrich
                                    spawned task                      -> linker
```

- **listener**: reads lines from the FIFO, parses `TorrentId`, sends into `ch1: mpsc::Sender<TorrentId>`. Never blocks — FIFO opened with `O_NONBLOCK`.
- **fetcher**: single task draining `ch1`; calls Transmission RPC (sequential — avoids hammering the daemon); for each result `tokio::spawn`s an independent pipeline task.
- **pipeline tasks**: run concurrently; each owns its `TorrentInfo` and a clone of `Arc<tvdb::Client>` and `Arc<DirConfig>`. No channel needed — the spawned future drives itself to completion.

The `axum` HTTP server is a fourth concurrent task on the same `current_thread` executor.

### Module Structure

```
src/
  main.rs          — CLI parse, config load, tracing init, spawn HTTP + run FIFO loop
  config.rs        — Config struct; load from TOML file + env vars + CLI (last wins)
  listener.rs      — async FIFO reader; open with O_NONBLOCK via nix, wrap in BufReader
  transmission.rs  — thin wrapper over transmission-rpc crate (fetch torrent files by ID)
  classifier.rs    — pure heuristic: TorrentInfo -> Classification (fully unit-testable)
  tvdb.rs          — TVDB v4 REST client: JWT auth, search + extended series/movie detail
  linker.rs        — path construction + nix::unistd::linkat; create_dir_all before linking
  pipeline.rs      — orchestrates one torrent end-to-end; takes &mut AppContext
  metrics.rs       — register counters/gauges; PrometheusHandle passed to server
  server.rs        — axum router: GET /health, GET /metrics
  parser/
    mod.rs
    episode.rs     — existing: parse season + episode number from filename
    movie.rs       — NEW: parse raw title + year hint from filename
```

### Key Types

```rust
pub struct TorrentId(pub i64);

pub struct TorrentFile { pub name: Utf8PathBuf, pub length: u64 }

pub struct TorrentInfo {
    pub id: TorrentId,
    pub name: String,            // top-level torrent name
    pub download_dir: Utf8PathBuf,
    pub files: Vec<TorrentFile>,
}

pub enum Classification {
    Show(ShowCandidate),
    Movie(MovieCandidate),
    Other,
}

pub struct ShowCandidate {
    pub raw_title: String,
    pub season: u32,
    pub episode: u32,
    pub video_files: Vec<TorrentFile>,   // excludes samples/extras
}

pub struct MovieCandidate {
    pub raw_title: String,
    pub year_hint: Option<u32>,
    pub video_files: Vec<TorrentFile>,
}

// Post-TVDB enrichment
pub struct ShowMeta  { canonical_title, release_year, imdb_id, season, episode }
pub struct MovieMeta { canonical_title, release_year, imdb_id }

// Shared across spawned pipeline tasks
pub struct PipelineContext {
    pub tvdb: Arc<tvdb::Client>,   // reqwest::Client is Send+Sync; token cache uses RwLock internally
    pub dirs: Arc<DirConfig>,
}

// Held only by the fetcher task — no sharing needed
pub struct FetcherContext {
    pub trans: transmission::Client,
    pub pipeline: Arc<PipelineContext>,
}
```

### Config

All configuration is via CLI args or env vars — no config file. `config.rs` holds the `clap` derive structs; `Config::parse()` in `main.rs` is sufficient.

```rust
#[derive(Parser)]
pub struct Config {
    #[arg(long, env = "VIDSORT_FIFO_PATH")]
    pub fifo_path: Utf8PathBuf,

    #[command(flatten)]
    pub transmission: TransmissionConfig,

    #[command(flatten)]
    pub tvdb: TvdbConfig,

    #[command(flatten)]
    pub dirs: DirConfig,

    #[command(flatten)]
    pub http: HttpConfig,
}

#[derive(Args)]
pub struct TransmissionConfig {
    #[arg(long, env = "VIDSORT_TRANSMISSION_URL")]
    pub url: Url,
    #[arg(long, env = "VIDSORT_TRANSMISSION_USERNAME")]
    pub username: Option<String>,
    #[arg(long, env = "VIDSORT_TRANSMISSION_PASSWORD")]
    pub password: Option<String>,
}

#[derive(Args)]
pub struct TvdbConfig {
    #[arg(long, env = "VIDSORT_TVDB_API_KEY")]
    pub api_key: String,
}

#[derive(Args)]
pub struct DirConfig {
    #[arg(long, env = "VIDSORT_MOVIES_DIR")]
    pub movies: Utf8PathBuf,
    #[arg(long, env = "VIDSORT_SHOWS_DIR")]
    pub shows: Utf8PathBuf,
    #[arg(long, env = "VIDSORT_OTHER_DIR")]
    pub other: Utf8PathBuf,
}

#[derive(Args)]
pub struct HttpConfig {
    #[arg(long, env = "VIDSORT_BIND", default_value = "0.0.0.0:9090")]
    pub bind: SocketAddr,
}
```

### Classification Heuristic (`classifier.rs`)

1. Filter to video extensions (`mkv mp4 avi mov wmv m4v`). Zero → `Other`.
2. Exclude samples (< 50 MB or name contains `sample`) and extras (`featurette`, `trailer`, `behind-the-scenes`, `deleted-scene`, `interview`).
3. **Show**: run `parser::episode` against the largest video file's stem, then the torrent name, then parent directory components. First success → `Show`.
4. **Movie**: strip quality tags (`BluRay`, `WEB-DL`, `1080p`, `x264`, `HEVC`, etc.) via static regex; extract 4-digit year hint; remaining text is `raw_title`. Non-empty → `Movie`.
5. Otherwise → `Other`.

Multi-file torrents (season packs): classify on the first parseable file; `video_files` holds all. Re-parse `EpisodeId` per file during linking for individual `SxxExx` numbers.

### TVDB Client (`tvdb.rs`)

TVDB v4 API. JWT obtained via `POST /v4/login`; cached in-memory, refreshed after 25 days.

Endpoints: `GET /v4/search?query={}&type=series|movie` then `GET /v4/series/{id}/extended` or `GET /v4/movies/{id}/extended` for IMDB ID + release year.

Disambiguation: take first result; if `year_hint` is present, filter by `first_aired` year first. On zero matches → log `WARN` and fall back to `Other`-style linking with raw title.

### Linker (`linker.rs`)

```
Movies: $MOVIE_BASE / "Title (Year) [ttXXX]" / "Title.ext"
Shows:  $SHOWS_BASE / "Title (Year) [ttXXX]" / "Season 01" / "Title S01E02.ext"
Other:  $OTHER_BASE / filename (verbatim)
```

Source path: `info.download_dir.join(&file.name)`. Uses `nix::unistd::linkat` with `AtFlags::empty()`. `EEXIST` → `WARN` + skip (non-fatal). Other `linkat` errors → `ERROR` + continue to next file.

### Error Handling

Use `AnyResult<T>` (defined and re-exported from `src/report.rs`) throughout — never use `eyre::Result` directly — no top-level custom error enum. Create typed errors with `thiserror` only when a specific variant needs to be caught and handled explicitly. All pipeline errors are logged via `tracing` and counted in Prometheus; the FIFO loop continues to the next torrent on any failure.

The custom `EyreHandler` in `src/report.rs` calls `SpanTrace::capture()` at error creation, then renders the full error chain followed by the span trace in `debug` output. Install order in `main`: `config.tracing.init()` first (so `ErrorLayer` is active), then `report::install_hook()`.

### HTTP Server (`server.rs` + `metrics.rs`)

`GET /health` → `"ok"`. `GET /metrics` → Prometheus text format via `metrics-exporter-prometheus`.

Key metrics: `vidsort_torrents_processed_total`, `vidsort_torrents_classified_total{kind}`, `vidsort_torrent_fetch_errors_total`, `vidsort_tvdb_errors_total`, `vidsort_link_errors_total`, `vidsort_links_created_total`, `vidsort_processing_duration_seconds`.

### Error Propagation

Use `?` for all error propagation. `eyre::Report` converts from any type that implements `std::error::Error`, so explicit `.map_err` is only needed when `?` genuinely cannot infer the conversion (e.g. converting between two custom error types, or attaching context with `.wrap_err()`).

```rust
// correct
let handle = PrometheusBuilder::new().install_recorder()?;

// wrong — map_err is redundant when the error implements std::error::Error
let handle = PrometheusBuilder::new().install_recorder().map_err(|e| eyre::eyre!(e))?;
```

### Task Spawning

All long-running tasks are spawned through `Launcher` (defined in `main.rs`), never via bare `tokio::spawn` at the top level. `Launcher` uses an `mpsc` channel: each spawned task sends its `AnyResult<()>` on completion. `wait(ct)` drives the event loop — it selects on `ct.cancelled()` (triggering graceful drain) and on incoming task results (logging errors immediately). This gives a single place to observe task failures and coordinate shutdown.

```rust
Launcher::new()
    .spawn(server::serve(config.http, metrics_handle, ct.clone()))
    .spawn(listener::run(config.fifo_path, tx, ct.clone()))
    .wait(ct)
    .await;
```

Tasks that internally manage sub-tasks (e.g. `server::serve` spawning `shutdown_on_cancel`) still use `tokio::spawn` directly — the `Launcher` convention applies only to the top-level application tasks in `run()`.

### Naming Conventions

- `CancellationToken` parameters are always named `ct`.

### Code Style

Always derive `Debug` on every type. Also derive `Clone` and `Copy` whenever all fields support it — `Copy` for small value types (IDs, coordinates, flags), `Clone` for types that own heap data (strings, vecs). Omit `Clone`/`Copy` only when a type intentionally models unique ownership (e.g. a file handle or cancellation token).

Prefer named functions over closures for any block of code with distinct, nameable functionality. Use closures only for short, inline transformations (e.g. `map`, `filter`, iterator adapters).

```rust
// correct — intent is clear, testable in isolation
std::panic::set_hook(Box::new(panic_hook));

// wrong — logic is buried and anonymous
std::panic::set_hook(Box::new(|info| { /* 15 lines */ }));
```

### Structured Logging

Use `tracing` fields for all dynamic values — event messages must be static strings.

```rust
// correct
tracing::error!(torrent_id = %id, error = ?err, span_trace = %err.span_trace(), "transmission fetch failed");

// wrong — interpolation bakes values into the message, breaking log aggregation
tracing::error!("transmission fetch failed for {id}: {err}");
```

- Instrument all significant functions with `#[tracing::instrument]`. Use `skip_all` and add only the fields that are meaningful for diagnostics. Trivial helpers and short pure functions do not need instrumentation.
- Use `%` (Display) for user-facing values, `?` (Debug) only when no Display impl exists.
- Field names: `snake_case`. Prefer domain names (`torrent_id`, `show_title`) over generic ones (`id`, `value`).
- Errors: always use the field name `error` with Debug format (`error = ?err`), never embedded in the message.
- Span traces: always use the field name `span_trace` (`span_trace = %err.span_trace()`).
- Panics are captured by the hook in `main.rs` via `tracing::error!(panic = %info)`.

### Key Dependencies

- `tokio` (`current_thread`) — async runtime
- `axum` + `axum-server` — HTTP server for health + metrics
- `reqwest` — HTTP client for Transmission RPC + TVDB
- `clap` — CLI argument parsing
- `nix` — `linkat`, `mkfifo`, `O_NONBLOCK` FIFO open
- `camino` — UTF-8 typed paths throughout
- `metrics` + `metrics-exporter-prometheus` — observability
- `tracing` + `tracing-subscriber` — structured logging (`RUST_LOG` / `EnvFilter`)
- `eyre` — error propagation; custom `EyreHandler` in `src/report.rs` captures a `SpanTrace` at error creation and renders it in the debug output
