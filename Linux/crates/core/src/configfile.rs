//! `.kvmb` — the KVM Bridge configuration file format.
//!
//! A small, versioned, sectioned container for backups of settings, the
//! screen arrangement, paired devices and (optionally, only in
//! password-protected files) this computer's identity. See `docs/config-file-format.md` for the byte-level
//! specification.
//!
//! ```text
//! header (16 bytes)
//!   "KVMBCONF"          magic
//!   u16 version         = 1
//!   u16 flags           bit0 encrypted, bit1 contains identity
//!   u32 body length
//! body (plain)          sections… || BLAKE2s-256(header || sections)
//! body (encrypted)      kdf params || cipher params || AEAD(sections)
//! section               u16 type || u32 length || data
//! ```

use crate::config::{Settings, SharedSettings, TrustedPeer};
use crate::crypto::Cipher;
use crate::layout::Layout;
use crate::types::DeviceId;
use serde::{Deserialize, Serialize};

pub const MAGIC: &[u8; 8] = b"KVMBCONF";
pub const VERSION: u16 = 1;
pub const EXTENSION: &str = "kvmb";

const FLAG_ENCRYPTED: u16 = 1 << 0;
const FLAG_IDENTITY: u16 = 1 << 1;
const HEADER_LEN: usize = 16;
const CHECKSUM_LEN: usize = 32;
const MAX_FILE: usize = 16 * 1024 * 1024;

const SEC_INFO: u16 = 1;
const SEC_SETTINGS: u16 = 2;
const SEC_SHARED: u16 = 3;
const SEC_LAYOUT: u16 = 4;
const SEC_DEVICES: u16 = 5;
const SEC_IDENTITY: u16 = 6;
// 7 held saved configs (before 0.1.4); such sections are skipped now.

const KDF_ARGON2ID: u8 = 1;
/// Argon2id parameters for new files (64 MiB, 3 passes).
const ARGON_M_KIB: u32 = 64 * 1024;
const ARGON_T: u32 = 3;
const ARGON_P: u8 = 1;

/// Who made the file and when.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BundleInfo {
    pub created_ms: u64,
    pub app_version: String,
    pub source_id: DeviceId,
    pub source_name: String,
    pub source_os: String,
    #[serde(default)]
    pub note: String,
}

/// This computer's identity (device id + private key).
#[derive(Clone, PartialEq)]
pub struct IdentityBackup {
    pub device_id: DeviceId,
    pub private_key: [u8; 32],
}

impl std::fmt::Debug for IdentityBackup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IdentityBackup({:?}, <private key>)", self.device_id)
    }
}

/// Everything a `.kvmb` file can hold. Each part is optional.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigBundle {
    pub info: BundleInfo,
    pub settings: Option<Settings>,
    pub shared: Option<SharedSettings>,
    pub layout: Option<Layout>,
    pub devices: Option<Vec<TrustedPeer>>,
    pub identity: Option<IdentityBackup>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum BundleError {
    #[error("This is not a KVM Bridge configuration file")]
    NotABundle,
    #[error("This configuration file was made by a newer version of KVM Bridge (format {0})")]
    UnsupportedVersion(u16),
    #[error("This configuration file is password protected")]
    PasswordRequired,
    #[error("Wrong password (or the file was modified)")]
    WrongPassword,
    #[error("The configuration file is damaged: {0}")]
    Corrupt(String),
    #[error("A password is required to include this computer's identity")]
    IdentityNeedsPassword,
    #[error("{0}")]
    Other(String),
}

/// What can be learned without decrypting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u16,
    pub encrypted: bool,
    pub has_identity: bool,
}

pub fn peek(bytes: &[u8]) -> Result<Header, BundleError> {
    if bytes.len() < HEADER_LEN || &bytes[..8] != MAGIC {
        return Err(BundleError::NotABundle);
    }
    let version = u16::from_be_bytes([bytes[8], bytes[9]]);
    if version > VERSION || version == 0 {
        return Err(BundleError::UnsupportedVersion(version));
    }
    let flags = u16::from_be_bytes([bytes[10], bytes[11]]);
    Ok(Header { version, encrypted: flags & FLAG_ENCRYPTED != 0, has_identity: flags & FLAG_IDENTITY != 0 })
}

fn push_section(out: &mut Vec<u8>, ty: u16, data: &[u8]) {
    out.extend_from_slice(&ty.to_be_bytes());
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
}

fn json<T: Serialize>(v: &T) -> Vec<u8> {
    serde_json::to_vec_pretty(v).expect("serialisable")
}

