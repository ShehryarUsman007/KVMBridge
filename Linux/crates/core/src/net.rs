//! TCP listener / dialer that performs the encrypted handshake and runs the
//! per-connection reader and writer tasks.

use crate::crypto::{self, Cipher, Established, HandshakeError, Identity, Purpose, SecureReader, SecureWriter};
use crate::proto::{DeviceIntro, Msg};
use parking_lot::RwLock;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::BufWriter;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

pub type Reader = SecureReader<OwnedReadHalf>;
pub type Writer = SecureWriter<BufWriter<OwnedWriteHalf>>;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);

/// Which ciphers are acceptable for incoming / outgoing connections.
#[derive(Clone, Copy, Debug)]
pub struct CipherPolicy {
    pub preferred: Cipher,
    pub strict: bool,
}

impl CipherPolicy {
    pub fn allows(&self, c: Cipher) -> bool {
        !self.strict || c == self.preferred
    }
}

/// Shared state needed to open and accept connections.
pub struct NetCtx {
    pub identity: Arc<Identity>,
    pub intro: RwLock<DeviceIntro>,
    pub policy: RwLock<CipherPolicy>,
    pub events: mpsc::UnboundedSender<NetEvent>,
    next_id: AtomicU64,
}

impl NetCtx {
    pub fn new(
        identity: Arc<Identity>,
        intro: DeviceIntro,
        policy: CipherPolicy,
        events: mpsc::UnboundedSender<NetEvent>,
    ) -> Arc<NetCtx> {
        Arc::new(NetCtx {
            identity,
            intro: RwLock::new(intro),
            policy: RwLock::new(policy),
            events,
            next_id: AtomicU64::new(1),
        })
    }

    fn intro_bytes(&self) -> Vec<u8> {
        postcard::to_stdvec(&*self.intro.read()).expect("serialisable")
    }

    fn alloc_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

/// Something sent to a connection's writer task.
#[derive(Debug)]
pub enum Outgoing {
    Msg(Msg),
    /// Flush and close the connection.
    Close,
}

/// Handle the engine keeps for each live control connection.
#[derive(Clone, Debug)]
pub struct ConnHandle {
    pub id: u64,
    pub intro: DeviceIntro,
    pub remote_key: [u8; 32],
    pub addr: SocketAddr,
    pub cipher: Cipher,
    pub sas: String,
    pub initiator: bool,
    pub tx: mpsc::UnboundedSender<Outgoing>,
}

impl ConnHandle {
    pub fn send(&self, m: Msg) -> bool {
        self.tx.send(Outgoing::Msg(m)).is_ok()
    }
    pub fn close(&self) {
        let _ = self.tx.send(Outgoing::Close);
    }
}

/// An authenticated data channel waiting to be used for a transfer.
pub struct DataChannel {
    pub intro: DeviceIntro,
    pub remote_key: [u8; 32],
    pub addr: SocketAddr,
    pub reader: Reader,
    pub writer: Writer,
}

impl std::fmt::Debug for DataChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DataChannel({:?} @ {})", self.intro.id, self.addr)
    }
}

#[derive(Debug)]
pub enum NetEvent {
    /// A control connection finished its handshake.
    Connected { conn: ConnHandle, user_initiated: bool },
    Message { conn: u64, msg: Msg },
    Closed { conn: u64, reason: String },
    DialFailed { addr: SocketAddr, error: String, user_initiated: bool },
    /// Someone opened a data channel to us.
    IncomingData(Box<DataChannel>),
    /// The listener could not be started.
    ListenFailed { port: u16, error: String },
}

fn decode_intro(est: &Established) -> Result<DeviceIntro, HandshakeError> {
    postcard::from_bytes(&est.remote_payload).map_err(|_| HandshakeError::BadPayload)
}

fn split(est: Established, stream: TcpStream) -> (Reader, Writer) {
    let (r, w) = stream.into_split();
    est.into_channels(r, BufWriter::with_capacity(256 * 1024, w))
}

fn tune(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
    let sock = socket2::SockRef::from(stream);
    let _ = sock.set_keepalive(true);
    let ka = socket2::TcpKeepalive::new().with_time(Duration::from_secs(10)).with_interval(Duration::from_secs(5));
    let _ = sock.set_tcp_keepalive(&ka);
}

