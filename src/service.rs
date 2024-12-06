use std::{fmt::Debug, net::SocketAddr};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use tracing::{Instrument, Span};

/// App result
pub type AppResult<T, E = AppError> = Result<T, E>;

/// App error. This is a wrapper around eyre::Report that also includes an HTTP
/// status code. It implements [`IntoResponse`] so that it can be returned as an
/// error type from [`axum::handler::Handler`]s.
#[derive(Debug)]
pub struct AppError {
    code: StatusCode,
    eyre: eyre::Report,
}

impl AppError {
    /// Instantiate a new error with the bad request status code.
    pub fn bad_req<E: std::error::Error + Send + Sync + 'static>(e: E) -> Self {
        Self { code: StatusCode::BAD_REQUEST, eyre: e.into() }
    }

    /// Instantiate a new error with the bad request status code and an error
    /// string.
    pub fn bad_req_str(e: &str) -> Self {
        Self { code: StatusCode::BAD_REQUEST, eyre: eyre::eyre!(e.to_owned()) }
    }

    /// Instantiate a new error with the internal server error status code.
    pub fn server_err<E: std::error::Error + Send + Sync + 'static>(e: E) -> Self {
        Self { code: StatusCode::INTERNAL_SERVER_ERROR, eyre: e.into() }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.code, format!("{}", self.eyre)).into_response()
    }
}

/// Return a 404 Not Found response
pub async fn return_404() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

/// Return a 200 OK response
pub async fn return_200() -> Response {
    (StatusCode::OK, "ok").into_response()
}

/// Serve a builder service on the given socket address.
pub fn serve_builder_with_span(
    socket: impl Into<SocketAddr>,
    span: Span,
) -> tokio::task::JoinHandle<()> {
    let router = Router::new().route("/healthcheck", get(return_200)).fallback(return_404);

    let addr = socket.into();
    tokio::spawn(
        async move {
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
        }
        .instrument(span),
    )
}
