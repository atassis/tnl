//! Local-only executable fixtures. Never discover binaries or inherit operator config.
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const DEADLINE: Duration = Duration::from_secs(10);

pub fn binary(name: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(name).expect("explicit test binary path required"));
    assert!(
        path.is_absolute() && path.is_file(),
        "test binary must be an existing absolute path"
    );
    path
}

pub fn command(binary: &Path, home: &Path) -> Command {
    let mut cmd = Command::new(binary);
    cmd.env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("NO_COLOR", "1")
        .env("RUST_LOG", "warn")
        .stdin(Stdio::null());
    cmd
}

pub struct Process {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}
impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}
impl Process {
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
    pub fn start(mut cmd: Command, home: &Path, name: &str) -> Self {
        let (out, stdout) = tempfile::Builder::new()
            .prefix(name)
            .suffix(".stdout")
            .tempfile_in(home)
            .unwrap()
            .keep()
            .unwrap();
        let (err, stderr) = tempfile::Builder::new()
            .prefix(name)
            .suffix(".stderr")
            .tempfile_in(home)
            .unwrap()
            .keep()
            .unwrap();
        let child = cmd
            .stdout(out)
            .stderr(err)
            .spawn()
            .expect("start explicit fixture binary");
        Self {
            child,
            stdout,
            stderr,
        }
    }
    pub fn stdout(&self) -> String {
        std::fs::read_to_string(&self.stdout).unwrap()
    }
    pub fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap()
    }
    pub fn alive(&mut self) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "fixture child exited early (private diagnostics not printed)"
        );
    }
    pub async fn exited(&mut self) -> std::process::ExitStatus {
        tokio::time::timeout(DEADLINE, async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    break status;
                }
                poll().await;
            }
        })
        .await
        .expect("bounded child exit")
    }
    pub async fn interrupt(&mut self) {
        let home = self.stdout.parent().unwrap();
        let mut cmd = command(Path::new("/bin/kill"), home);
        cmd.args(["-INT", &self.child.id().to_string()]);
        let mut signal = Self::start(cmd, home, &format!("signal-{}", self.child.id()));
        assert!(signal.exited().await.success());
        assert!(
            self.exited().await.success(),
            "Ctrl-C must exit successfully"
        );
    }
    pub async fn inspector(&mut self) -> Inspector {
        tokio::time::timeout(DEADLINE, async {
            loop {
                self.alive();
                let stderr = self.stderr();
                if let Some(url) = stderr
                    .lines()
                    .find_map(|line| line.strip_prefix("Inspector (private; do not share): "))
                {
                    let (origin, token) = url
                        .split_once("/#token=")
                        .expect("private bootstrap URL shape");
                    let addr: SocketAddr = origin.strip_prefix("http://").unwrap().parse().unwrap();
                    assert!(addr.ip().is_loopback());
                    assert_eq!(token.len(), 64);
                    return Inspector {
                        origin: origin.into(),
                        token: token.into(),
                        addr,
                    };
                }
                poll().await;
            }
        })
        .await
        .expect("bounded inspector bootstrap")
    }
    pub async fn log(&mut self, id: &str) -> Value {
        tokio::time::timeout(DEADLINE, async {
            loop {
                self.alive();
                for line in self.stdout().lines().filter(|line| line.starts_with('{')) {
                    let record: Value =
                        serde_json::from_str(line).expect("CLI JSON log must be valid JSON");
                    if record["req_id"] == id {
                        return record;
                    }
                }
                poll().await;
            }
        })
        .await
        .expect("bounded correlated CLI log")
    }
}
fn private_file(path: &Path) -> std::fs::File {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap()
}
fn write_private(path: &Path, data: &[u8]) {
    use std::io::Write;
    private_file(path).write_all(data).unwrap();
}
pub async fn poll() {
    tokio::time::sleep(Duration::from_millis(25)).await;
}
pub fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap()
}

