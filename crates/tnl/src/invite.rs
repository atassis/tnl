//! Parse `https://<endpoint>/invite/<code>` URLs.

use anyhow::{Context, Result};
use url::Url;

#[derive(Debug, PartialEq, Eq)]
pub struct Invite {
    pub endpoint: String,
    pub code: String,
}

pub fn parse(invite_url: &str) -> Result<Invite> {
    let u = Url::parse(invite_url).context("parse invite URL")?;
    let scheme = u.scheme();
    if scheme != "http" && scheme != "https" {
        anyhow::bail!("invite URL must be http or https, got {scheme}");
    }
    let mut segs = u.path().trim_start_matches('/').splitn(2, '/');
    let kind = segs.next().unwrap_or("");
    let code = segs.next().unwrap_or("");
    if kind != "invite" || code.is_empty() {
        anyhow::bail!("invite URL path must be /invite/<code>");
    }
    let mut e = u.clone();
    e.set_path("");
    e.set_query(None);
    e.set_fragment(None);
    Ok(Invite {
        endpoint: e.as_str().trim_end_matches('/').to_string(),
        code: code.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_url() {
        let i = parse("https://tnl-api.example.com/invite/AB-12-CD").unwrap();
        assert_eq!(i.endpoint, "https://tnl-api.example.com");
        assert_eq!(i.code, "AB-12-CD");
    }

    #[test]
    fn http_with_port() {
        let i = parse("http://127.0.0.1:7777/invite/AB12CD").unwrap();
        assert_eq!(i.endpoint, "http://127.0.0.1:7777");
        assert_eq!(i.code, "AB12CD");
    }

    #[test]
    fn rejects_non_invite_path() {
        assert!(parse("https://x/healthz").is_err());
    }

    #[test]
    fn rejects_ws_scheme() {
        assert!(parse("ws://x/invite/A").is_err());
    }
}
