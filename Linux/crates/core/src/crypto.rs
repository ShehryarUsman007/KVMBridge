//! Encrypted, mutually authenticated channels between devices.
//!
//! Every TCP connection starts with a 7 byte plaintext preface that names the
//! protocol version, the AEAD cipher and the purpose of the connection. The
//! responder answers with a one byte status. Then a Noise `XX` handshake runs
//! with the preface bound into the prologue (so a tampered preface makes the
//! handshake fail). After the handshake every frame is
//! `u16 length (big endian) || AEAD ciphertext` with implicit counter nonces.
//!
//! Supported ciphers: AES-256-GCM and ChaCha20-Poly1305 (both 256-bit keys,
//! X25519 key agreement, BLAKE2s hashing).

use rand::RngCore;
use serde::{Deserialize, Serialize};
use snow::params::NoiseParams;
use snow::{Builder, StatelessTransportState};
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAGIC: &[u8; 4] = b"KVMB";
pub const WIRE_VERSION: u8 = 1;
/// Largest Noise message (ciphertext incl. tag).
pub const MAX_FRAME: usize = 65535;
pub const TAG_LEN: usize = 16;
/// Largest plaintext that fits into a single frame.
pub const MAX_PLAINTEXT: usize = MAX_FRAME - TAG_LEN;
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// AEAD cipher used for a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Cipher {
    #[serde(rename = "aes-256-gcm")]
    Aes256Gcm,
    #[serde(rename = "chacha20-poly1305")]
    ChaCha20Poly1305,
}

impl Cipher {
    pub fn wire_id(self) -> u8 {
        match self {
            Cipher::Aes256Gcm => 1,
            Cipher::ChaCha20Poly1305 => 2,
        }
    }
    pub fn from_wire(id: u8) -> Option<Cipher> {
        match id {
            1 => Some(Cipher::Aes256Gcm),
            2 => Some(Cipher::ChaCha20Poly1305),
            _ => None,
        }
    }
    pub fn noise_params(self) -> NoiseParams {
        let s = match self {
            Cipher::Aes256Gcm => "Noise_XX_25519_AESGCM_BLAKE2s",
            Cipher::ChaCha20Poly1305 => "Noise_XX_25519_ChaChaPoly_BLAKE2s",
        };
        s.parse().expect("valid noise params")
    }
    pub fn label(self) -> &'static str {
        match self {
            Cipher::Aes256Gcm => "AES-256-GCM",
            Cipher::ChaCha20Poly1305 => "ChaCha20-Poly1305",
        }
    }
}

/// What the user picked in settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CipherPreference {
    /// AES-256-GCM when the CPU has AES instructions, ChaCha20-Poly1305 otherwise.
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "aes-256-gcm")]
    Aes256Gcm,
    #[serde(rename = "chacha20-poly1305")]
    ChaCha20Poly1305,
}

impl CipherPreference {
    pub fn resolve(self) -> Cipher {
        match self {
            CipherPreference::Aes256Gcm => Cipher::Aes256Gcm,
            CipherPreference::ChaCha20Poly1305 => Cipher::ChaCha20Poly1305,
            CipherPreference::Auto => {
                if has_hw_aes() {
                    Cipher::Aes256Gcm
                } else {
                    Cipher::ChaCha20Poly1305
                }
            }
        }
    }
}

pub fn has_hw_aes() -> bool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        std::arch::is_x86_feature_detected!("aes") && std::arch::is_x86_feature_detected!("pclmulqdq")
    }
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("aes")
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

/// Purpose of a connection, sent in the preface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// Long lived control / input / clipboard channel.
    Control = 0,
    /// Bulk data channel for one file / clipboard transfer.
    Data = 1,
}

impl Purpose {
    fn from_wire(b: u8) -> Option<Purpose> {
        match b {
            0 => Some(Purpose::Control),
            1 => Some(Purpose::Data),
            _ => None,
        }
    }
}

/// Status byte the responder sends after reading the preface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefaceStatus {
    Ok = 0,
    BadVersion = 1,
    CipherNotAllowed = 2,
    Busy = 3,
}

