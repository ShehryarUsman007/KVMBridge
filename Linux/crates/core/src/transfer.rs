//! Streaming files and blobs over an encrypted data channel.

use crate::net::{Reader, Writer};
use crate::proto::data::{encode_ctrl, DataCtrl, CHUNK, TAG_CHUNK, TAG_CTRL};
use crate::proto::{ItemMeta, MAX_SUMMARY_ITEMS};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Safety limit on the number of entries in one transfer.
pub const MAX_ENTRIES: usize = 200_000;

/// One entry the sender will stream.
#[derive(Clone, Debug)]
pub enum Entry {
    Dir { rel: String },
    File { rel: String, path: PathBuf, size: u64, mtime: Option<i64> },
    Bytes { rel: String, data: Arc<Vec<u8>> },
}

impl Entry {
    pub fn size(&self) -> u64 {
        match self {
            Entry::Dir { .. } => 0,
            Entry::File { size, .. } => *size,
            Entry::Bytes { data, .. } => data.len() as u64,
        }
    }
}

/// What the sender prepared for a transfer.
#[derive(Clone, Debug, Default)]
pub struct Manifest {
    pub entries: Vec<Entry>,
    /// Top-level items for display (capped).
    pub summary: Vec<ItemMeta>,
    pub file_count: u32,
    pub total_bytes: u64,
}

/// Live progress counters shared between a transfer task and the engine.
#[derive(Debug, Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
}

impl Progress {
    pub fn new(total: u64) -> Arc<Progress> {
        let p = Progress::default();
        p.total.store(total, Ordering::Relaxed);
        Arc::new(p)
    }
    pub fn get(&self) -> (u64, u64) {
        (self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed))
    }
}

fn mtime_of(md: &std::fs::Metadata) -> Option<i64> {
    md.modified().ok()?.duration_since(SystemTime::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64)
}

fn file_name_string(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".to_string())
}

/// Walk the given paths (files or directories) and build a manifest.
/// Symbolic links are skipped. Blocking: run on a blocking thread.
pub fn build_manifest(paths: &[PathBuf]) -> std::io::Result<Manifest> {
    let mut m = Manifest::default();
    for p in paths {
        let md = std::fs::symlink_metadata(p)?;
        let name = file_name_string(p);
        if md.is_dir() {
            let before_bytes = m.total_bytes;
            m.entries.push(Entry::Dir { rel: name.clone() });
            walk_dir(p, &name, &mut m)?;
            if m.summary.len() < MAX_SUMMARY_ITEMS {
                m.summary.push(ItemMeta { rel_path: name, size: m.total_bytes - before_bytes, is_dir: true, mtime: mtime_of(&md) });
            }
        } else if md.is_file() {
            let size = md.len();
            m.entries.push(Entry::File { rel: name.clone(), path: p.clone(), size, mtime: mtime_of(&md) });
            m.total_bytes += size;
            m.file_count += 1;
            if m.summary.len() < MAX_SUMMARY_ITEMS {
                m.summary.push(ItemMeta { rel_path: name, size, is_dir: false, mtime: mtime_of(&md) });
            }
        }
        if m.entries.len() > MAX_ENTRIES {
            return Err(std::io::Error::other("too many files"));
        }
    }
    Ok(m)
}

fn walk_dir(dir: &Path, rel: &str, m: &mut Manifest) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Ok(md) = std::fs::symlink_metadata(e.path()) else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        let child_rel = format!("{rel}/{name}");
        if md.is_dir() {
            m.entries.push(Entry::Dir { rel: child_rel.clone() });
            walk_dir(&e.path(), &child_rel, m)?;
        } else if md.is_file() {
            m.entries.push(Entry::File { rel: child_rel, path: e.path(), size: md.len(), mtime: mtime_of(&md) });
            m.total_bytes += md.len();
            m.file_count += 1;
        }
        if m.entries.len() > MAX_ENTRIES {
            return Err(std::io::Error::other("too many files"));
        }
    }
    Ok(())
}

