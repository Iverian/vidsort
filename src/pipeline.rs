use std::sync::Arc;
use std::time::Instant;

use camino::Utf8Path;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::classifier::ClassificationKind;
use crate::classifier::ShowFile;
use crate::classifier::{self};
use crate::config::DirConfig;
use crate::linker;
use crate::report::AnyResult;
use crate::report::SpanTraceExt as _;
use crate::tvdb;
use crate::tvdb::MovieMeta;
use crate::tvdb::ShowMeta;
use crate::types::TorrentFile;
use crate::types::TorrentInfo;

// ── shared context ────────────────────────────────────────────────────────────

/// Shared state for the pipeline worker.  Cheaply cloneable via `Arc`.
#[derive(Debug)]
pub struct PipelineContext {
    pub tvdb: Arc<tvdb::Client>,
    pub dirs: Arc<DirConfig>,
    pub dry_run: bool,
    pub imdb_blacklist: Vec<String>,
}

// ── pipeline worker ───────────────────────────────────────────────────────────

/// Pipeline worker task: drains the fetcher channel and runs the full
/// classify → enrich → link pipeline for each torrent **sequentially**.
///
/// Sequential processing avoids races when two torrents for the same show
/// complete at nearly the same time (e.g. simultaneous season pack + single
/// episode): directory creation and hard-linking are always serialised.
#[tracing::instrument(skip_all)]
pub async fn run_worker(
    mut rx: mpsc::Receiver<TorrentInfo>,
    ctx: Arc<PipelineContext>,
    ct: CancellationToken,
) -> AnyResult<()> {
    loop {
        tokio::select! {
            biased;
            () = ct.cancelled() => break,
            item = rx.recv() => match item {
                None => {
                    tracing::debug!("fetcher channel closed; stopping pipeline worker");
                    break;
                }
                Some(info) => run(info, &ctx).await,
            }
        }
    }
    ctx.tvdb.flush().await;
    Ok(())
}

// ── per-torrent pipeline ──────────────────────────────────────────────────────

/// Run the full pipeline for a single completed torrent.
///
/// All errors are logged and counted in Prometheus — this function never
/// panics and always returns.  Callers may fire-and-forget via `tokio::spawn`.
#[tracing::instrument(skip(ctx), fields(torrent_id = info.id.0, torrent_name = %info.name))]
pub async fn run(info: TorrentInfo, ctx: &PipelineContext) {
    let start = Instant::now();
    metrics::counter!("vidsort_torrents_processed_total").increment(1);

    let download_dir = info.download_dir.clone();
    let all_files_backup = info.files.clone();

    let classification = match classifier::classify(info, &ctx.tvdb, &ctx.imdb_blacklist).await {
        Ok(c) => c,
        Err(e) => {
            metrics::counter!("vidsort_tvdb_errors_total").increment(1);
            tracing::error!(
                error = ?e,
                span_trace = %e.span_trace(),
                "classification failed; falling back to other directory"
            );
            if ctx.dry_run {
                log_other_links(&all_files_backup, &download_dir, &ctx.dirs);
            } else {
                linker::link_other(&all_files_backup, &download_dir, &ctx.dirs);
            }
            metrics::histogram!("vidsort_processing_duration_seconds")
                .record(start.elapsed().as_secs_f64());
            return;
        }
    };

    let classifier::Classification { all_files, kind } = classification;
    metrics::counter!(
        "vidsort_torrents_classified_total",
        "kind" => kind_label(&kind)
    )
    .increment(1);

    match kind {
        ClassificationKind::Show(result) => {
            tracing::info!(
                title = %result.meta.canonical_title,
                season = result.meta.season,
                episode = result.meta.episode,
                "classified as show"
            );
            if ctx.dry_run {
                log_show_links(&result.meta, &result.video_files, &download_dir, &ctx.dirs);
            } else {
                linker::link_show(&result.meta, &result.video_files, &download_dir, &ctx.dirs);
            }
        }
        ClassificationKind::Movie(result) => {
            tracing::info!(title = %result.meta.canonical_title, "classified as movie");
            if ctx.dry_run {
                log_movie_links(&result.meta, &result.video_files, &download_dir, &ctx.dirs);
            } else {
                linker::link_movie(&result.meta, &result.video_files, &download_dir, &ctx.dirs);
            }
        }
        ClassificationKind::Other => {
            tracing::info!(
                file_count = all_files.len(),
                "torrent classified as Other; linking all files verbatim"
            );
            if ctx.dry_run {
                log_other_links(&all_files, &download_dir, &ctx.dirs);
            } else {
                linker::link_other(&all_files, &download_dir, &ctx.dirs);
            }
        }
    }

    metrics::histogram!("vidsort_processing_duration_seconds")
        .record(start.elapsed().as_secs_f64());
}

fn log_show_links(meta: &ShowMeta, files: &[ShowFile], download_dir: &Utf8Path, dirs: &DirConfig) {
    for sf in files {
        let src = download_dir.join(&sf.file.name);
        let dst = dirs.shows.join(linker::show_dest(meta, sf));
        tracing::info!(src = %src, dst = %dst, "dry-run: would link");
    }
}

fn log_movie_links(
    meta: &MovieMeta,
    files: &[TorrentFile],
    download_dir: &Utf8Path,
    dirs: &DirConfig,
) {
    for file in files {
        let src = download_dir.join(&file.name);
        let dst = dirs.movies.join(linker::movie_dest(meta, file));
        tracing::info!(src = %src, dst = %dst, "dry-run: would link");
    }
}

fn log_other_links(files: &[TorrentFile], download_dir: &Utf8Path, dirs: &DirConfig) {
    for file in files {
        let src = download_dir.join(&file.name);
        let dst = dirs.other.join(&file.name);
        tracing::info!(src = %src, dst = %dst, "dry-run: would link");
    }
}

fn kind_label(kind: &ClassificationKind) -> &'static str {
    match kind {
        ClassificationKind::Show(_) => "show",
        ClassificationKind::Movie(_) => "movie",
        ClassificationKind::Other => "other",
    }
}