pub struct Fixture {
    pub home: tempfile::TempDir,
    pub daemon: Process,
    pub caddy: Process,
    pub addr: SocketAddr,
    daemon_bin: PathBuf,
    client_bin: PathBuf,
    daemon_config: PathBuf,
    client_config: PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.daemon.stop();
        self.caddy.stop();
    }
}
impl Fixture {
    pub async fn start() -> Self {
        let client_bin = binary("TNL_TEST_CLIENT_BIN");
        let daemon_bin = binary("TNL_TEST_DAEMON_BIN");
        let caddy_bin = binary("TNL_TEST_CADDY_BIN");
        let home = tempfile::tempdir().unwrap();
        let tokens = home.path().join("tokens.toml");
        let mut cmd = command(&daemon_bin, home.path());
        cmd.args(["token", "add", "fixture", "--tokens-file"])
            .arg(&tokens);
        let mut mint = Process::start(cmd, home.path(), "mint");
        assert!(
            mint.exited().await.success(),
            "mint synthetic fixture token"
        );
        let token = mint.stdout();
        assert!(token.trim().starts_with("tnl_"));
        let reserved_upstream = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let reserved_caddy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream = reserved_upstream.local_addr().unwrap();
        let addr = reserved_caddy.local_addr().unwrap();
        let daemon_config = home.path().join("daemon.toml");
        write_private(&daemon_config, format!("listen = \"{upstream}\"\npublic_url = \"http://{addr}\"\nhostname_root = \"t.test\"\ntokens_file = {:?}\nsession_grace_sec = 0\ntrusted_proxies = [\"127.0.0.1/32\"]\n", tokens.to_str().unwrap()).as_bytes());
        let client_config = home.path().join("client.toml");
        write_private(
            &client_config,
            toml::to_string(&tnl::config::Config {
                endpoint: format!("http://{addr}"),
                token: token.trim().into(),
            })
            .unwrap()
            .as_bytes(),
        );
        let mut cmd = command(&daemon_bin, home.path());
        cmd.args(["serve", "--config"]).arg(&daemon_config);
        drop(reserved_upstream);
        let daemon = Process::start(cmd, home.path(), "daemon");
        let caddyfile = home.path().join("Caddyfile");
        write_private(&caddyfile, caddy_config(addr, upstream).as_bytes());
        let mut cmd = command(&caddy_bin, home.path());
        cmd.args(["run", "--adapter", "caddyfile", "--config"])
            .arg(caddyfile);
        drop(reserved_caddy);
        let caddy = Process::start(cmd, home.path(), "caddy");
        let mut fixture = Self {
            home,
            daemon,
            caddy,
            addr,
            daemon_bin,
            client_bin,
            daemon_config,
            client_config,
        };
        fixture.ready().await;
        fixture
    }
    async fn ready(&mut self) {
        tokio::time::timeout(DEADLINE, async {
            loop {
                self.daemon.alive();
                self.caddy.alive();
                if let Ok(response) = http()
                    .get(format!("http://{}/healthz", self.addr))
                    .send()
                    .await
                {
                    if response.status() == 200 {
                        break;
                    }
                }
                poll().await;
            }
        })
        .await
        .expect("bounded daemon/Caddy health readiness");
    }
    pub fn client_command(&self) -> Command {
        let mut cmd = command(&self.client_bin, self.home.path());
        cmd.env("TNL_CONFIG", &self.client_config);
        cmd
    }
    pub fn client(&self, backend: SocketAddr, name: &str, options: &[&str]) -> Process {
        let mut cmd = self.client_command();
        cmd.args(["http", &backend.to_string(), name, "--json"])
            .args(options);
        Process::start(cmd, self.home.path(), name)
    }
    pub async fn registered(&mut self, child: &mut Process, name: &str) {
        // Authenticated status is readiness; probes do not pollute backend counters/history.
        tokio::time::timeout(DEADLINE, async {
            loop {
                child.alive();
                self.daemon.alive();
                self.caddy.alive();
                let mut cmd = self.client_command();
                cmd.args(["status", "--json"]);
                let mut status = Process::start(cmd, self.home.path(), &format!("status-{name}"));
                if status.exited().await.success() {
                    let value: Value = serde_json::from_str(&status.stdout()).expect("status JSON");
                    if value
                        .as_array()
                        .expect("status array")
                        .iter()
                        .any(|row| row["subdomain"] == name && row["active"] == true)
                    {
                        break;
                    }
                }
                poll().await;
            }
        })
        .await
        .expect("bounded real CLI registration");
    }
    pub async fn restart_daemon(&mut self) {
        self.daemon.child.kill().unwrap();
        self.daemon.child.wait().unwrap();
        let mut cmd = command(&self.daemon_bin, self.home.path());
        cmd.args(["serve", "--config"]).arg(&self.daemon_config);
        self.daemon = Process::start(cmd, self.home.path(), "daemon-restarted");
        self.ready().await;
    }
    pub fn alternate_config(&self, endpoint: &str) -> PathBuf {
        let path = self.home.path().join("alternate.toml");
        write_private(
            &path,
            format!("endpoint = {endpoint:?}\ntoken = \"SYNTHETIC_TLS_CANARY\"\n").as_bytes(),
        );
        path
    }
}
fn caddy_config(addr: SocketAddr, upstream: SocketAddr) -> String {
    let source = include_str!("../../../../deploy/Caddyfile");
    let (_, block) = source.split_once("reverse_proxy 127.0.0.1:7777 {").unwrap();
    let (directives, _) = block.split_once('}').unwrap();
    assert!(!directives.contains('{') && !directives.contains("header_up Connection close"));
    assert!(source.contains("reverse_proxy 127.0.0.1:7777\n}"));
    format!("{{\n admin off\n auto_https off\n}}\nhttp://*.t.test:{} {{\n bind 127.0.0.1\n reverse_proxy {upstream} {{{directives}\n }}\n}}\nhttp://127.0.0.1:{} {{\n bind 127.0.0.1\n reverse_proxy {upstream}\n}}\n", addr.port(), addr.port())
}