#[derive(Debug, thiserror::Error)]
pub enum HandshakeError {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("not a KVM Bridge peer")]
    BadMagic,
    #[error("peer runs an incompatible protocol version")]
    BadVersion,
    #[error("peer does not allow cipher {0}")]
    CipherNotAllowed(&'static str),
    #[error("peer is busy")]
    Busy,
    #[error("noise handshake failed: {0}")]
    Noise(String),
    #[error("handshake timed out")]
    Timeout,
    #[error("invalid handshake payload")]
    BadPayload,
}

impl From<snow::Error> for HandshakeError {
    fn from(e: snow::Error) -> Self {
        HandshakeError::Noise(e.to_string())
    }
}

/// Long-term X25519 identity of this device.
#[derive(Clone)]
pub struct Identity {
    pub private: [u8; 32],
    pub public: [u8; 32],
}

impl Identity {
    pub fn generate() -> Identity {
        let kp = Builder::new(Cipher::ChaCha20Poly1305.noise_params())
            .generate_keypair()
            .expect("keypair generation");
        let mut private = [0u8; 32];
        let mut public = [0u8; 32];
        private.copy_from_slice(&kp.private);
        public.copy_from_slice(&kp.public);
        Identity { private, public }
    }

    /// Recompute the public key for a stored private key.
    pub fn from_private(private: [u8; 32]) -> Identity {
        // Noise does not expose scalar multiplication directly; perform a
        // throwaway "N" handshake-free derivation through the builder.
        let public = derive_public(&private);
        Identity { private, public }
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public)
    }
}

fn derive_public(private: &[u8; 32]) -> [u8; 32] {
    // A Noise "XN" initiator with a local static key writes its public static
    // key in plaintext-equivalent form in message 3; simpler: use the
    // resolver's DH directly.
    use snow::resolvers::{CryptoResolver, DefaultResolver};
    let mut dh = DefaultResolver
        .resolve_dh(&snow::params::DHChoice::Curve25519)
        .expect("curve25519 available");
    dh.set(private);
    let mut out = [0u8; 32];
    out.copy_from_slice(dh.pubkey());
    out
}

/// Human readable fingerprint of a public key: 16 hex chars in groups of 4.
pub fn fingerprint(public: &[u8]) -> String {
    use blake2::{Blake2s256, Digest};
    let h = Blake2s256::digest(public);
    let hexed = hex::encode(&h[..8]);
    hexed
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap().to_uppercase())
        .collect::<Vec<_>>()
        .join("-")
}

/// Six digit short authentication string both users compare when pairing.
pub fn sas_code(handshake_hash: &[u8]) -> String {
    use blake2::{Blake2s256, Digest};
    let mut h = Blake2s256::new();
    h.update(b"kvmbridge-sas-v1");
    h.update(handshake_hash);
    let d = h.finalize();
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 1_000_000;
    format!("{:03} {:03}", n / 1000, n % 1000)
}

/// Result of a completed handshake.
pub struct Established {
    pub transport: StatelessTransportState,
    pub remote_static: [u8; 32],
    pub handshake_hash: Vec<u8>,
    pub cipher: Cipher,
    pub purpose: Purpose,
    /// Application payload the peer sent inside the handshake.
    pub remote_payload: Vec<u8>,
    pub initiator: bool,
}

impl Established {
    pub fn sas(&self) -> String {
        sas_code(&self.handshake_hash)
    }

    /// Split into an independent reader and writer over the given stream halves.
    pub fn into_channels<R, W>(self, r: R, w: W) -> (SecureReader<R>, SecureWriter<W>) {
        let t = Arc::new(self.transport);
        (
            SecureReader { inner: r, state: t.clone(), nonce: 0, cbuf: vec![0u8; MAX_FRAME] },
            SecureWriter { inner: w, state: t, nonce: 0, buf: vec![0u8; MAX_FRAME + 2] },
        )
    }
}

fn preface(cipher: Cipher, purpose: Purpose) -> [u8; 7] {
    [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], WIRE_VERSION, cipher.wire_id(), purpose as u8]
}

async fn write_hs_frame<S: AsyncWrite + Unpin>(s: &mut S, data: &[u8]) -> io::Result<()> {
    let mut v = Vec::with_capacity(data.len() + 2);
    v.extend_from_slice(&(data.len() as u16).to_be_bytes());
    v.extend_from_slice(data);
    s.write_all(&v).await?;
    s.flush().await
}

