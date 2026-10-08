//! Finding other KVM Bridge devices on the local subnet.
//!
//! Devices find each other with small UDP beacons sent to the broadcast
//! address of each IPv4 interface: one announcement when a device starts, and
//! probes when the user searches (answered directly by every device that hears
//! them). Nothing is sent in between. Beacons only advertise; all trust
//! decisions happen later in the authenticated handshake.

use crate::types::DeviceId;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

const BEACON_MAGIC: &[u8; 6] = b"KVMB-D";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Beacon {
    pub protocol: u32,
    pub id: DeviceId,
    pub name: String,
    pub os: String,
    /// TCP port of the control listener.
    pub port: u16,
    /// Fingerprint of the device's public key (for display only).
    pub fingerprint: String,
    /// Random per process start, lets peers notice restarts.
    pub instance: u64,
    /// Asks receivers to answer immediately with a unicast beacon.
    pub probe: bool,
}

impl Beacon {
    pub fn encode(&self) -> Vec<u8> {
        let mut v = BEACON_MAGIC.to_vec();
        v.extend(postcard::to_stdvec(self).expect("serialisable"));
        v
    }
    pub fn decode(b: &[u8]) -> Option<Beacon> {
        if b.len() < BEACON_MAGIC.len() || &b[..BEACON_MAGIC.len()] != BEACON_MAGIC {
            return None;
        }
        postcard::from_bytes(&b[BEACON_MAGIC.len()..]).ok()
    }
}

#[derive(Clone, Debug)]
pub struct DiscoveryConfig {
    pub port: u16,
    /// Announce this device and answer searches.
    pub enabled: bool,
    /// Send to interface broadcast addresses (disable in tests).
    pub broadcast: bool,
    /// Additional unicast targets (e.g. manually configured peers).
    pub extra_targets: Vec<SocketAddr>,
}

#[derive(Clone, Debug)]
pub struct Seen {
    pub beacon: Beacon,
    pub from: SocketAddr,
}

/// Broadcast address of every non-loopback IPv4 interface.
pub fn broadcast_targets(port: u16) -> Vec<SocketAddr> {
    let mut out: Vec<SocketAddr> = Vec::new();
    let mut seen = HashSet::new();
    if let Ok(ifs) = if_addrs::get_if_addrs() {
        for i in ifs {
            if i.is_loopback() {
                continue;
            }
            if let if_addrs::IfAddr::V4(v4) = &i.addr {
                let bcast = v4.broadcast.unwrap_or_else(|| {
                    let ip = u32::from(v4.ip);
                    let mask = u32::from(v4.netmask);
                    Ipv4Addr::from(ip | !mask)
                });
                if seen.insert(bcast) {
                    out.push(SocketAddr::V4(SocketAddrV4::new(bcast, port)));
                }
            }
        }
    }
    let limited = Ipv4Addr::BROADCAST;
    if seen.insert(limited) {
        out.push(SocketAddr::V4(SocketAddrV4::new(limited, port)));
    }
    out
}

/// Local IPv4 addresses (for display in the UI).
pub fn local_addresses() -> Vec<IpAddr> {
    if_addrs::get_if_addrs()
        .map(|v| v.into_iter().filter(|i| !i.is_loopback()).map(|i| i.ip()).filter(|ip| ip.is_ipv4()).collect())
        .unwrap_or_default()
}

fn bind_socket(port: u16) -> std::io::Result<std::net::UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(all(unix, not(target_os = "solaris"), not(target_os = "illumos")))]
    s.set_reuse_port(true)?;
    s.set_broadcast(true)?;
    s.set_nonblocking(true)?;
    s.bind(&SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)).into())?;
    Ok(s.into())
}

/// When a search sends its probes, relative to the click. Spread out so a
/// device that misses one (Wi-Fi drops broadcasts) still answers another.
const SEARCH_PROBES_MS: [u64; 4] = [0, 400, 1200, 2500];

/// Run discovery until a channel closes. Every beacon received from another
/// device is forwarded on `out`.
///
/// Nothing is sent periodically. This device announces itself once when it
/// starts (so paired devices learn a changed address) and when its name or
/// port changes; each message on `searches` sends a few probes, which other
/// devices answer directly.
pub async fn run(
    mut cfg: watch::Receiver<DiscoveryConfig>,
    mut beacon: watch::Receiver<Beacon>,
    mut searches: mpsc::UnboundedReceiver<()>,
    out: mpsc::Sender<Seen>,
) {
    let me = beacon.borrow().id;
    loop {
        let port = cfg.borrow().port;
        let sock = match bind_socket(port).and_then(UdpSocket::from_std) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("discovery: cannot bind UDP port {port}: {e}; retrying in 5s");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => continue,
                    r = cfg.changed() => { if r.is_err() { return; } continue; }
                }
            }
        };
        tracing::info!("discovery listening on UDP {port}");
        let mut buf = vec![0u8; 2048];
        // Beacons still to send: when, and whether it is a search probe.
        let mut queue: Vec<(Instant, bool)> = vec![(Instant::now(), false)];
        loop {
            let enabled = cfg.borrow().enabled;
            let next = queue.iter().map(|(t, _)| *t).min();
            tokio::select! {
                _ = tokio::time::sleep_until(next.unwrap_or_else(Instant::now)), if next.is_some() => {
                    let now = Instant::now();
                    let probe = queue.iter().any(|(t, p)| *t <= now && *p);
                    queue.retain(|(t, _)| *t > now);
                    // Announcing can be turned off; searching was asked for.
                    if enabled || probe {
                        let mut b = beacon.borrow().clone();
                        b.probe = probe;
                        let c = cfg.borrow().clone();
                        send_beacon(&sock, &b, &c).await;
                    }
                }
                s = searches.recv() => {
                    if s.is_none() { return; }
                    let now = Instant::now();
                    queue.retain(|(_, p)| !*p);
                    queue.extend(SEARCH_PROBES_MS.iter().map(|ms| (now + Duration::from_millis(*ms), true)));
                }
                r = sock.recv_from(&mut buf) => {
                    let Ok((n, from)) = r else { continue };
                    let Some(b) = Beacon::decode(&buf[..n]) else { continue };
                    if b.id == me { continue; }
                    if b.probe && enabled {
                        let mut mine = beacon.borrow().clone();
                        mine.probe = false;
                        let _ = sock.send_to(&mine.encode(), from).await;
                    }
                    if out.send(Seen { beacon: b, from }).await.is_err() {
                        return;
                    }
                }
                r = cfg.changed() => {
                    if r.is_err() { return; }
                    if cfg.borrow().port != port { break; }
                }
                r = beacon.changed() => {
                    if r.is_err() { return; }
                    // Name/port change: announce right away.
                    queue.push((Instant::now(), false));
                }
            }
        }
    }
}

async fn send_beacon(sock: &UdpSocket, b: &Beacon, cfg: &DiscoveryConfig) {
    let payload = b.encode();
    let bcast = if cfg.broadcast { broadcast_targets(cfg.port) } else { vec![] };
    for t in bcast.into_iter().chain(cfg.extra_targets.iter().copied()) {
        if let Err(e) = sock.send_to(&payload, t).await {
            tracing::trace!("discovery send to {t} failed: {e}");
        }
    }
}