fn sections(b: &ConfigBundle) -> Vec<u8> {
    let mut s = Vec::new();
    push_section(&mut s, SEC_INFO, &json(&b.info));
    if let Some(x) = &b.settings {
        push_section(&mut s, SEC_SETTINGS, &json(x));
    }
    if let Some(x) = &b.shared {
        push_section(&mut s, SEC_SHARED, &json(x));
    }
    if let Some(x) = &b.layout {
        push_section(&mut s, SEC_LAYOUT, &json(x));
    }
    if let Some(x) = &b.devices {
        push_section(&mut s, SEC_DEVICES, &json(x));
    }
    if let Some(id) = &b.identity {
        let mut d = Vec::with_capacity(48);
        d.extend_from_slice(id.device_id.0.as_bytes());
        d.extend_from_slice(&id.private_key);
        push_section(&mut s, SEC_IDENTITY, &d);
    }
    s
}

fn header(flags: u16, body_len: usize) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..8].copy_from_slice(MAGIC);
    h[8..10].copy_from_slice(&VERSION.to_be_bytes());
    h[10..12].copy_from_slice(&flags.to_be_bytes());
    h[12..16].copy_from_slice(&(body_len as u32).to_be_bytes());
    h
}

fn checksum(parts: &[&[u8]]) -> [u8; 32] {
    use blake2::{Blake2s256, Digest};
    let mut h = Blake2s256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

fn derive_key(password: &str, salt: &[u8], m: u32, t: u32, p: u8) -> Result<[u8; 32], BundleError> {
    let params = argon2::Params::new(m, t, p as u32, Some(32)).map_err(|e| BundleError::Corrupt(e.to_string()))?;
    let a = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = [0u8; 32];
    a.hash_password_into(password.as_bytes(), salt, &mut key).map_err(|e| BundleError::Other(e.to_string()))?;
    Ok(key)
}

fn aead_seal(cipher: Cipher, key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], plain: &[u8]) -> Vec<u8> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    let payload = Payload { msg: plain, aad };
    match cipher {
        Cipher::Aes256Gcm => aes_gcm::Aes256Gcm::new(key.into()).encrypt(nonce.into(), payload),
        Cipher::ChaCha20Poly1305 => chacha20poly1305::ChaCha20Poly1305::new(key.into()).encrypt(nonce.into(), payload),
    }
    .expect("encryption cannot fail")
}

fn aead_open(cipher: Cipher, key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], ct: &[u8]) -> Option<Vec<u8>> {
    use aes_gcm::aead::{Aead, KeyInit, Payload};
    let payload = Payload { msg: ct, aad };
    match cipher {
        Cipher::Aes256Gcm => aes_gcm::Aes256Gcm::new(key.into()).decrypt(nonce.into(), payload).ok(),
        Cipher::ChaCha20Poly1305 => chacha20poly1305::ChaCha20Poly1305::new(key.into()).decrypt(nonce.into(), payload).ok(),
    }
}

/// Serialise a bundle. With a password the contents are encrypted with
/// `cipher` under an Argon2id-derived key; an identity can only be included
/// in encrypted files.
pub fn encode(b: &ConfigBundle, password: Option<&str>, cipher: Cipher) -> Result<Vec<u8>, BundleError> {
    let password = password.filter(|p| !p.is_empty());
    if b.identity.is_some() && password.is_none() {
        return Err(BundleError::IdentityNeedsPassword);
    }
    let secs = sections(b);
    let mut flags = 0u16;
    if b.identity.is_some() {
        flags |= FLAG_IDENTITY;
    }
    match password {
        None => {
            let h = header(flags, secs.len() + CHECKSUM_LEN);
            let sum = checksum(&[&h, &secs]);
            let mut out = Vec::with_capacity(HEADER_LEN + secs.len() + CHECKSUM_LEN);
            out.extend_from_slice(&h);
            out.extend_from_slice(&secs);
            out.extend_from_slice(&sum);
            Ok(out)
        }
        Some(pw) => {
            use rand::RngCore;
            flags |= FLAG_ENCRYPTED;
            let mut salt = [0u8; 16];
            let mut nonce = [0u8; 12];
            rand::rng().fill_bytes(&mut salt);
            rand::rng().fill_bytes(&mut nonce);
            let mut params = Vec::with_capacity(39);
            params.push(KDF_ARGON2ID);
            params.extend_from_slice(&ARGON_M_KIB.to_be_bytes());
            params.extend_from_slice(&ARGON_T.to_be_bytes());
            params.push(ARGON_P);
            params.extend_from_slice(&salt);
            params.push(cipher.wire_id());
            params.extend_from_slice(&nonce);
            let key = derive_key(pw, &salt, ARGON_M_KIB, ARGON_T, ARGON_P)?;
            let body_len = params.len() + secs.len() + 16;
            let h = header(flags, body_len);
            let mut aad = h.to_vec();
            aad.extend_from_slice(&params);
            let ct = aead_seal(cipher, &key, &nonce, &aad, &secs);
            let mut out = aad;
            out.extend_from_slice(&ct);
            Ok(out)
        }
    }
}

