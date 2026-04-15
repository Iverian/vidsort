use std::net::SocketAddr;

use camino::Utf8PathBuf;
use clap::Args;
use clap::Parser;
use clap::ValueEnum;
use tracing_error::ErrorLayer;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::report;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Sort downloaded torrent files into organized media directories"
)]
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

    #[command(flatten)]
    pub tracing: TracingConfig,

    /// Log planned hard-links instead of creating them.
    #[arg(long, action)]
    pub dry_run: bool,

    /// IMDB IDs to treat as Other regardless of classification result (comma-separated or repeated).
    #[arg(long, env = "VIDSORT_IMDB_BLACKLIST", value_delimiter = ',')]
    pub imdb_blacklist: Vec<String>,
}

#[derive(Args, Debug)]
pub struct TransmissionConfig {
    #[arg(long, env = "VIDSORT_TRANSMISSION_URL")]
    pub url: url::Url,

    #[arg(long, env = "VIDSORT_TRANSMISSION_USERNAME")]
    pub username: Option<String>,

    #[arg(long, env = "VIDSORT_TRANSMISSION_PASSWORD")]
    pub password: Option<String>,

    /// Maximum number of attempts for each Transmission RPC request (1 = no retry).
    #[arg(long, env = "VIDSORT_TRANSMISSION_RETRY_ATTEMPTS", default_value = "3")]
    pub transmission_retry_attempts: u32,

    /// Delay between retry attempts, e.g. "1s", "500ms".
    #[arg(long, env = "VIDSORT_TRANSMISSION_RETRY_DELAY", default_value = "15s")]
    pub transmission_retry_delay: humantime::Duration,
}

#[derive(Args, Debug)]
pub struct TvdbConfig {
    #[arg(long, env = "VIDSORT_TVDB_API_KEY")]
    pub tvdb_api_key: String,

    /// Maximum number of attempts for each TVDB HTTP request (1 = no retry).
    #[arg(long, env = "VIDSORT_TVDB_RETRY_ATTEMPTS", default_value = "3")]
    pub tvdb_retry_attempts: u32,

    /// Delay between retry attempts, e.g. "1s", "500ms".
    #[arg(long, env = "VIDSORT_TVDB_RETRY_DELAY", default_value = "1m")]
    pub tvdb_retry_delay: humantime::Duration,

    /// Path to the persistent TVDB cache database directory.
    /// When unset the cache is in-memory only and is lost on restart.
    #[arg(long, env = "VIDSORT_TVDB_CACHE_PATH")]
    pub cache_path: Option<Utf8PathBuf>,
}

#[derive(Args, Debug)]
pub struct DirConfig {
    #[arg(long, env = "VIDSORT_MOVIES_DIR")]
    pub movies: Utf8PathBuf,

    #[arg(long, env = "VIDSORT_SHOWS_DIR")]
    pub shows: Utf8PathBuf,

    #[arg(long, env = "VIDSORT_OTHER_DIR")]
    pub other: Utf8PathBuf,
}

#[derive(Args, Debug)]
pub struct HttpConfig {
    #[arg(long, env = "VIDSORT_BIND", default_value = "0.0.0.0:9090")]
    pub bind: SocketAddr,

    /// Grace period for in-flight requests on shutdown, e.g. "30s", "1m"
    #[arg(long, env = "VIDSORT_HTTP_GRACE_PERIOD", default_value = "30s")]
    pub grace_period: humantime::Duration,
}

#[derive(Args, Debug)]
pub struct TracingConfig {
    /// Log filter in tracing-subscriber `EnvFilter` format, e.g. "vidsort=debug,warn"
    #[arg(long, env = "VIDSORT_LOG", default_value = "info")]
    pub log_filter: String,

    /// Log output format
    #[arg(long, env = "VIDSORT_LOG_FORMAT", default_value = "pretty")]
    pub log_format: LogFormat,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum LogFormat {
    /// Human-readable output for local development
    Pretty,
    /// Structured JSON output for log aggregation (e.g. k8s)
    Json,
}

impl TracingConfig {
    pub fn init(&self) {
        let filter = EnvFilter::new(&self.log_filter);

        match self.log_format {
            LogFormat::Pretty => tracing_subscriber::registry()
                .with(fmt::layer().pretty().with_filter(filter))
                .with(ErrorLayer::default())
                .init(),
            LogFormat::Json => tracing_subscriber::registry()
                .with(fmt::layer().json().with_filter(filter))
                .with(ErrorLayer::default())
                .init(),
        }

        report::install_error_hook();
        report::install_panic_hook();
    }
}
