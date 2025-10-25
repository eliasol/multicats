use anyhow::{Error, Result};
use getifaddrs::{Address, Interface, InterfaceFlags, getifaddrs};
use socket2::{InterfaceIndexOrAddress, SockRef};
use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};
use tokio::net::UdpSocket;

#[derive(Clone, Debug)]
pub struct NetworkInterface {
    pub name: String,
    #[cfg(target_os = "windows")]
    pub description: String,
    pub ips: Vec<IpAddr>,
    pub index: u32,
    pub flags: InterfaceFlags,
}

pub fn get_interfaces() -> Result<Vec<NetworkInterface>> {
    let ifaces: Vec<Interface> = getifaddrs()?.collect();
    let mut tmp: BTreeSet<u32> = BTreeSet::new();
    let mut out: Vec<NetworkInterface> = Vec::new();
    for iface in ifaces.iter() {
        let index = match iface.index {
            Some(x) => x,
            None => continue,
        };
        if tmp.contains(&index) {
            continue;
        }
        tmp.insert(index);
        let ips: Vec<IpAddr> = ifaces
            .iter()
            .filter_map(|int| {
                if int.name != iface.name {
                    return None;
                }
                match int.address {
                    Address::V4(ref ip) => Some(IpAddr::V4(ip.address)),
                    Address::V6(ref ip) => Some(IpAddr::V6(ip.address)),
                    _ => None,
                }
            })
            .collect();
        out.push(NetworkInterface {
            name: iface.name.clone(),
            #[cfg(target_os = "windows")]
            description: iface.description.clone(),
            ips,
            index,
            flags: iface.flags,
        });
    }
    Ok(out)
}

pub fn get_interface(name: Option<&str>) -> Result<Option<NetworkInterface>> {
    let ifaces = get_interfaces()?;
    Ok(match name {
        None => ifaces
            .iter()
            .find(|int| {
                int.flags.contains(InterfaceFlags::UP)
                    && !int.flags.contains(InterfaceFlags::LOOPBACK)
                    && int.flags.contains(InterfaceFlags::MULTICAST)
            })
            .cloned(),
        Some(name) => ifaces.iter().find(|&int| int.name == name).cloned(),
    })
}

pub async fn new_sender_multicast_socket(
    group: SocketAddr,
    bind: SocketAddr,
    interface: InterfaceIndexOrAddress,
    hops: u32,
) -> Result<UdpSocket> {
    let socket = UdpSocket::bind(bind).await?;

    socket.connect(group).await?;

    let sockref = SockRef::from(&socket);

    match interface {
        InterfaceIndexOrAddress::Address(_) => {
            if bind.is_ipv6() {
                return Err(Error::msg("Expected index as interface identifier"));
            }
        }
        InterfaceIndexOrAddress::Index(_) => {
            if bind.is_ipv4() {
                return Err(Error::msg("Expected IPv4 address as interface identifier"));
            }
        }
    }

    match interface {
        InterfaceIndexOrAddress::Index(0) => {}
        InterfaceIndexOrAddress::Address(Ipv4Addr::UNSPECIFIED) => {}
        InterfaceIndexOrAddress::Index(x) => sockref.set_multicast_if_v6(x)?,
        InterfaceIndexOrAddress::Address(x) => sockref.set_multicast_if_v4(&x)?,
    }

    match bind {
        SocketAddr::V4(_) => sockref.set_multicast_ttl_v4(hops)?,
        SocketAddr::V6(_) => sockref.set_multicast_hops_v6(hops)?,
    }

    Ok(socket)
}

pub async fn new_receiver_multicast_socket(
    group: SocketAddr,
    interface: InterfaceIndexOrAddress,
) -> Result<UdpSocket> {
    // Windows does not allow binding a socket to a multicast address, so we bind
    // the unspecified address. This has some implications like not being able to
    // bind multiple sockets to the same port in different multicast groups, and
    // also receiving unicast traffic through this socket, but this is the only way
    // I know of dealing with this so it will do for now.
    #[cfg(target_os = "windows")]
    let sockaddr = match group {
        SocketAddr::V4(addr) => {
            use std::net::SocketAddrV4;
            SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, addr.port()))
        }
        SocketAddr::V6(addr) => {
            use std::net::{Ipv6Addr, SocketAddrV6};
            SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, addr.port(), 0, 0))
        }
    };

    #[cfg(not(target_os = "windows"))]
    let sockaddr = group;

    let socket = UdpSocket::bind(sockaddr).await?;

    match group.ip() {
        IpAddr::V4(ipv4) => match interface {
            InterfaceIndexOrAddress::Address(interface) => {
                socket.join_multicast_v4(ipv4, interface)?
            }
            _ => return Err(Error::msg("Expected IPv4 address as interface identifier")),
        },
        IpAddr::V6(ipv6) => match interface {
            InterfaceIndexOrAddress::Index(interface) => {
                socket.join_multicast_v6(&ipv6, interface)?
            }
            _ => return Err(Error::msg("Expected index as interface identifier")),
        },
    }

    Ok(socket)
}
