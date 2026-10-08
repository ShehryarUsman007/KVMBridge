//! Finding other KVM Bridge devices on the local subnet.
//!
//! Every device periodically broadcasts a small beacon (UDP) to the broadcast
//! address of each IPv4 interface. Beacons only advertise; all trust decisions
//! happen later in the authenticated handshake.

use crate::types::DeviceId;
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};

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
    pub interval: Duration,
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

/// Run discovery until the config channel closes. Every beacon received from
/// another device is forwarded on `out`.
pub async fn run(
    mut cfg: watch::Receiver<DiscoveryConfig>,
    mut beacon: watch::Receiver<Beacon>,
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
        // Probe burst at start so devices show up immediately.
        let mut burst = 3u32;
        let mut buf = vec![0u8; 2048];
        let mut next_send = tokio::time::Instant::now();
        loop {
            let (enabled, interval) = {
                let c = cfg.borrow();
                (c.enabled, c.interval)
            };
            tokio::select! {
                _ = tokio::time::sleep_until(next_send) => {
                    if enabled {
                        let mut b = beacon.borrow().clone();
                        b.probe = burst > 0;
                        let c = cfg.borrow().clone();
                        send_beacon(&sock, &b, &c).await;
                    }
                    let wait = if burst > 0 { burst -= 1; Duration::from_millis(300) } else { interval };
                    next_send = tokio::time::Instant::now() + wait;
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
                    burst = 1;
                    next_send = tokio::time::Instant::now();
                }
                r = beacon.changed() => {
                    if r.is_err() { return; }
                    // Name/port change: announce right away.
                    next_send = tokio::time::Instant::now();
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