async fn read_hs_frame<S: AsyncRead + Unpin>(s: &mut S) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 2];
    s.read_exact(&mut len).await?;
    let n = u16::from_be_bytes(len) as usize;
    let mut v = vec![0u8; n];
    s.read_exact(&mut v).await?;
    Ok(v)
}

/// Run the initiator side of the handshake.
pub async fn initiate<S>(
    stream: &mut S,
    id: &Identity,
    cipher: Cipher,
    purpose: Purpose,
    payload: &[u8],
) -> Result<Established, HandshakeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    tokio::time::timeout(HANDSHAKE_TIMEOUT, initiate_inner(stream, id, cipher, purpose, payload))
        .await
        .map_err(|_| HandshakeError::Timeout)?
}

async fn initiate_inner<S>(
    stream: &mut S,
    id: &Identity,
    cipher: Cipher,
    purpose: Purpose,
    payload: &[u8],
) -> Result<Established, HandshakeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let pre = preface(cipher, purpose);
    stream.write_all(&pre).await?;
    stream.flush().await?;
    let mut status = [0u8; 1];
    stream.read_exact(&mut status).await?;
    match status[0] {
        0 => {}
        1 => return Err(HandshakeError::BadVersion),
        2 => return Err(HandshakeError::CipherNotAllowed(cipher.label())),
        3 => return Err(HandshakeError::Busy),
        _ => return Err(HandshakeError::BadMagic),
    }

    let mut hs = Builder::new(cipher.noise_params())
        .local_private_key(&id.private)?
        .prologue(&pre)?
        .build_initiator()?;
    let mut buf = vec![0u8; MAX_FRAME];
    let mut pbuf = vec![0u8; MAX_FRAME];

    // -> e
    let n = hs.write_message(&[], &mut buf)?;
    write_hs_frame(stream, &buf[..n]).await?;
    // <- e, ee, s, es (+ responder payload)
    let msg = read_hs_frame(stream).await?;
    let n = hs.read_message(&msg, &mut pbuf)?;
    let remote_payload = pbuf[..n].to_vec();
    // -> s, se (+ our payload)
    if payload.len() > MAX_PLAINTEXT - 64 {
        return Err(HandshakeError::BadPayload);
    }
    let n = hs.write_message(payload, &mut buf)?;
    write_hs_frame(stream, &buf[..n]).await?;

    finish(hs, cipher, purpose, remote_payload, true)
}

/// Run the responder side. `allowed` decides which ciphers are accepted.
pub async fn respond<S, F, P>(
    stream: &mut S,
    id: &Identity,
    allowed: F,
    payload_for: P,
) -> Result<Established, HandshakeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: Fn(Cipher, Purpose) -> bool,
    P: FnOnce(Purpose) -> Vec<u8>,
{
    tokio::time::timeout(HANDSHAKE_TIMEOUT, respond_inner(stream, id, allowed, payload_for))
        .await
        .map_err(|_| HandshakeError::Timeout)?
}

async fn respond_inner<S, F, P>(
    stream: &mut S,
    id: &Identity,
    allowed: F,
    payload_for: P,
) -> Result<Established, HandshakeError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: Fn(Cipher, Purpose) -> bool,
    P: FnOnce(Purpose) -> Vec<u8>,
{
    let mut pre = [0u8; 7];
    stream.read_exact(&mut pre).await?;
    if &pre[..4] != MAGIC {
        return Err(HandshakeError::BadMagic);
    }
    if pre[4] != WIRE_VERSION {
        let _ = stream.write_all(&[PrefaceStatus::BadVersion as u8]).await;
        return Err(HandshakeError::BadVersion);
    }
    let (cipher, purpose) = match (Cipher::from_wire(pre[5]), Purpose::from_wire(pre[6])) {
        (Some(c), Some(p)) => (c, p),
        _ => {
            let _ = stream.write_all(&[PrefaceStatus::BadVersion as u8]).await;
            return Err(HandshakeError::BadVersion);
        }
    };
    if !allowed(cipher, purpose) {
        let _ = stream.write_all(&[PrefaceStatus::CipherNotAllowed as u8]).await;
        return Err(HandshakeError::CipherNotAllowed(cipher.label()));
    }
    stream.write_all(&[PrefaceStatus::Ok as u8]).await?;
    stream.flush().await?;

    let mut hs = Builder::new(cipher.noise_params())
        .local_private_key(&id.private)?
        .prologue(&pre)?
        .build_responder()?;
    let mut buf = vec![0u8; MAX_FRAME];
    let mut pbuf = vec![0u8; MAX_FRAME];

    // <- e
    let msg = read_hs_frame(stream).await?;
    hs.read_message(&msg, &mut pbuf)?;
    // -> e, ee, s, es (+ our payload)
    let payload = payload_for(purpose);
    if payload.len() > MAX_PLAINTEXT - 128 {
        return Err(HandshakeError::BadPayload);
    }
    let n = hs.write_message(&payload, &mut buf)?;
    write_hs_frame(stream, &buf[..n]).await?;
    // <- s, se (+ initiator payload)
    let msg = read_hs_frame(stream).await?;
    let n = hs.read_message(&msg, &mut pbuf)?;
    let remote_payload = pbuf[..n].to_vec();

    finish(hs, cipher, purpose, remote_payload, false)
}

