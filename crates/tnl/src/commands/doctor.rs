//! `tnl doctor` — self-diagnostic. Walks a series of checks and renders a
//! PASS/WARN/FAIL table (or JSON via --json) with actionable hints.

use std::time::{Duration, SystemTime};

use serde::Serialize;

use crate::commands::config::resolve_config_path;
use crate::config::Config;

#[derive(Serialize, PartialEq, Eq, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Warn,
    Fail,
}

#[derive(Serialize, Debug)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
    pub hint: Option<String>,
}

impl Check {
    fn pass(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: Status::Pass,
            detail: detail.into(),
            hint: None,
        }
    }

    fn fail(name: &str, detail: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: Status::Fail,
            detail: detail.into(),
            hint: Some(hint.into()),
        }
    }

    fn warn(name: &str, detail: impl Into<String>, hint: Option<String>) -> Self {
        Self {
            name: name.into(),
            status: Status::Warn,
            detail: detail.into(),
            hint,
        }
    }

    fn render(&self) {
        let badge = match self.status {
            Status::Pass => "[PASS]",
            Status::Warn => "[WARN]",
            Status::Fail => "[FAIL]",
        };
        println!("{}  {}: {}", badge, self.name, self.detail);
        if let Some(h) = &self.hint {
            println!("        hint: {h}");
        }
    }
}

#[allow(clippy::too_many_lines)]
pub async fn run(json: bool) -> anyhow::Result<()> {
    let mut report = Vec::<Check>::new();

    // 1. Config present.
    let cfg_path = match resolve_config_path() {
        Ok(p) => p,
        Err(e) => {
            report.push(Check::fail(
                "config_path",
                format!("could not resolve config path: {e}"),
                "ensure HOME is set, or pass TNL_CONFIG=<path>",
            ));
            return finalize(&report, json);
        }
    };

    let cfg = match Config::load_from(&cfg_path) {
        Ok(c) => {
            report.push(Check::pass(
                "config_load",
                format!("loaded from {}", cfg_path.display()),
            ));
            c
        }
        Err(e) => {
            report.push(Check::fail(
                "config_load",
                format!("{e}"),
                format!(
                    "run `tnl init` or set TNL_CONFIG=<path> (looked at {})",
                    cfg_path.display()
                ),
            ));
            return finalize(&report, json);
        }
    };

    // 2. /healthz reachable.
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            report.push(Check::fail(
                "http_client",
                format!("could not build HTTP client: {e}"),
                "this is a tnl bug — please file an issue",
            ));
            return finalize(&report, json);
        }
    };

    let healthz_url = format!("{}/healthz", cfg.endpoint.trim_end_matches('/'));
    let healthz_resp = client.get(&healthz_url).send().await;
    match healthz_resp {
        Ok(r) if r.status().is_success() => {
            // Date-header skew check (best-effort; warn-only).
            let date_hdr = r.headers().get("date").cloned();
            report.push(Check::pass(
                "healthz",
                format!("{} → {}", healthz_url, r.status()),
            ));
            if let Some(date) = date_hdr {
                if let Ok(remote) = parse_http_date(date.to_str().unwrap_or("")) {
                    let skew = SystemTime::now()
                        .duration_since(remote)
                        .unwrap_or_else(|e| e.duration());
                    if skew > Duration::from_secs(300) {
                        report.push(Check::fail(
                            "clock_skew",
                            format!("local clock is off by {} seconds vs server", skew.as_secs()),
                            "synchronise your clock (systemd-timesyncd, chrony, ntpd)",
                        ));
                    } else if skew > Duration::from_secs(30) {
                        report.push(Check::warn(
                            "clock_skew",
                            format!("clock skew {} seconds", skew.as_secs()),
                            Some(
                                "synchronise your clock if pairing or tokens start failing".into(),
                            ),
                        ));
                    } else {
                        report.push(Check::pass(
                            "clock_skew",
                            format!("skew {} seconds (within tolerance)", skew.as_secs()),
                        ));
                    }
                }
            }
        }
        Ok(r) => {
            report.push(Check::fail(
                "healthz",
                format!("{} → {}", healthz_url, r.status()),
                "is the daemon up? check `docker ps` or `tnld serve` logs",
            ));
        }
        Err(e) => {
            report.push(Check::fail(
                "healthz",
                format!("could not reach {healthz_url}: {e}"),
                "check the endpoint URL and that the daemon is running",
            ));
        }
    }

    // 3. /whoami with our token.
    let whoami_url = format!("{}/whoami", cfg.endpoint.trim_end_matches('/'));
    match client.get(&whoami_url).bearer_auth(&cfg.token).send().await {
        Ok(r) if r.status().is_success() => {
            report.push(Check::pass(
                "whoami",
                format!("{whoami_url} → 200 (token accepted)"),
            ));
        }
        Ok(r) if r.status().as_u16() == 401 => {
            report.push(Check::fail(
                "whoami",
                "token rejected by server (401)",
                "run `tnl init` again with a fresh invite URL, or `tnl auth login`",
            ));
        }
        Ok(r) => {
            report.push(Check::warn(
                "whoami",
                format!("{} → {}", whoami_url, r.status()),
                Some("unexpected status; check daemon logs".into()),
            ));
        }
        Err(e) => {
            report.push(Check::fail(
                "whoami",
                format!("could not reach {whoami_url}: {e}"),
                "check the endpoint URL and that the daemon is running",
            ));
        }
    }

    finalize(&report, json)
}

