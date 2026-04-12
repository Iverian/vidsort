use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::report::AnyResult;
use crate::report::SpanTraceExt as _;
use crate::transmission;
use crate::types::TorrentId;
use crate::types::TorrentInfo;

/// Fetcher task: drains the listener channel, calls Transmission RPC for each
/// ID, and forwards the resulting `TorrentInfo` to the pipeline worker.
///
/// Sequential by design — avoids hammering the Transmission daemon and ensures
/// the pipeline queue receives torrents in arrival order.
#[tracing::instrument(skip_all)]
pub async fn run(
    mut rx: mpsc::Receiver<TorrentId>,
    tx: mpsc::Sender<TorrentInfo>,
    mut trans: transmission::Client,
    ct: CancellationToken,
) -> AnyResult<()> {
    loop {
        tokio::select! {
            biased;
            () = ct.cancelled() => break,
            item = rx.recv() => match item {
                None => {
                    tracing::debug!("listener channel closed; stopping fetcher");
                    break;
                }
                Some(id) => match trans.fetch(id).await {
                    Ok(info) => {
                        if tx.send(info).await.is_err() {
                            tracing::warn!("pipeline channel closed; stopping fetcher");
                            break;
                        }
                    }
                    Err(e) => {
                        metrics::counter!("vidsort_torrent_fetch_errors_total").increment(1);
                        tracing::error!(
                            torrent_id = id.0,
                            error = ?e,
                            span_trace = %e.span_trace(),
                            "transmission fetch failed"
                        );
                        // Non-fatal: continue to next torrent.
                    }
                },
            }
        }
    }
    Ok(())
}