/// Bind the listener. Returns the bound port (useful when `port == 0`).
pub async fn listen(ctx: Arc<NetCtx>, port: u16) -> std::io::Result<(u16, tokio::task::JoinHandle<()>)> {
    let listener = match TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) if port != 0 => {
            tracing::warn!("cannot listen on TCP {port} ({e}); using a random port");
            TcpListener::bind(("0.0.0.0", 0)).await?
        }
        Err(e) => return Err(e),
    };
    let bound = listener.local_addr()?.port();
    tracing::info!("listening on TCP {bound}");
    let h = tokio::spawn(async move {
        loop {
            let (stream, addr) = match listener.accept().await {
                Ok(x) => x,
                Err(e) => {
                    tracing::warn!("accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
            };
            let ctx = ctx.clone();
            tokio::spawn(async move {
                if let Err(e) = accept_one(ctx, stream, addr).await {
                    tracing::debug!("incoming connection from {addr} failed: {e}");
                }
            });
        }
    });
    Ok((bound, h))
}

async fn accept_one(ctx: Arc<NetCtx>, mut stream: TcpStream, addr: SocketAddr) -> Result<(), HandshakeError> {
    tune(&stream);
    let policy = *ctx.policy.read();
    let payload = ctx.intro_bytes();
    let est = crypto::respond(&mut stream, &ctx.identity, |c, _| policy.allows(c), |_| payload).await?;
    let intro = decode_intro(&est)?;
    let remote_key = est.remote_static;
    match est.purpose {
        Purpose::Control => {
            let cipher = est.cipher;
            let sas = est.sas();
            let (r, w) = split(est, stream);
            start_control(ctx, r, w, intro, remote_key, addr, cipher, sas, false, false);
        }
        Purpose::Data => {
            let (reader, writer) = split(est, stream);
            let _ = ctx.events.send(NetEvent::IncomingData(Box::new(DataChannel {
                intro,
                remote_key,
                addr,
                reader,
                writer,
            })));
        }
    }
    Ok(())
}

/// Open a control connection in the background; the result arrives as a
/// [`NetEvent`].
pub fn dial_control(ctx: Arc<NetCtx>, addr: SocketAddr, user_initiated: bool) {
    tokio::spawn(async move {
        match connect(&ctx, addr, Purpose::Control).await {
            Ok((est, stream)) => match decode_intro(&est) {
                Ok(intro) => {
                    let remote_key = est.remote_static;
                    let cipher = est.cipher;
                    let sas = est.sas();
                    let (r, w) = split(est, stream);
                    start_control(ctx, r, w, intro, remote_key, addr, cipher, sas, true, user_initiated);
                }
                Err(e) => {
                    let _ = ctx.events.send(NetEvent::DialFailed { addr, error: e.to_string(), user_initiated });
                }
            },
            Err(e) => {
                let _ = ctx.events.send(NetEvent::DialFailed { addr, error: e, user_initiated });
            }
        }
    });
}

/// Open a data channel and verify the peer is who we expect.
pub async fn dial_data(ctx: &NetCtx, addr: SocketAddr, expect_key: &[u8; 32]) -> Result<DataChannel, String> {
    let (est, stream) = connect(ctx, addr, Purpose::Data).await?;
    if &est.remote_static != expect_key {
        return Err("data channel peer key mismatch".into());
    }
    let intro = decode_intro(&est).map_err(|e| e.to_string())?;
    let remote_key = est.remote_static;
    let (reader, writer) = split(est, stream);
    Ok(DataChannel { intro, remote_key, addr, reader, writer })
}

async fn connect(ctx: &NetCtx, addr: SocketAddr, purpose: Purpose) -> Result<(Established, TcpStream), String> {
    let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| format!("connection to {addr} timed out"))?
        .map_err(|e| format!("cannot connect to {addr}: {e}"))?;
    tune(&stream);
    let cipher = ctx.policy.read().preferred;
    let payload = ctx.intro_bytes();
    let est = crypto::initiate(&mut stream, &ctx.identity, cipher, purpose, &payload)
        .await
        .map_err(|e| e.to_string())?;
    Ok((est, stream))
}

#[allow(clippy::too_many_arguments)]
fn start_control(
    ctx: Arc<NetCtx>,
    mut reader: Reader,
    mut writer: Writer,
    intro: DeviceIntro,
    remote_key: [u8; 32],
    addr: SocketAddr,
    cipher: Cipher,
    sas: String,
    initiator: bool,
    user_initiated: bool,
) {
    let id = ctx.alloc_id();
    let (tx, mut rx) = mpsc::unbounded_channel::<Outgoing>();
    let handle = ConnHandle { id, intro, remote_key, addr, cipher, sas, initiator, tx };
    let events = ctx.events.clone();

    // Reader: decrypt frames and hand messages to the engine.
    let rd_events = events.clone();
    let reader_task = tokio::spawn(async move {
        let mut buf = Vec::with_capacity(4096);
        let reason = loop {
            if let Err(e) = reader.recv_into(&mut buf).await {
                break if e.kind() == std::io::ErrorKind::UnexpectedEof { "connection closed".to_string() } else { e.to_string() };
            }
            match Msg::decode(&buf) {
                Ok(msg) => {
                    if rd_events.send(NetEvent::Message { conn: id, msg }).is_err() {
                        return;
                    }
                }
                // Unknown message from a newer version: ignore it.
                Err(e) => tracing::debug!("undecodable message on conn {id}: {e}"),
            }
        };
        let _ = rd_events.send(NetEvent::Closed { conn: id, reason });
    });

    // Writer: serialise, encrypt and coalesce bursts into one syscall.
    tokio::spawn(async move {
        'outer: while let Some(first) = rx.recv().await {
            let mut next = Some(first);
            let mut n = 0;
            while let Some(item) = next.take() {
                match item {
                    Outgoing::Msg(m) => {
                        if writer.send_unflushed(&m.encode()).await.is_err() {
                            break 'outer;
                        }
                    }
                    Outgoing::Close => {
                        let _ = writer.flush().await;
                        let _ = writer.shutdown().await;
                        break 'outer;
                    }
                }
                n += 1;
                if n < 64 {
                    next = rx.try_recv().ok();
                }
            }
            if writer.flush().await.is_err() {
                break;
            }
        }
        // Make sure the reader stops as well so the engine sees `Closed`.
        tokio::time::sleep(Duration::from_millis(200)).await;
        reader_task.abort();
        let _ = events.send(NetEvent::Closed { conn: id, reason: "closed".into() });
    });

    let _ = ctx.events.send(NetEvent::Connected { conn: handle, user_initiated });
}