/// Manifest for an in-memory blob.
pub fn blob_manifest(name: &str, data: Vec<u8>) -> Manifest {
    let size = data.len() as u64;
    Manifest {
        entries: vec![Entry::Bytes { rel: name.to_string(), data: Arc::new(data) }],
        summary: vec![ItemMeta { rel_path: name.to_string(), size, is_dir: false, mtime: None }],
        file_count: 1,
        total_bytes: size,
    }
}

async fn recv_ctrl(r: &mut Reader, buf: &mut Vec<u8>) -> Result<DataCtrl, String> {
    r.recv_into(buf).await.map_err(|e| format!("data channel: {e}"))?;
    if buf.first() != Some(&TAG_CTRL) {
        return Err("unexpected data frame".into());
    }
    postcard::from_bytes(&buf[1..]).map_err(|e| format!("bad data frame: {e}"))
}

async fn send_ctrl(w: &mut Writer, c: &DataCtrl) -> Result<(), String> {
    w.send(&encode_ctrl(c)).await.map_err(|e| format!("data channel: {e}"))
}

/// Sender side: announce the transfer, stream every entry, wait for the verdict.
pub async fn send(
    mut r: Reader,
    mut w: Writer,
    transfer: u64,
    manifest: Manifest,
    progress: Arc<Progress>,
) -> Result<(), String> {
    let mut buf = Vec::new();
    send_ctrl(&mut w, &DataCtrl::Open { transfer }).await?;
    match recv_ctrl(&mut r, &mut buf).await? {
        DataCtrl::Ready { ok: true, .. } => {}
        DataCtrl::Ready { ok: false, error } => return Err(error.unwrap_or_else(|| "refused".into())),
        other => return Err(format!("unexpected reply {other:?}")),
    }
    let mut frame = vec![0u8; CHUNK + 1];
    frame[0] = TAG_CHUNK;
    for e in &manifest.entries {
        match e {
            Entry::Dir { rel } => {
                let c = encode_ctrl(&DataCtrl::Dir { rel_path: rel.clone() });
                w.send_unflushed(&c).await.map_err(|e| e.to_string())?;
            }
            Entry::File { rel, path, size, mtime } => {
                let mut f = File::open(path).await.map_err(|e| format!("{}: {e}", path.display()))?;
                let c = encode_ctrl(&DataCtrl::FileStart { rel_path: rel.clone(), size: *size, mtime: *mtime });
                w.send_unflushed(&c).await.map_err(|e| e.to_string())?;
                let mut left = *size;
                while left > 0 {
                    let want = (left as usize).min(CHUNK);
                    let n = f.read(&mut frame[1..1 + want]).await.map_err(|e| format!("{}: {e}", path.display()))?;
                    if n == 0 {
                        return Err(format!("{} shrank while sending", path.display()));
                    }
                    w.send_unflushed(&frame[..1 + n]).await.map_err(|e| e.to_string())?;
                    left -= n as u64;
                    progress.done.fetch_add(n as u64, Ordering::Relaxed);
                }
                w.send_unflushed(&encode_ctrl(&DataCtrl::FileEnd)).await.map_err(|e| e.to_string())?;
            }
            Entry::Bytes { rel, data } => {
                let c = encode_ctrl(&DataCtrl::FileStart { rel_path: rel.clone(), size: data.len() as u64, mtime: None });
                w.send_unflushed(&c).await.map_err(|e| e.to_string())?;
                for chunk in data.chunks(CHUNK) {
                    frame[1..1 + chunk.len()].copy_from_slice(chunk);
                    w.send_unflushed(&frame[..1 + chunk.len()]).await.map_err(|e| e.to_string())?;
                    progress.done.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                }
                w.send_unflushed(&encode_ctrl(&DataCtrl::FileEnd)).await.map_err(|e| e.to_string())?;
            }
        }
    }
    send_ctrl(&mut w, &DataCtrl::End).await?;
    match recv_ctrl(&mut r, &mut buf).await? {
        DataCtrl::Done { ok: true, .. } => {
            let _ = w.shutdown().await;
            Ok(())
        }
        DataCtrl::Done { ok: false, error } => Err(error.unwrap_or_else(|| "receiver failed".into())),
        other => Err(format!("unexpected reply {other:?}")),
    }
}

