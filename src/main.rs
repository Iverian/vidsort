#![warn(clippy::pedantic)]

use std::future::Future;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use vidsort::config::Config;
use vidsort::fetcher;
use vidsort::listener;
use vidsort::metrics;
use vidsort::pipeline;
use vidsort::pipeline::PipelineContext;
use vidsort::report::AnyResult;
use vidsort::report::SpanTraceExt as _;
use vidsort::server;
use vidsort::transmission;
use vidsort::tvdb;
use vidsort::types::TorrentInfo;

fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    let config = Config::parse();
    config.tracing.init();

    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
        .block_on(run(config));

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = ?error, span_trace = %error.span_trace(), "application failed");
            ExitCode::FAILURE
        }
    }
}

#[tracing::instrument(skip_all)]
async fn run(config: Config) -> AnyResult<()> {
    let ct = CancellationToken::new();
    let metrics_handle = metrics::init()?;

    // ch1: listener -> fetcher (torrent IDs)
    let (id_tx, id_rx) = mpsc::channel::<vidsort::types::TorrentId>(32);
    // ch2: fetcher -> pipeline worker (fetched torrent metadata)
    //
    // Both channels are bounded so back-pressure propagates naturally: if the
    // pipeline worker is busy, the fetcher pauses; if the fetcher pauses, the
    // listener's sends will eventually block, dropping the Transmission
    // callback line rather than unboundedly queuing work.
    let (info_tx, info_rx) = mpsc::channel::<TorrentInfo>(32);

    let pipeline_ctx = Arc::new(PipelineContext {
        tvdb: Arc::new(tvdb::Client::new(&config.tvdb)?),
        dirs: Arc::new(config.dirs),
        dry_run: config.dry_run,
        imdb_blacklist: config.imdb_blacklist,
    });

    let trans = transmission::Client::new(&config.transmission);

    Launcher::new()
        .spawn(server::serve(config.http, metrics_handle, ct.clone()))
        .spawn(listener::run(config.fifo_path, id_tx, ct.clone()))
        .spawn(fetcher::run(id_rx, info_tx, trans, ct.clone()))
        .spawn(pipeline::run_worker(info_rx, pipeline_ctx, ct.clone()))
        .wait(ct)
        .await;
    Ok(())
}

struct Launcher {
    tx: mpsc::Sender<AnyResult<()>>,
    rx: mpsc::Receiver<AnyResult<()>>,
}

impl Launcher {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel(8);
        Self { tx, rx }
    }

    fn spawn<F>(self, f: F) -> Self
    where
        F: Future<Output = AnyResult<()>> + Send + 'static,
    {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            tx.send(f.await).await.ok();
        });
        self
    }

    async fn wait(self, ct: CancellationToken) {
        let Self { tx, mut rx } = self;
        drop(tx);
        tokio::spawn(shutdown_on_ctrl_c(ct.clone()));

        loop {
            tokio::select! {
                biased;
                () = ct.cancelled() => {
                    Self::complete(rx).await;
                    break;
                }
                r = rx.recv() => {
                    match r {
                        Some(Ok(())) => {
                        }
                        Some(Err(e)) => {
                            tracing::error!(error = ?e, span_trace = %e.span_trace(), "task failed");
                            Self::complete(rx).await;
                            break;
                        }
                        None => {
                            break;
                        }
                    }
                }
            }
        }
    }

    async fn complete(mut rx: mpsc::Receiver<AnyResult<()>>) {
        while let Some(r) = rx.recv().await {
            if let Err(e) = r {
                tracing::error!(error = ?e, span_trace = %e.span_trace(), "task failed");
            }
        }
    }
}

#[tracing::instrument(skip_all)]
async fn shutdown_on_ctrl_c(ct: CancellationToken) {
    tokio::signal::ctrl_c().await.ok();
    tracing::info!("received ctrl_c signal");
    ct.cancel();
}
