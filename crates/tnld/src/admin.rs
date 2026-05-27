use std::net::SocketAddr;
use std::time::Duration;

use std::collections::HashMap;

use axum::extract::{ConnectInfo, Json, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;
use tnl_protocol::messages::{PairCreateReq, PairCreateResp, PairRedeemReq, PairRedeemResp};

use crate::auth::{hash_plaintext, TokenEntry};
use crate::pair::RedeemErr;
use crate::serve::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/pair", post(pair_create))
        .route("/pair/redeem", post(pair_redeem))
        .route("/pair/list", get(pair_list))
        .route("/tunnels", get(tunnels_list))
        .route(
            "/tunnels/{subdomain}",
            axum::routing::delete(tunnels_delete),
        )
}

fn require_bearer(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, (StatusCode, Json<serde_json::Value>)> {
    let h = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = h.strip_prefix("Bearer ").unwrap_or("");
    state.tokens.verify(token).map_or_else(
        || {
            Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "unauthorized"})),
            ))
        },
        Ok,
    )
}

async fn pair_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PairCreateReq>,
) -> axum::response::Response {
    if let Err(e) = require_bearer(&state, &headers) {
        return e.into_response();
    }
    let ttl_secs = u64::from(req.expires_in_sec.clamp(60, 900));
    let ttl = Duration::from_secs(ttl_secs);
    let (code, expires_at) = match state.pair_registry.create(req.name, ttl) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "registry_full", "detail": e.to_string()})),
            )
                .into_response();
        }
    };
    let invite_url = format!("{}/invite/{}", state.public_url.trim_end_matches('/'), code);
    let expires_at_unix = expires_at
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let resp = PairCreateResp {
        code,
        expires_at_unix,
        invite_url,
    };
    (StatusCode::OK, Json(resp)).into_response()
}

async fn pair_redeem(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<PairRedeemReq>,
) -> axum::response::Response {
    match state.pair_registry.redeem(&req.code, addr.ip()) {
        Ok(name) => {
            let plaintext = format!("tnl_{}", crate::commands::token::random_token_suffix(26));
            let Ok(hash) = hash_plaintext(&plaintext) else {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "internal"})),
                )
                    .into_response();
            };
            if state
                .tokens
                .append(TokenEntry {
                    name: name.clone(),
                    hash,
                })
                .is_err()
            {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": "internal"})),
                )
                    .into_response();
            }
            let resp = PairRedeemResp {
                token: plaintext,
                endpoint: state.public_url.clone(),
                name,
            };
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(RedeemErr::NotFound) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "pair_not_found"})),
        )
            .into_response(),
        Err(RedeemErr::Expired) => {
            (StatusCode::GONE, Json(json!({"error": "pair_expired"}))).into_response()
        }
        Err(RedeemErr::TooManyAttempts) => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": "pair_too_many_attempts"})),
        )
            .into_response(),
        Err(RedeemErr::RateLimited { retry_after_sec }) => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": "rate_limited",
                "retry_after": retry_after_sec
            })),
        )
            .into_response(),
    }
}

async fn pair_list(State(state): State<AppState>, headers: HeaderMap) -> axum::response::Response {
    if let Err(e) = require_bearer(&state, &headers) {
        return e.into_response();
    }
    let items: Vec<serde_json::Value> = state
        .pair_registry
        .list()
        .into_iter()
        .map(|(code, name, expires_at)| {
            let expires_at_unix = expires_at
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            json!({"code": code, "name": name, "expires_at_unix": expires_at_unix})
        })
        .collect();
    (StatusCode::OK, Json(items)).into_response()
}

async fn tunnels_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> axum::response::Response {
    let owner = match require_bearer(&state, &headers) {
        Ok(n) => n,
        Err(e) => return e.into_response(),
    };
    let all = params.get("all").is_some_and(|v| v == "true");
    let infos = state.registry.snapshot_infos();
    let filtered: Vec<_> = if all {
        infos
    } else {
        infos
            .into_iter()
            .filter(|i| i.owner_token == owner)
            .collect()
    };
    (StatusCode::OK, Json(filtered)).into_response()
}

async fn tunnels_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(subdomain): Path<String>,
) -> axum::response::Response {
    let owner = match require_bearer(&state, &headers) {
        Ok(n) => n,
        Err(e) => return e.into_response(),
    };
    match state.registry.close_by_subdomain(&subdomain, &owner) {
        Ok(()) => (StatusCode::NO_CONTENT, Json(json!({}))).into_response(),
        Err("not_found") => {
            (StatusCode::NOT_FOUND, Json(json!({"error": "not_found"}))).into_response()
        }
        Err("not_owner") => {
            (StatusCode::FORBIDDEN, Json(json!({"error": "not_owner"}))).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))).into_response(),
    }
}