/// Receiver side, step 1: which transfer is this channel for?
pub async fn read_open(r: &mut Reader) -> Result<u64, String> {
    let mut buf = Vec::new();
    match recv_ctrl(r, &mut buf).await? {
        DataCtrl::Open { transfer } => Ok(transfer),
        other => Err(format!("expected Open, got {other:?}")),
    }
}

/// Tell the sender we will not accept this channel.
pub async fn refuse(mut w: Writer, reason: &str) {
    let _ = send_ctrl(&mut w, &DataCtrl::Ready { ok: false, error: Some(reason.to_string()) }).await;
    let _ = w.shutdown().await;
}

#[derive(Clone, Debug)]
pub enum Destination {
    /// Write files below this directory.
    Dir(PathBuf),
    /// Keep the (single) payload in memory.
    Memory,
}

#[derive(Debug)]
pub enum Received {
    /// Top-level paths that were created (in arrival order).
    Files(Vec<PathBuf>),
    Bytes(Vec<u8>),
}

/// Make a relative path from the wire safe for the local filesystem: no
/// absolute paths, no `..`, no characters the OS cannot store.
pub fn sanitize_rel_path(rel: &str) -> Option<Vec<String>> {
    if rel.is_empty() || rel.len() > 4096 || rel.contains('\0') {
        return None;
    }
    let mut out = Vec::new();
    for comp in rel.split('/') {
        if comp.is_empty() || comp == "." || comp == ".." {
            return None;
        }
        let mut c: String = comp
            .chars()
            .map(|ch| {
                if ch.is_control() || ch == '\\' || (cfg!(windows) && "<>:\"|?*".contains(ch)) {
                    '_'
                } else {
                    ch
                }
            })
            .collect();
        if cfg!(windows) {
            c = c.trim_end_matches(['.', ' ']).to_string();
            let stem = c.split('.').next().unwrap_or("").to_ascii_uppercase();
            let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
                || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.len() == 4
                    && stem.as_bytes()[3].is_ascii_digit());
            if reserved {
                c.insert(0, '_');
            }
        }
        if c.is_empty() || c.len() > 255 {
            return None;
        }
        // Double check nothing weird slipped through.
        if Path::new(&c).components().count() != 1
            || !matches!(Path::new(&c).components().next(), Some(Component::Normal(_)))
        {
            return None;
        }
        out.push(c);
    }
    Some(out)
}

/// `name (1).ext`, `name (2).ext`, ... until it does not exist.
pub fn unique_path(dir: &Path, name: &str, taken: &dyn Fn(&Path) -> bool) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() && !taken(&first) {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 1.. {
        let p = dir.join(format!("{stem} ({n}){ext}"));
        if !p.exists() && !taken(&p) {
            return p;
        }
    }
    unreachable!()
}

/// Holding folders (below the cache folder) for files that arrive for a drop
/// or a paste; the application they go to copies them from there.
pub const DRAG_STAGING: &str = "dnd";
pub const CLIPBOARD_STAGING: &str = "clipboard";
/// Holding folders older than this are removed at startup.
const STAGING_KEEP: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// A new holding folder below `cache/kind`.
pub fn staging_dir(cache: &Path, kind: &str) -> PathBuf {
    cache.join(kind).join(format!("{:x}", crate::crypto::random_u64()))
}

/// Removes holding folders left from earlier days.
pub fn prune_staging(cache: &Path) {
    for kind in [DRAG_STAGING, CLIPBOARD_STAGING] {
        let Ok(entries) = std::fs::read_dir(cache.join(kind)) else { continue };
        for e in entries.flatten() {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > STAGING_KEEP);
            if old {
                let p = e.path();
                let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
            }
        }
    }
}

