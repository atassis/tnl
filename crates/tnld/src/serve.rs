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
    pub pair_registry: Arc<crate::pair::PairRegistry>,
    pub public_url: String,
    pub session_grace_sec: u32,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("tokens", &self.tokens)
            .field("registry", &self.registry)
            .field("session_handles_len", &self.session_handles.len())
            .field("public_url", &self.public_url)
            .field("session_grace_sec", &self.session_grace_sec)
            .finish_non_exhaustive()
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
    let (handle, _state) = spawn_server_with_state(cfg).await?;
    Ok(handle)
}

/// Like [`spawn_server`] but also returns the [`AppState`] so tests can
/// pre-populate the registry or session handles before sending requests.
pub async fn spawn_server_with_state(cfg: Config) -> anyhow::Result<(ServerHandle, AppState)> {
    let tokens = Arc::new(TokenStore::load(std::path::Path::new(&cfg.tokens_file))?);
    let registry = Arc::new(Registry::new(cfg.hostname_root.clone()));
    let pair_registry = Arc::new(crate::pair::PairRegistry::new());

    // Background cleanup of expired pair codes.
    {
        let pr = pair_registry.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                pr.cleanup();
            }
        });
    }

    // Background GC: sweep tunnels whose grace window has expired.
    {
        let reg = registry.clone();
        let grace = std::time::Duration::from_secs(u64::from(cfg.session_grace_sec));
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                reg.gc_disconnected(grace);
            }
        });
    }

    let state = AppState {
        tokens: tokens.clone(),
        registry: registry.clone(),
        session_handles: Arc::new(DashMap::new()),
        pair_registry,
        public_url: cfg.public_url.clone(),
        session_grace_sec: cfg.session_grace_sec,
    };

    let router = build_router(state.clone());

    let listener = TcpListener::bind(&cfg.listen).await?;
    let local_addr = listener.local_addr()?;
    let join = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .ok();
    });
    Ok((ServerHandle { local_addr, join }, state))
}

fn build_router(state: AppState) -> Router {
    let authed = Router::new()
        .route("/whoami", get(whoami))
        .route("/control", get(crate::control::handler))
        .layer(middleware::from_fn_with_state(state.clone(), bearer_auth));

    Router::new()
        .route("/healthz", get(healthz))
        .merge(authed)
        .merge(crate::admin::router())
        .fallback(crate::data_plane::handler)
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
        req.extensions_mut().insert(AuthedToken { name });
        next.run(req).await
    } else {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        (StatusCode::UNAUTHORIZED, "invalid token").into_response()
    }
}