fn finish(
    hs: snow::HandshakeState,
    cipher: Cipher,
    purpose: Purpose,
    remote_payload: Vec<u8>,
    initiator: bool,
) -> Result<Established, HandshakeError> {
    let rs = hs.get_remote_static().ok_or(HandshakeError::BadPayload)?;
    let mut remote_static = [0u8; 32];
    remote_static.copy_from_slice(rs);
    let handshake_hash = hs.get_handshake_hash().to_vec();
    let transport = hs.into_stateless_transport_mode()?;
    Ok(Established { transport, remote_static, handshake_hash, cipher, purpose, remote_payload, initiator })
}

/// Decrypting half of a secure channel.
pub struct SecureReader<R> {
    inner: R,
    state: Arc<StatelessTransportState>,
    nonce: u64,
    cbuf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> SecureReader<R> {
    /// Read and decrypt the next frame into `out` (replacing its contents).
    pub async fn recv_into(&mut self, out: &mut Vec<u8>) -> io::Result<()> {
        let mut len = [0u8; 2];
        self.inner.read_exact(&mut len).await?;
        let n = u16::from_be_bytes(len) as usize;
        if n < TAG_LEN {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "short frame"));
        }
        self.inner.read_exact(&mut self.cbuf[..n]).await?;
        out.resize(n, 0);
        let m = self
            .state
            .read_message(self.nonce, &self.cbuf[..n], out)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("decrypt: {e}")))?;
        self.nonce += 1;
        out.truncate(m);
        Ok(())
    }

    pub async fn recv(&mut self) -> io::Result<Vec<u8>> {
        let mut v = Vec::new();
        self.recv_into(&mut v).await?;
        Ok(v)
    }
}

/// Encrypting half of a secure channel.
pub struct SecureWriter<W> {
    inner: W,
    state: Arc<StatelessTransportState>,
    nonce: u64,
    buf: Vec<u8>,
}

impl<W: AsyncWrite + Unpin> SecureWriter<W> {
    /// Encrypt a frame into the internal buffer without writing it.
    fn seal(&mut self, plaintext: &[u8]) -> io::Result<usize> {
        if plaintext.len() > MAX_PLAINTEXT {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "frame too large"));
        }
        let n = self
            .state
            .write_message(self.nonce, plaintext, &mut self.buf[2..])
            .map_err(|e| io::Error::other(format!("encrypt: {e}")))?;
        self.nonce += 1;
        self.buf[..2].copy_from_slice(&(n as u16).to_be_bytes());
        Ok(n + 2)
    }

    /// Encrypt and send one frame (a single write call, no extra flush needed
    /// for TCP streams with `TCP_NODELAY`).
    pub async fn send(&mut self, plaintext: &[u8]) -> io::Result<()> {
        let n = self.seal(plaintext)?;
        self.inner.write_all(&self.buf[..n]).await?;
        self.inner.flush().await
    }

    /// Like [`send`](Self::send) but without flushing (for buffered writers).
    pub async fn send_unflushed(&mut self, plaintext: &[u8]) -> io::Result<()> {
        let n = self.seal(plaintext)?;
        self.inner.write_all(&self.buf[..n]).await
    }

    pub async fn flush(&mut self) -> io::Result<()> {
        self.inner.flush().await
    }

    pub async fn shutdown(&mut self) -> io::Result<()> {
        self.inner.shutdown().await
    }
}

/// Random 64 bit identifier.
pub fn random_u64() -> u64 {
    rand::rng().next_u64()
}
