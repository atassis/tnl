//! Shared rendering of failure-attribution error bodies.
//!
//! Used by both the CLI forwarder (synthesising a 502 on local-side failure)
//! and the daemon data plane (emitting 4xx/5xx with `X-Tnl-Component`).

use std::fmt::Write as _;

use ulid::Ulid;

/// Origin of a failure that the response attributes blame to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Component {
    /// No tunnel registered for the requested host.
    Registry,
    /// Tunnel exists in registry but no live session right now.
    Daemon,
    /// Daemon-side substream / write / read failure (yamux or transport).
    Transport,
    /// A response arrived through the tunnel that tnld could not relay as
    /// HTTP/1.1 (unparseable, incomplete, or non-HTTP). Origin is the local
    /// backend, relayed verbatim by the client — the daemon cannot pin blame
    /// on the client binary, so this is a neutral "upstream" attribution.
    Upstream,
    /// Client-side failure (synth) — local backend down, EOF, malformed.
    Client,
}

impl Component {
    /// Lower-case identifier emitted in the `X-Tnl-Component` header.
    pub const fn as_header(self) -> &'static str {
        match self {
            Self::Registry => "registry",
            Self::Daemon => "daemon",
            Self::Transport => "transport",
            Self::Upstream => "upstream",
            Self::Client => "client",
        }
    }
}

/// Negotiated content type for the body. Chosen by the caller from the
/// request's `Accept` header (see [`parse_accept`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accept {
    Html,
    Json,
    Text,
}

impl Accept {
    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Html => "text/html; charset=utf-8",
            Self::Json => "application/json",
            Self::Text => "text/plain; charset=utf-8",
        }
    }
}

/// Parse a request's `Accept` header (or `User-Agent` fall-back) into the
/// negotiated body format.
///
/// Selection order:
///  1. `application/json` or `application/*+json` present → `Json`
///  2. `text/html` present → `Html`
///  3. `*/*` and `User-Agent` starts with `Mozilla` → `Html` (browser default)
///  4. otherwise → `Text`
pub fn parse_accept(accept_header: Option<&str>, user_agent_header: Option<&str>) -> Accept {
    let accept = accept_header.unwrap_or("");
    let ua = user_agent_header.unwrap_or("");

    let tokens: Vec<String> = accept
        .split(',')
        .map(|s| {
            s.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .collect();

    if tokens
        .iter()
        .any(|s| s == "application/json" || (s.starts_with("application/") && s.ends_with("+json")))
    {
        return Accept::Json;
    }

    if tokens.iter().any(|s| s == "text/html") {
        return Accept::Html;
    }

    let has_star = accept.contains("*/*") || accept.is_empty();
    if has_star && ua.starts_with("Mozilla") {
        return Accept::Html;
    }

    Accept::Text
}

/// All inputs needed to render an error body. Optional fields are skipped
/// gracefully when the component does not own that piece of context (e.g.
/// daemon 404 has no `target`).
#[derive(Clone, Debug)]
pub struct ErrorContext<'a> {
    pub component: Component,
    pub kind: Option<&'a str>,
    pub tunnel: Option<&'a str>,
    pub target: Option<&'a str>,
    pub reason: &'a str,
    pub hint: &'a str,
    pub req_id: Ulid,
    pub version: &'a str,
}

/// Render the body bytes for the negotiated format.
///
/// Headers (status, `Content-Type`, `Content-Length`, `X-Tnl-Component`,
/// `X-Tnl-Origin-Failure`, etc.) are the caller's responsibility — this
/// function only emits the response body.
#[must_use]
pub fn render(ctx: &ErrorContext<'_>, accept: Accept) -> Vec<u8> {
    match accept {
        Accept::Html => render_html(ctx).into_bytes(),
        Accept::Json => render_json(ctx).into_bytes(),
        Accept::Text => render_text(ctx).into_bytes(),
    }
}

/// Escape special HTML characters to prevent injection.
///
/// Must replace `&` first so the entity sequences produced below are not
/// double-escaped.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

fn render_html(c: &ErrorContext<'_>) -> String {
    let title = match c.component {
        Component::Registry => "tnl: no tunnel for host",
        Component::Daemon => "tnl: tunnel disconnected",
        Component::Transport => "tnl: tunnel transport error",
        Component::Upstream => "tnl: invalid response from local backend",
        Component::Client => "tnl: local backend unreachable",
    };
    let tunnel = html_escape(c.tunnel.unwrap_or("(unknown)"));
    let target = html_escape(c.target.unwrap_or("(none)"));
    let kind = html_escape(c.kind.unwrap_or("(n/a)"));
    let reason = html_escape(c.reason);
    let hint = html_escape(c.hint);
    let mut s = String::with_capacity(1024);
    let _ = write!(
        s,
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<title>{title}</title>\
<style>body{{font:14px/1.4 system-ui,sans-serif;max-width:36em;margin:3em auto;padding:0 1em;color:#222}}\
code{{background:#f4f4f4;padding:.1em .3em;border-radius:.2em}}\
.fix{{border-left:3px solid #c33;padding:.5em 1em;margin:1em 0;background:#fff5f5}}\
.foot{{color:#888;font-size:.85em;margin-top:2em}}</style></head><body>\
<h1>{title}</h1>\
<p>Tunnel <code>{tunnel}</code>; target <code>{target}</code>; kind <code>{kind}</code>.</p>\
<p><b>Reason:</b> {reason}</p>\
<div class=\"fix\">{hint}</div>\
<p class=\"foot\">tnl v{version} \u{00b7} request <code>{req_id}</code> \u{00b7} component <code>{component}</code></p>\
</body></html>",
        title = title,
        tunnel = tunnel,
        target = target,
        kind = kind,
        reason = reason,
        hint = hint,
        version = c.version,
        req_id = c.req_id,
        component = c.component.as_header(),
    );
    s
}

fn render_json(c: &ErrorContext<'_>) -> String {
    let v = serde_json::json!({
        "error": match c.component {
            Component::Client => "local_backend_failure",
            Component::Registry => "no_such_tunnel",
            Component::Daemon => "tunnel_disconnected",
            Component::Transport => "transport_error",
            Component::Upstream => "invalid_upstream_response",
        },
        "kind": c.kind,
        "tunnel": c.tunnel,
        "target": c.target,
        "component": c.component.as_header(),
        "reason": c.reason,
        "hint": c.hint,
        "request_id": c.req_id.to_string(),
        "version": c.version,
    });
    v.to_string()
}

fn render_text(c: &ErrorContext<'_>) -> String {
    let mut s = String::with_capacity(256);
    let _ = writeln!(
        s,
        "tnl: {} ({}: {}).",
        c.target.unwrap_or("(no target)"),
        c.kind.unwrap_or("error"),
        c.reason
    );
    let _ = writeln!(s, "Tunnel:    {}", c.tunnel.unwrap_or("(unknown)"));
    let _ = writeln!(s, "Hint:      {}", c.hint);
    let _ = writeln!(s, "Component: {}", c.component.as_header());
    let _ = writeln!(s, "Request:   {}", c.req_id);
    s
}
