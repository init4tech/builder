use axum::{
    Router,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use std::net::SocketAddr;

/// Return a 404 Not Found response
pub async fn return_404() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

/// Return a 200 OK response
pub async fn return_200() -> Response {
    (StatusCode::OK, "ok").into_response()
}

/// Serve a builder service on the given socket address.
pub fn serve_builder(socket: impl Into<SocketAddr>) -> tokio::task::JoinHandle<()> {
    let router = Router::new().route("/healthcheck", get(return_200)).fallback(return_404);

    let addr = socket.into();
    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(&addr).await {
            Ok(listener) => {
                if let Err(err) = axum::serve(listener, router).await {
                    tracing::error!(%err, "serve failed");
                }
            }
            Err(err) => {
                tracing::error!(%err, "failed to bind to the address");
            }
        };
    })
}