pub struct Inspector {
    pub origin: String,
    token: String,
    pub addr: SocketAddr,
}
impl Inspector {
    pub fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        http()
            .request(method, format!("{}{path}", self.origin))
            .bearer_auth(&self.token)
    }
    pub fn mutation(&self, path: &str, body: &Value) -> reqwest::RequestBuilder {
        self.request(reqwest::Method::POST, path)
            .header("Origin", &self.origin)
            .header("Sec-Fetch-Site", "same-origin")
            .header("Sec-Fetch-Mode", "cors")
            .header("Sec-Fetch-Dest", "empty")
            .json(body)
    }
    // Raw bytes preserve duplicates and invalid JSON/encoding that builders normalize.
    pub async fn raw_mutation(&self, path: &str, body: &[u8], extra: &str) -> u16 {
        tokio::time::timeout(DEADLINE, async {
            let mut io = tokio::net::TcpStream::connect(self.addr).await.unwrap();
            let wire = format!("POST {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nOrigin: {}\r\nSec-Fetch-Site: same-origin\r\nSec-Fetch-Mode: cors\r\nSec-Fetch-Dest: empty\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}\r\n", self.addr, self.token, self.origin, body.len());
            assert!(wire.len() + body.len() <= 40 * 1024, "bounded adversarial request");
            io.write_all(&[wire.as_bytes(), body].concat()).await.unwrap();
            let mut response = Vec::new();
            let read = io.take(65537).read_to_end(&mut response).await;
            if let Err(error) = read {
                assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
            }
            assert!(response.len() <= 65536, "bounded raw response");
            let response = String::from_utf8(response).unwrap();
            let (head, body) = response.split_once("\r\n\r\n").expect("real HTTP rejection response");
            assert_eq!(header(head, "content-length").parse::<usize>().unwrap(), body.len());
            assert_eq!(header(head, "cache-control"), "no-store");
            assert!(!head.to_ascii_lowercase().contains("access-control-allow"), "no CORS grants");
            let _: Value = serde_json::from_str(body).expect("real API JSON rejection");
            head.split_whitespace().nth(1).unwrap().parse().unwrap()
        }).await.expect("bounded raw authenticated mutation")
    }
    pub async fn get(&self, path: &str) -> Value {
        let response = self
            .request(reqwest::Method::GET, path)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "authenticated inspector read");
        response.json().await.expect("inspector JSON")
    }
    pub async fn capture(&self, id: &str) -> Value {
        tokio::time::timeout(DEADLINE, async {
            loop {
                let list = self.get("/api/v1/requests").await;
                assert_eq!(list["schema_version"], 1);
                for item in list["items"].as_array().expect("list items") {
                    if item["request_id"] == id && item["state"] == "complete" {
                        return self
                            .get(&format!(
                                "/api/v1/requests/{}",
                                item["capture_id"].as_str().unwrap()
                            ))
                            .await;
                    }
                }
                poll().await;
            }
        })
        .await
        .expect("bounded complete inspector capture")
    }
}