/// Removes a partially written file unless disarmed.
struct PartGuard(Option<PathBuf>);
impl Drop for PartGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Receiver side, step 2: accept the channel and store what arrives.
pub async fn receive(
    mut r: Reader,
    mut w: Writer,
    dest: Destination,
    max_bytes: u64,
    progress: Arc<Progress>,
) -> Result<Received, String> {
    if let Destination::Dir(d) = &dest {
        tokio::fs::create_dir_all(d).await.map_err(|e| format!("{}: {e}", d.display()))?;
    }
    send_ctrl(&mut w, &DataCtrl::Ready { ok: true, error: None }).await?;
    let res = receive_inner(&mut r, &dest, max_bytes, &progress).await;
    match &res {
        Ok(_) => {
            send_ctrl(&mut w, &DataCtrl::Done { ok: true, error: None }).await?;
        }
        Err(e) => {
            let _ = send_ctrl(&mut w, &DataCtrl::Done { ok: false, error: Some(e.clone()) }).await;
        }
    }
    let _ = w.shutdown().await;
    res
}

async fn receive_inner(r: &mut Reader, dest: &Destination, max_bytes: u64, progress: &Progress) -> Result<Received, String> {
    let mut buf = Vec::with_capacity(CHUNK + 64);
    let mut total: u64 = 0;
    // top-level name on the wire -> local unique path
    let mut roots: HashMap<String, PathBuf> = HashMap::new();
    let mut root_order: Vec<PathBuf> = Vec::new();
    let mut memory: Option<Vec<u8>> = None;

    let resolve = |parts: &[String], roots: &mut HashMap<String, PathBuf>, order: &mut Vec<PathBuf>, dir: &Path| -> PathBuf {
        let root = roots
            .entry(parts[0].clone())
            .or_insert_with(|| {
                let taken: Vec<PathBuf> = order.clone();
                let p = unique_path(dir, &parts[0], &|p| taken.iter().any(|t| t == p));
                order.push(p.clone());
                p
            })
            .clone();
        parts[1..].iter().fold(root, |acc, c| acc.join(c))
    };

    loop {
        r.recv_into(&mut buf).await.map_err(|e| format!("data channel: {e}"))?;
        if buf.first() != Some(&TAG_CTRL) {
            return Err("chunk outside of a file".into());
        }
        let ctrl: DataCtrl = postcard::from_bytes(&buf[1..]).map_err(|e| format!("bad frame: {e}"))?;
        match ctrl {
            DataCtrl::End => break,
            DataCtrl::Dir { rel_path } => {
                let Destination::Dir(dir) = dest else { return Err("unexpected directory".into()) };
                let parts = sanitize_rel_path(&rel_path).ok_or_else(|| format!("unsafe path {rel_path:?}"))?;
                let p = resolve(&parts, &mut roots, &mut root_order, dir);
                tokio::fs::create_dir_all(&p).await.map_err(|e| format!("{}: {e}", p.display()))?;
            }
            DataCtrl::FileStart { rel_path, size, mtime } => {
                total = total.saturating_add(size);
                if total > max_bytes {
                    return Err("transfer is larger than announced".into());
                }
                match dest {
                    Destination::Memory => {
                        if memory.is_some() {
                            return Err("more than one blob".into());
                        }
                        let mut v = Vec::with_capacity(size.min(64 << 20) as usize);
                        read_chunks(r, &mut buf, size, progress, |b| {
                            v.extend_from_slice(b);
                            async { Ok(()) }
                        })
                        .await?;
                        memory = Some(v);
                    }
                    Destination::Dir(dir) => {
                        let parts = sanitize_rel_path(&rel_path).ok_or_else(|| format!("unsafe path {rel_path:?}"))?;
                        let final_path = resolve(&parts, &mut roots, &mut root_order, dir);
                        if let Some(parent) = final_path.parent() {
                            tokio::fs::create_dir_all(parent).await.map_err(|e| format!("{}: {e}", parent.display()))?;
                        }
                        let part = PathBuf::from(format!("{}.kvmb-part", final_path.display()));
                        let mut guard = PartGuard(Some(part.clone()));
                        let mut f = File::create(&part).await.map_err(|e| format!("{}: {e}", part.display()))?;
                        let fref = &mut f;
                        read_chunks_into_file(r, &mut buf, size, progress, fref).await?;
                        f.flush().await.map_err(|e| e.to_string())?;
                        let f = f.into_std().await;
                        if let Some(t) = mtime.and_then(|s| u64::try_from(s).ok()) {
                            let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(t);
                            let _ = f.set_times(std::fs::FileTimes::new().set_modified(t));
                        }
                        drop(f);
                        // If something appeared at the target meanwhile, pick a new name.
                        let target = if final_path.exists() {
                            let parent = final_path.parent().unwrap_or(dir);
                            let name = final_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            unique_path(parent, &name, &|_| false)
                        } else {
                            final_path
                        };
                        tokio::fs::rename(&part, &target).await.map_err(|e| format!("{}: {e}", target.display()))?;
                        guard.0 = None;
                    }
                }
                // Expect FileEnd.
                r.recv_into(&mut buf).await.map_err(|e| format!("data channel: {e}"))?;
                let end: Option<DataCtrl> =
                    if buf.first() == Some(&TAG_CTRL) { postcard::from_bytes(&buf[1..]).ok() } else { None };
                if end != Some(DataCtrl::FileEnd) {
                    return Err("missing end of file marker".into());
                }
            }
            other => return Err(format!("unexpected frame {other:?}")),
        }
    }
    Ok(match dest {
        Destination::Memory => Received::Bytes(memory.unwrap_or_default()),
        Destination::Dir(_) => Received::Files(root_order),
    })
}

