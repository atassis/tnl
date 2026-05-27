//! Inspector: drains `LogLine`s emitted by the forwarder, renders them on stdout.

use std::time::SystemTime;

use serde::Serialize;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub timestamp: SystemTime,
    pub method: String,
    pub path: String,
    pub status: Option<u16>,
    pub duration_ms: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub remote_ip: Option<String>,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub enum Verbosity {
    Quiet,
    Default,
    Verbose,
    VeryVerbose,
}

#[derive(Clone, Copy, Debug)]
pub enum Format {
    Text,
    Json,
}

#[derive(Debug)]
pub struct Inspector {
    rx: mpsc::Receiver<LogLine>,
    verbosity: Verbosity,
    format: Format,
    use_color: bool,
}

impl Inspector {
    pub fn new(rx: mpsc::Receiver<LogLine>, verbosity: Verbosity, format: Format) -> Self {
        let use_color = std::io::IsTerminal::is_terminal(&std::io::stdout())
            && std::env::var_os("NO_COLOR").is_none();
        Self {
            rx,
            verbosity,
            format,
            use_color,
        }
    }

    pub async fn run(mut self) {
        while let Some(line) = self.rx.recv().await {
            if matches!(self.verbosity, Verbosity::Quiet) {
                continue;
            }
            match self.format {
                Format::Json => {
                    if let Ok(s) = serde_json::to_string(&line) {
                        println!("{s}");
                    }
                }
                Format::Text => self.print_text(&line),
            }
        }
    }

    fn print_text(&self, l: &LogLine) {
        let t = chrono_like_hms(l.timestamp);
        let status_str = l.status.map_or_else(|| "?".into(), |s| s.to_string());
        let status_col = if self.use_color {
            colorise_status(l.status)
        } else {
            status_str
        };
        println!(
            "{}  {:<6} {:<24} {}  {:>6}ms  {:>8}  {:>8}",
            t,
            l.method,
            truncate(&l.path, 24),
            status_col,
            l.duration_ms,
            fmt_bytes(l.bytes_in),
            fmt_bytes(l.bytes_out)
        );
    }
}

#[allow(clippy::many_single_char_names)]
fn chrono_like_hms(ts: SystemTime) -> String {
    let dur = ts
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs() % 86_400;
    let ms = dur.subsec_millis();
    let (h, rem) = (secs / 3600, secs % 3600);
    let (m, s) = (rem / 60, rem % 60);
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n - 1])
    }
}

fn fmt_bytes(n: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    if n < 1024 {
        format!("{n}B")
    } else if n < 1024 * 1024 {
        format!("{:.1}KB", n as f64 / 1024.0)
    } else {
        format!("{:.1}MB", n as f64 / (1024.0 * 1024.0))
    }
}

fn colorise_status(s: Option<u16>) -> String {
    use nu_ansi_term::Color::{Blue, Green, Red, Yellow};
    let raw = s.map_or_else(|| "?".into(), |s| s.to_string());
    match s.map(|s| s / 100) {
        Some(2) => Green.paint(raw).to_string(),
        Some(3) => Blue.paint(raw).to_string(),
        Some(4) => Yellow.paint(raw).to_string(),
        Some(5) => Red.paint(raw).to_string(),
        _ => raw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_formatting() {
        assert_eq!(fmt_bytes(500), "500B");
        assert_eq!(fmt_bytes(2048), "2.0KB");
        assert_eq!(fmt_bytes(5 * 1024 * 1024), "5.0MB");
    }

    #[test]
    fn truncate_keeps_short_strings() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abcdefghij", 5), "abcd…");
    }

    #[test]
    fn chrono_like_hms_formats_known_timestamp() {
        // 2024-01-01 00:01:30 UTC = 1_704_067_290
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_704_067_290);
        let s = chrono_like_hms(t);
        assert_eq!(s, "00:01:30.000");
    }
}
