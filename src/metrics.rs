use metrics_exporter_prometheus::PrometheusBuilder;
use metrics_exporter_prometheus::PrometheusHandle;

use crate::report::AnyResult;

#[tracing::instrument]
pub fn init() -> AnyResult<PrometheusHandle> {
    let handle = PrometheusBuilder::new().install_recorder()?;

    metrics::describe_counter!(
        "vidsort_torrents_processed_total",
        "Total number of torrents successfully processed end-to-end"
    );
    metrics::describe_counter!(
        "vidsort_torrents_classified_total",
        "Total number of torrents classified, labelled by kind (show, movie, other)"
    );
    metrics::describe_counter!(
        "vidsort_torrent_fetch_errors_total",
        "Total number of Transmission RPC fetch failures"
    );
    metrics::describe_counter!(
        "vidsort_tvdb_errors_total",
        "Total number of TVDB lookup failures"
    );
    metrics::describe_counter!(
        "vidsort_link_errors_total",
        "Total number of hard-link syscall failures"
    );
    metrics::describe_counter!(
        "vidsort_links_created_total",
        "Total number of hard links successfully created"
    );
    metrics::describe_histogram!(
        "vidsort_processing_duration_seconds",
        "Per-torrent processing duration from fetch to final link"
    );

    Ok(handle)
}