/// Parse a bundle, decrypting it if needed.
pub fn decode(bytes: &[u8], password: Option<&str>) -> Result<ConfigBundle, BundleError> {
    if bytes.len() > MAX_FILE {
        return Err(BundleError::Corrupt("file is too large".into()));
    }
    let hdr = peek(bytes)?;
    let body_len = u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
    if bytes.len() != HEADER_LEN + body_len {
        return Err(BundleError::Corrupt("unexpected length".into()));
    }
    let body = &bytes[HEADER_LEN..];
    let secs: Vec<u8> = if hdr.encrypted {
        let Some(pw) = password.filter(|p| !p.is_empty()) else { return Err(BundleError::PasswordRequired) };
        const PARAMS: usize = 1 + 4 + 4 + 1 + 16 + 1 + 12;
        if body.len() < PARAMS + 16 {
            return Err(BundleError::Corrupt("truncated".into()));
        }
        let p = &body[..PARAMS];
        if p[0] != KDF_ARGON2ID {
            return Err(BundleError::Corrupt("unknown key derivation".into()));
        }
        let m = u32::from_be_bytes([p[1], p[2], p[3], p[4]]);
        let t = u32::from_be_bytes([p[5], p[6], p[7], p[8]]);
        let par = p[9];
        // Refuse absurd parameters (denial of service through a crafted file).
        if !(8..=1024 * 1024).contains(&m) || !(1..=16).contains(&t) || !(1..=16).contains(&par) {
            return Err(BundleError::Corrupt("unreasonable key derivation parameters".into()));
        }
        let salt = &p[10..26];
        let cipher = Cipher::from_wire(p[26]).ok_or_else(|| BundleError::Corrupt("unknown cipher".into()))?;
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&p[27..39]);
        let key = derive_key(pw, salt, m, t, par)?;
        let aad = &bytes[..HEADER_LEN + PARAMS];
        aead_open(cipher, &key, &nonce, aad, &body[PARAMS..]).ok_or(BundleError::WrongPassword)?
    } else {
        if body.len() < CHECKSUM_LEN {
            return Err(BundleError::Corrupt("truncated".into()));
        }
        let (secs, sum) = body.split_at(body.len() - CHECKSUM_LEN);
        if checksum(&[&bytes[..HEADER_LEN], secs]) != sum {
            return Err(BundleError::Corrupt("checksum mismatch".into()));
        }
        secs.to_vec()
    };
    parse_sections(&secs, hdr)
}

fn parse_sections(mut s: &[u8], hdr: Header) -> Result<ConfigBundle, BundleError> {
    let bad = |what: &str, e: serde_json::Error| BundleError::Corrupt(format!("{what}: {e}"));
    let mut info = None;
    let mut b = ConfigBundle {
        info: BundleInfo {
            created_ms: 0,
            app_version: String::new(),
            source_id: DeviceId::nil(),
            source_name: String::new(),
            source_os: String::new(),
            note: String::new(),
        },
        settings: None,
        shared: None,
        layout: None,
        devices: None,
        identity: None,
    };
    while !s.is_empty() {
        if s.len() < 6 {
            return Err(BundleError::Corrupt("truncated section".into()));
        }
        let ty = u16::from_be_bytes([s[0], s[1]]);
        let len = u32::from_be_bytes([s[2], s[3], s[4], s[5]]) as usize;
        if s.len() < 6 + len {
            return Err(BundleError::Corrupt("truncated section".into()));
        }
        let data = &s[6..6 + len];
        match ty {
            SEC_INFO => info = Some(serde_json::from_slice(data).map_err(|e| bad("info", e))?),
            SEC_SETTINGS => b.settings = Some(serde_json::from_slice(data).map_err(|e| bad("settings", e))?),
            SEC_SHARED => b.shared = Some(serde_json::from_slice(data).map_err(|e| bad("encryption settings", e))?),
            SEC_LAYOUT => b.layout = Some(serde_json::from_slice(data).map_err(|e| bad("arrangement", e))?),
            SEC_DEVICES => b.devices = Some(serde_json::from_slice(data).map_err(|e| bad("devices", e))?),
            SEC_IDENTITY => {
                if !hdr.encrypted {
                    return Err(BundleError::Corrupt("identity in an unencrypted file".into()));
                }
                if data.len() != 48 {
                    return Err(BundleError::Corrupt("identity".into()));
                }
                let mut id = [0u8; 16];
                id.copy_from_slice(&data[..16]);
                let mut key = [0u8; 32];
                key.copy_from_slice(&data[16..]);
                b.identity = Some(IdentityBackup { device_id: DeviceId(uuid::Uuid::from_bytes(id)), private_key: key });
            }
            // Sections from newer versions are ignored.
            _ => {}
        }
        s = &s[6 + len..];
    }
    b.info = info.ok_or_else(|| BundleError::Corrupt("missing info section".into()))?;
    Ok(b)
}
