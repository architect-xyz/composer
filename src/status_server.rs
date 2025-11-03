use crate::{compose::ComposeContext, status};
use anyhow::{Context, Result};
use axum::{http::StatusCode, response::Response, routing::get, Router};
use log::{error, info};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

pub async fn run_status_server(
    context: Arc<ComposeContext>,
    port: u16,
    cancellation_token: CancellationToken,
) -> Result<()> {
    let app = Router::new().route("/status.txt", get(handle_status)).with_state(context);

    let addr = format!("0.0.0.0:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("failed to bind to {addr}"))?;

    info!("status server listening on {addr}");

    let server_handle = tokio::spawn(async move { axum::serve(listener, app).await });

    // Wait for cancellation or server error
    tokio::select! {
        result = server_handle => {
            result??;
        }
        _ = cancellation_token.cancelled() => {
            info!("status server cancelled, shutting down");
            // Server will be dropped when handle is dropped
        }
    }

    Ok(())
}

async fn handle_status(
    axum::extract::State(context): axum::extract::State<Arc<ComposeContext>>,
) -> axum::response::Result<Response<String>> {
    match status::gather_status_data(&context).await {
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
