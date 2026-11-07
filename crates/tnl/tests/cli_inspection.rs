use std::process::Command;

#[test]
fn inspection_options_are_explicit_and_require_opt_in() {
    let help = Command::new(env!("CARGO_BIN_EXE_tnl"))
        .args(["http", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    for option in ["--inspect", "--inspect-port", "--inspect-bodies"] {
        assert!(help.contains(option), "missing {option}: {help}");
    }
    for args in [
        vec!["http", "3000", "--inspect-bodies", "json"],
        vec!["http", "3000", "--inspect-port", "1234"],
        vec!["http", "3000", "--inspect", "--inspect-bodies", "html"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_tnl"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Tunnel:"));
    }
}

#[test]
fn inspect_bind_failure_is_explicit_before_any_tunnel_banner() {
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let config = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        config.path(),
        "endpoint = \"http://127.0.0.1:1\"\ntoken = \"fixture\"\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tnl"))
        .args([
            "http",
            "3000",
            "--inspect",
            "--inspect-port",
            &occupied.local_addr().unwrap().port().to_string(),
        ])
        .env("TNL_CONFIG", config.path())
        .env_remove("TNL_ENDPOINT")
        .env_remove("TNL_TOKEN")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("bind loopback inspector"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Tunnel:"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("#token="));
}

#[cfg(unix)]
#[test]
fn actual_cli_private_service_stops_on_ctrl_c_without_a_tunnel() {
    use std::io::{BufRead, Read, Write};
    use std::process::Stdio;
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let vacant = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = vacant.local_addr().unwrap();
    drop(vacant);
    let config = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        config.path(),
        format!("endpoint = \"http://{endpoint}\"\ntoken = \"ACCOUNT_CANARY\"\n"),
    )
    .unwrap();
    let mut child = Child(
        Command::new(env!("CARGO_BIN_EXE_tnl"))
            .args(["http", "3000", "--inspect", "--quiet", "--json"])
            .env("TNL_CONFIG", config.path())
            .env_remove("TNL_ENDPOINT")
            .env_remove("TNL_TOKEN")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut stderr = std::io::BufReader::new(child.0.stderr.take().unwrap());
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    let url = url::Url::parse(
        line.split_whitespace()
            .find(|s| s.starts_with("http://"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    assert!(url.query().is_none());
    let token = url.fragment().unwrap().strip_prefix("token=").unwrap();
    assert_ne!(token, "ACCOUNT_CANARY");
    let addr = format!("127.0.0.1:{}", url.port().unwrap());
    let mut socket = std::net::TcpStream::connect(&addr).unwrap();
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    socket.write_all(format!("GET /api/v1/stats HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\n\r\n").as_bytes()).unwrap();
    let mut reply = String::new();
    socket.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"));
    assert!(reply.contains("\"connected\":false"));
    assert!(!reply.contains(token));
    // A failed remote connect proves run's select has polled the Ctrl-C future.
    while !line.contains("reconnecting") {
        line.clear();
        assert_ne!(stderr.read_line(&mut line).unwrap(), 0);
    }
    assert!(Command::new("kill")
        .args(["-INT", &child.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(status.success());
    assert!(std::net::TcpStream::connect(&addr).is_err());
    let mut stdout = String::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    assert!(!stdout.contains(token));
    assert!(!stdout.contains("Tunnel:"));
}