fn finalize(report: &[Check], json: bool) -> anyhow::Result<()> {
    let any_failed = report.iter().any(|c| c.status == Status::Fail);
    if json {
        println!("{}", serde_json::to_string(report)?);
    } else {
        for c in report {
            c.render();
        }
    }
    if any_failed {
        std::process::exit(1);
    }
    Ok(())
}

/// Parse RFC 7231 HTTP-date (`Sun, 06 Nov 1994 08:49:37 GMT`). Returns the
/// instant as `SystemTime`. Best-effort: only supports the IMF-fixdate format.
fn parse_http_date(s: &str) -> Result<SystemTime, &'static str> {
    // Format: "Day, DD Mon YYYY HH:MM:SS GMT"
    let s = s.trim();
    if !s.ends_with("GMT") {
        return Err("not IMF-fixdate");
    }
    let parts: Vec<&str> = s.split_ascii_whitespace().collect();
    if parts.len() != 6 {
        return Err("wrong number of fields");
    }
    let day: u32 = parts[1].parse().map_err(|_| "bad day")?;
    let month = match parts[2] {
        "Jan" => 1u32,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return Err("bad month"),
    };
    let year: u32 = parts[3].parse().map_err(|_| "bad year")?;
    let hms: Vec<&str> = parts[4].split(':').collect();
    if hms.len() != 3 {
        return Err("bad HMS");
    }
    let h: u32 = hms[0].parse().map_err(|_| "bad hour")?;
    let m: u32 = hms[1].parse().map_err(|_| "bad minute")?;
    let s_: u32 = hms[2].parse().map_err(|_| "bad second")?;
    let unix_secs = days_since_epoch(year, month, day) * 86_400
        + u64::from(h) * 3600
        + u64::from(m) * 60
        + u64::from(s_);
    Ok(SystemTime::UNIX_EPOCH + Duration::from_secs(unix_secs))
}

fn days_since_epoch(year: u32, month: u32, day: u32) -> u64 {
    const NORMAL: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    const LEAP: [u32; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    // Simple Gregorian calendar arithmetic; good for 1970..2100.
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    let months = if is_leap(year) { &LEAP } else { &NORMAL };
    for m in months.iter().take(month as usize - 1) {
        days += u64::from(*m);
    }
    days + u64::from(day - 1)
}

const fn is_leap(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_http_date() {
        // Sun, 06 Nov 1994 08:49:37 GMT = 784111777 seconds since epoch
        let t = parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
        let secs = t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(secs, 784_111_777);
    }

    #[test]
    fn rejects_non_imf_fixdate() {
        assert!(parse_http_date("2024-01-01").is_err());
        assert!(parse_http_date("Sun, 06 Nov 1994 08:49:37 UTC").is_err());
    }
}
