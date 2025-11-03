use crate::{compose::ComposeContext, status};
use anyhow::{Context, Result};
use axum::{http::StatusCode, response::Response, routing::get, Router};
use log::{error, info};
use std::sync::Arc;
use tokio::net::TcpListener;

struct StatusServerState {
    context: ComposeContext,
    compose: crate::compose_types::Compose,
}

pub async fn run_status_server(
    context: ComposeContext,
    compose: crate::compose_types::Compose,
    port: u16,
) -> Result<()> {
    let app = Router::new()
        .route("/status.txt", get(handle_status))
        .with_state(Arc::new(StatusServerState { context, compose }));

    let addr = format!("0.0.0.0:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("failed to bind to {addr}"))?;

    info!("status server listening on {addr}");

    axum::serve(listener, app).await.context("status server error")?;

    Ok(())
}

async fn handle_status(
    axum::extract::State(state): axum::extract::State<Arc<StatusServerState>>,
) -> axum::response::Result<Response<String>> {
    match status::gather_status_data(&state.context, &state.compose).await {
        Ok((services_info, status_map)) => {
            let formatted = status::format_status_table(&services_info, &status_map)
                .map_err(|e| {
                    error!("formatting status: {e:?}");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/plain; charset=utf-8")
                .body(formatted)
                .map_err(|e| {
                    error!("building response: {e:?}");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?)
        }
        Err(e) => {
            error!("gathering status: {e:?}");
            Err(StatusCode::INTERNAL_SERVER_ERROR.into())
        }
    }
}