pub struct Task<T>(pub tokio::task::JoinHandle<T>);
impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub struct Observation {
    pub head: String,
    pub body: Vec<u8>,
}
enum NextResponse {
    Created,
    Close,
    Redirect(SocketAddr),
}
pub struct Backend {
    pub addr: SocketAddr,
    pub observed: tokio::sync::mpsc::Receiver<Observation>,
    _task: Task<()>,
    response: std::sync::Arc<std::sync::Mutex<NextResponse>>,
    connections: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl Backend {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, observed) = tokio::sync::mpsc::channel(16);
        let response = std::sync::Arc::new(std::sync::Mutex::new(NextResponse::Created));
        let responses = response.clone();
        let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let accepted = connections.clone();
        let task = Task(tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            loop {
                let (io, _) = listener.accept().await.unwrap();
                accepted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tasks.spawn(backend_connection(io, tx.clone(), responses.clone()));
            }
        }));
        Self {
            addr,
            observed,
            _task: task,
            response,
            connections,
        }
    }
    pub fn fail_next_response(&self) {
        *self.response.lock().unwrap() = NextResponse::Close;
    }
    pub fn redirect_next_response(&self, target: SocketAddr) {
        assert!(
            target.ip().is_loopback(),
            "redirect sink must be fixture-owned loopback"
        );
        *self.response.lock().unwrap() = NextResponse::Redirect(target);
    }
    pub fn connections(&self) -> usize {
        self.connections.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub async fn unchanged_connections(&mut self, before: usize) {
        self.none().await;
        assert_eq!(
            self.connections(),
            before,
            "unexpected backend TCP connection"
        );
    }
    pub async fn next(&mut self) -> Observation {
        tokio::time::timeout(DEADLINE, self.observed.recv())
            .await
            .unwrap()
            .unwrap()
    }
    pub async fn none(&mut self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(150), self.observed.recv())
                .await
                .is_err(),
            "unexpected backend delivery"
        );
    }
}
pub async fn head(io: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        bytes.push(io.read_u8().await.unwrap());
        assert!(bytes.len() < 32768);
    }
    String::from_utf8(bytes).unwrap()
}
pub fn header(head: &str, name: &str) -> String {
    let values: Vec<_> = head
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim())
        })
        .collect();
    assert_eq!(values.len(), 1, "expected exactly one named header");
    values[0].into()
}
async fn backend_connection(
    mut io: tokio::net::TcpStream,
    tx: tokio::sync::mpsc::Sender<Observation>,
    response: std::sync::Arc<std::sync::Mutex<NextResponse>>,
) {
    use async_tungstenite::{
        tokio::TokioAdapter,
        tungstenite::{protocol::Role, Message},
        WebSocketStream,
    };
    use futures::{SinkExt, StreamExt};
    let head = head(&mut io).await;
    let length = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    assert!(length < 65536);
    let mut body = vec![0; length];
    io.read_exact(&mut body).await.unwrap();
    let upgrade = head
        .to_ascii_lowercase()
        .contains("connection: upgrade\r\n");
    tx.send(Observation { head, body }).await.unwrap();
    let next = std::mem::replace(&mut *response.lock().unwrap(), NextResponse::Created);
    match next {
        NextResponse::Close => {
            io.shutdown().await.unwrap();
            return;
        }
        NextResponse::Redirect(target) => {
            io.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://{target}/redirect-target\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
            io.shutdown().await.unwrap();
            return;
        }
        NextResponse::Created => {}
    }
    if upgrade {
        io.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\nX-Tnl-Request-Id: backend-forgery\r\n\r\n\x82\x05first").await.unwrap();
        let mut ws =
            WebSocketStream::from_raw_socket(TokioAdapter::new(io), Role::Server, None).await;
        while let Some(message) = ws.next().await {
            match message.unwrap() {
                Message::Close(_) => {
                    ws.flush().await.unwrap();
                    break;
                }
                message => ws.send(message).await.unwrap(),
            }
        }
    } else {
        let body = b"{\"ok\":true,\"password\":\"RESPONSE_CANARY\"}";
        io.write_all(format!("HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nSet-Cookie: SESSION_CANARY\r\nX-Tnl-Request-Id: backend-forgery\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
        io.write_all(body).await.unwrap();
        io.shutdown().await.unwrap();
    }
}

pub async fn public_post(fixture: &Fixture, name: &str) -> (String, Vec<u8>) {
    let body = br#"{"message":"hello","password":"BODY_CANARY"}"#.to_vec();
    let response = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .local_address("127.0.0.2".parse::<std::net::IpAddr>().unwrap())
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap()
        .post(format!("http://{}/submit", fixture.addr))
        .header("Host", format!("{name}.t.test"))
        .header("X-Forwarded-For", "203.0.113.66")
        .header("X-Tnl-Request-Id", "visitor-forgery")
        .header("Authorization", "Bearer AUTH_CANARY")
        .header("Cookie", "COOKIE_CANARY")
        .header("Content-Type", "application/json")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    assert_eq!(
        response
            .headers()
            .get_all("x-tnl-request-id")
            .iter()
            .count(),
        1
    );
    let id = response.headers()["x-tnl-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(id.len(), 26);
    assert_ne!(id, "visitor-forgery");
    response.bytes().await.unwrap();
    (id, body)
}
