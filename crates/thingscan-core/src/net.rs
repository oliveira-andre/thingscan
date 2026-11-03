//! Per-interface UDP sockets for multicast queries (SSDP, CoAP/OCF). Each socket is
//! bound to one interface so the query leaves on every LAN the host is attached to;
//! replies come back unicast to that socket.

use anyhow::Result;
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use tokio::net::UdpSocket;
use tokio::time::Instant;

pub struct LanSocket {
    pub socket: UdpSocket,
    /// Interface index for IPv6 scope ids; `None` for IPv4 sockets.
    pub scope: Option<u32>,
    /// IPv4 address and prefix length of the interface; `None` for IPv6 sockets.
    pub v4_net: Option<(Ipv4Addr, u8)>,
}

fn usable(iface: &if_addrs::Interface) -> bool {
    !iface.is_loopback()
        && iface.is_oper_up()
        && !iface.name.starts_with("utun")
        && !iface.name.starts_with("awdl")
        && !iface.name.starts_with("llw")
}

pub fn ipv4_sockets() -> Result<Vec<LanSocket>> {
    let mut out = Vec::new();
    for iface in if_addrs::get_if_addrs()? {
        let if_addrs::IfAddr::V4(v4) = &iface.addr else { continue };
        let ip = v4.ip;
        if !usable(&iface) || ip.is_link_local() {
            continue;
        }
        let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_multicast_if_v4(&ip)?;
        socket.set_multicast_ttl_v4(4)?;
        socket.bind(&SocketAddr::V4(SocketAddrV4::new(ip, 0)).into())?;
        socket.set_nonblocking(true)?;
        out.push(LanSocket {
            socket: UdpSocket::from_std(socket.into())?,
            scope: None,
            v4_net: Some((ip, v4.prefixlen)),
        });
    }
    Ok(out)
}

pub fn ipv6_sockets() -> Result<Vec<LanSocket>> {
    let mut out = Vec::new();
    for iface in if_addrs::get_if_addrs()? {
        let (IpAddr::V6(ip), Some(index)) = (iface.ip(), iface.index) else { continue };
        // One socket per interface, bound to its link-local address.
        if !usable(&iface) || !ip.is_unicast_link_local() || out.iter().any(|s: &LanSocket| s.scope == Some(index)) {
            continue;
        }
        let socket = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
        socket.set_only_v6(true)?;
        socket.set_multicast_if_v6(index)?;
        socket.set_multicast_hops_v6(4)?;
        socket.bind(&SocketAddr::V6(SocketAddrV6::new(ip, 0, 0, index)).into())?;
        socket.set_nonblocking(true)?;
        out.push(LanSocket { socket: UdpSocket::from_std(socket.into())?, scope: Some(index), v4_net: None });
    }
    Ok(out)
}

/// Every other host address on an IPv4 subnet, or nothing if the subnet is larger than /22.
pub fn subnet_hosts(ip: Ipv4Addr, prefix: u8) -> Vec<Ipv4Addr> {
    if !(22..=30).contains(&prefix) {
        return Vec::new();
    }
    let mask = u32::MAX << (32 - prefix);
    let network = u32::from(ip) & mask;
    let broadcast = network | !mask;
    (network + 1..broadcast).map(Ipv4Addr::from).filter(|h| *h != ip).collect()
}

pub fn v6_group(group: Ipv6Addr, port: u16, scope: u32) -> SocketAddr {
    SocketAddr::V6(SocketAddrV6::new(group, port, 0, scope))
}

pub fn v4_group(group: Ipv4Addr, port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(group, port))
}

/// Collect datagrams until `deadline`.
pub async fn collect(socket: &UdpSocket, deadline: Instant) -> Vec<(Vec<u8>, SocketAddr)> {
    let mut buf = vec![0u8; 8192];
    let mut out = Vec::new();
    while let Ok(Ok((n, from))) = tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await {
        out.push((buf[..n].to_vec(), from));
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerates_subnet_hosts() {
        let hosts = subnet_hosts(Ipv4Addr::new(192, 168, 3, 126), 24);
        assert_eq!(hosts.len(), 253);
        assert_eq!(hosts.first(), Some(&Ipv4Addr::new(192, 168, 3, 1)));
        assert_eq!(hosts.last(), Some(&Ipv4Addr::new(192, 168, 3, 254)));
        assert!(!hosts.contains(&Ipv4Addr::new(192, 168, 3, 126)));
        assert!(subnet_hosts(Ipv4Addr::new(10, 0, 0, 5), 16).is_empty());
    }
}