async fn read_chunks<F, Fut>(r: &mut Reader, buf: &mut Vec<u8>, size: u64, progress: &Progress, mut sink: F) -> Result<(), String>
where
    F: FnMut(&[u8]) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let mut left = size;
    while left > 0 {
        r.recv_into(buf).await.map_err(|e| format!("data channel: {e}"))?;
        if buf.first() != Some(&TAG_CHUNK) {
            return Err("file ended early".into());
        }
        let n = (buf.len() - 1) as u64;
        if n > left {
            return Err("file is longer than announced".into());
        }
        sink(&buf[1..]).await?;
        left -= n;
        progress.done.fetch_add(n, Ordering::Relaxed);
    }
    Ok(())
}

async fn read_chunks_into_file(r: &mut Reader, buf: &mut Vec<u8>, size: u64, progress: &Progress, f: &mut File) -> Result<(), String> {
    let mut left = size;
    while left > 0 {
        r.recv_into(buf).await.map_err(|e| format!("data channel: {e}"))?;
        if buf.first() != Some(&TAG_CHUNK) {
            return Err("file ended early".into());
        }
        let n = (buf.len() - 1) as u64;
        if n > left {
            return Err("file is longer than announced".into());
        }
        f.write_all(&buf[1..]).await.map_err(|e| format!("write failed: {e}"))?;
        left -= n;
        progress.done.fetch_add(n, Ordering::Relaxed);
    }
    Ok(())
}

/// `file://` URI list (RFC 2483) for the given paths.
pub fn uri_list(paths: &[PathBuf]) -> String {
    let mut s = String::new();
    for p in paths {
        s.push_str(&path_to_uri(p));
        s.push_str("\r\n");
    }
    s
}

pub fn path_to_uri(p: &Path) -> String {
    let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(p) };
    let mut s = String::from("file://");
    let text = abs.to_string_lossy().replace('\\', "/");
    if !text.starts_with('/') {
        s.push('/');
    }
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => s.push(b as char),
            _ => s.push_str(&format!("%{b:02X}")),
        }
    }
    s
}

/// Parse a `text/uri-list` (or GNOME's `x-special/gnome-copied-files`) into local paths.
pub fn parse_uri_list(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        .map(|rest| {
            // Strip an optional host part ("file://host/path").
            let path = match rest.find('/') {
                Some(0) => rest,
                Some(i) => &rest[i..],
                None => rest,
            };
            let bytes = percent_decode(path);
            let s = String::from_utf8_lossy(&bytes).into_owned();
            #[cfg(windows)]
            let s = s.trim_start_matches('/').replace('/', "\\");
            PathBuf::from(s)
        })
        .collect()
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}
