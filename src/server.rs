use axum::Router;
use axum::extract::State;
use axum::routing::get;
use metrics_exporter_prometheus::PrometheusHandle;
use tokio_util::sync::CancellationToken;

use crate::config::HttpConfig;
use crate::report::AnyResult;

#[tracing::instrument(skip_all, fields(bind = %config.bind))]
pub async fn serve(
    config: HttpConfig,
    metrics_handle: PrometheusHandle,
    ct: CancellationToken,
) -> AnyResult<()> {
    let server_handle = axum_server::Handle::new();
    tokio::spawn(shutdown_on_cancel(
        ct,
        server_handle.clone(),
        *config.grace_period,
    ));
    tracing::info!(bind = %config.bind, "starting http server");
    axum_server::bind(config.bind)
        .handle(server_handle)
        .serve(router(metrics_handle).into_make_service())
        .await?;
    Ok(())
}

#[tracing::instrument(skip_all, fields(grace_period = ?grace_period))]
async fn shutdown_on_cancel(
    ct: CancellationToken,
    handle: axum_server::Handle<std::net::SocketAddr>,
    grace_period: std::time::Duration,
) {
    ct.cancelled().await;
    tracing::info!("shutting down http server");
    handle.graceful_shutdown(Some(grace_period));
}

fn router(metrics_handle: PrometheusHandle) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics))
        .with_state(metrics_handle)
}

async fn health() -> &'static str {
    "ok"
}

async fn metrics(State(handle): State<PrometheusHandle>) -> String {
    handle.render()
}
