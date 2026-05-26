use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use dashmap::DashMap;
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::auth::TokenStore;
use crate::config::Config;
use crate::registry::Registry;

pub type SessionHandle = Arc<Mutex<Box<dyn tnl_protocol::transport::Session>>>;

#[derive(Clone)]
pub struct AppState {
    pub tokens: Arc<TokenStore>,
    pub registry: Arc<Registry>,
    pub session_handles: Arc<DashMap<String, SessionHandle>>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("tokens", &self.tokens)
            .field("registry", &self.registry)
            .field("session_handles_len", &self.session_handles.len())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct AuthedToken {
    pub name: String,
}

#[derive(Debug)]
pub struct ServerHandle {
    pub local_addr: SocketAddr,
    pub join: JoinHandle<()>,
}

pub async fn spawn_server(cfg: Config) -> anyhow::Result<ServerHandle> {
    let tokens = Arc::new(TokenStore::load(std::path::Path::new(&cfg.tokens_file))?);
    let registry = Arc::new(Registry::new(cfg.hostname_root.clone()));
    let state = AppState {
        tokens: tokens.clone(),
        registry: registry.clone(),
        session_handles: Arc::new(DashMap::new()),
    };

    let router = build_router(state);

    let listener = TcpListener::bind(&cfg.listen).await?;
    let local_addr = listener.local_addr()?;
    let join = tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });
    Ok(ServerHandle { local_addr, join })
}

fn build_router(state: AppState) -> Router {
    let authed = Router::new()
        .route("/whoami", get(whoami))
        .layer(middleware::from_fn_with_state(state.clone(), bearer_auth));

    Router::new()
        .route("/healthz", get(healthz))
        .merge(authed)
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn whoami(axum::Extension(token): axum::Extension<AuthedToken>) -> Json<serde_json::Value> {
    Json(json!({ "token_name": token.name }))
}

async fn bearer_auth(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut req: axum::extract::Request,
    next: Next,
) -> impl IntoResponse {
    let Some(bearer) = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
    else {
        return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
    };

    if let Some(name) = state.tokens.verify(bearer) {
        req.extensions_mut().insert(AuthedToken {
            name: name.to_string(),
        });
        next.run(req).await
    } else {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        (StatusCode::UNAUTHORIZED, "invalid token").into_response()
    }
}
