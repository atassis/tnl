//! Bounded raw TCP fault edge. EOF is observed, NEVER forwarded: both owned
//! halves stay alive until explicit release, including after the CLI closes.
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Pass,
    ClientToServer,
    ServerToClient,
    Both,
}
impl Mode {
    fn drops(self, direction: usize) -> bool {
        self == Self::Both
            || (direction == 0 && self == Self::ClientToServer)
            || (direction == 1 && self == Self::ServerToClient)
    }
}
#[derive(Default)]
pub struct Counters {
    pub dropped: [AtomicUsize; 2],
    pub eof: [AtomicUsize; 2],
    observed: [AtomicUsize; 2],
}
pub struct Connection {
    mode: watch::Sender<(usize, Mode)>,
    release: watch::Sender<bool>,
    pub counters: Arc<Counters>,
}
impl Connection {
    pub async fn blackhole(&self, mode: Mode) {
        self.mode.send((1, mode)).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while self
                .counters
                .observed
                .iter()
                .any(|v| v.load(Ordering::SeqCst) != 1)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("both relay pumps acknowledged fault barrier");
    }
    pub fn release(&self) {
        let _ = self.release.send(true);
    }
}
pub struct Relay {
    pub addr: SocketAddr,
    pub connections: mpsc::Receiver<Connection>,
    pub active: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Relay {
    pub async fn start(upstream: SocketAddr) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (events, connections) = mpsc::channel(8);
        let active = Arc::new(AtomicUsize::new(0));
        let count = active.clone();
        let task = tokio::spawn(async move {
            let mut tasks = tokio::task::JoinSet::new();
            let limit = Arc::new(tokio::sync::Semaphore::new(8));
            loop {
                tokio::select! {
                    Some(result) = tasks.join_next(), if !tasks.is_empty() => { result.unwrap(); }
                    accepted = listener.accept() => {
                        let (client, _) = accepted.unwrap();
                        let permit = limit.clone().try_acquire_owned().expect("relay connection bound");
                        let server = tokio::net::TcpStream::connect(upstream).await.unwrap();
                        let (mode, changes) = watch::channel((0, Mode::Pass));
                        let (release, until_release) = watch::channel(false);
                        let counters = Arc::new(Counters::default());
                        events.send(Connection { mode, release, counters: counters.clone() }).await.unwrap();
                        let count = count.clone();
                        count.fetch_add(1, Ordering::SeqCst);
                        tasks.spawn(async move {
                            let _permit = permit;
                            let (cr, cw) = client.into_split();
                            let (sr, sw) = server.into_split();
                            tokio::select! {
                                () = async { tokio::join!(
                                    pump(cr, sw, 0, changes.clone(), counters.clone()),
                                    pump(sr, cw, 1, changes, counters));
                                } => {}
                                () = async { let mut release = until_release; let _ = release.wait_for(|v| *v).await; } => {}
                            }
                            count.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                }
            }
        });
        Self {
            addr,
            connections,
            active,
            task,
        }
    }
    pub async fn next(&mut self) -> Connection {
        tokio::time::timeout(std::time::Duration::from_secs(30), self.connections.recv())
            .await
            .expect("new physical connection")
            .unwrap()
    }
}
async fn pump(
    mut read: impl AsyncRead + Unpin,
    mut write: impl AsyncWrite + Unpin,
    direction: usize,
    mut changes: watch::Receiver<(usize, Mode)>,
    counters: Arc<Counters>,
) {
    let mut buf = [0; 4096]; // constant storage, drop rather than queue faulted bytes
    let mut eof = false;
    loop {
        let (epoch, mode) = *changes.borrow_and_update();
        counters.observed[direction].store(epoch, Ordering::SeqCst);
        tokio::select! {
            biased;
            update = changes.changed() => { if update.is_err() { std::future::pending::<()>().await; } }
            result = read.read(&mut buf), if !eof => {
                match result {
                    Ok(0) | Err(_) => { eof = true; counters.eof[direction].fetch_add(1, Ordering::SeqCst); }
                    Ok(n) if mode.drops(direction) => { counters.dropped[direction].fetch_add(n, Ordering::SeqCst); }
                    Ok(n) => { if write.write_all(&buf[..n]).await.is_err() { eof = true; } }
                }
            }
        }
    }
}

#[tokio::test]
async fn blackhole_keeps_upstream_open_after_client_eof() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut relay = Relay::start(listener.local_addr().unwrap()).await;
    let mut client = tokio::net::TcpStream::connect(relay.addr).await.unwrap();
    let (mut server, _) = listener.accept().await.unwrap();
    let connection = relay.next().await;
    connection.blackhole(Mode::Both).await;
    client.write_all(b"not forwarded").await.unwrap();
    drop(client);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while connection.counters.eof[0].load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), server.read_u8())
            .await
            .is_err(),
        "negative control: relayed FIN would produce immediate EOF"
    );
    assert_eq!(connection.counters.dropped[0].load(Ordering::SeqCst), 13);
    connection.release();
    assert!(server.read_u8().await.is_err());
}
