use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    str::FromStr,
    sync::Arc,
};

use anyhow::{Error, Result};
use clap::Parser;
use env_logger::Env;
use log::info;
use multicats::{
    ServerDiscovery,
    net::{NetworkInterface, get_interface, new_receiver_multicast_socket},
};
use socket2::InterfaceIndexOrAddress;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
struct ClientArgs {
    #[clap(long, default_value_t = SocketAddr::from_str("[ff18::1]:7890").unwrap())]
    discovery_socket: SocketAddr,
    #[clap(long, short = 'f')]
    force: bool,
    #[clap(long)]
    interface: Option<String>,
    #[clap(long)]
    unicast_address: Option<IpAddr>,
    #[clap(long, default_value_t = 1)]
    hops: u32,
    file: PathBuf,
}

struct ClientState {
    token: CancellationToken,
    interface: NetworkInterface,
    interface_id: InterfaceIndexOrAddress,
    unicast: IpAddr,
    args: ClientArgs,
    server: ServerDiscovery,
}

pub async fn server_discovery(
    discovery_socket: SocketAddr,
    interface: InterfaceIndexOrAddress,
) -> Result<ServerDiscovery> {
    let socket = new_receiver_multicast_socket(discovery_socket, interface).await?;

    let mut buf = [0u8; size_of::<ServerDiscovery>()];

    loop {
        let size = socket.recv(&mut buf).await?;
        if let Ok(server) = postcard::from_bytes::<ServerDiscovery>(&buf[0..size]) {
            info!(
                "Discovered server at {} on socket {}",
                server.request_socket, server.transfer_socket
            );
            return Ok(server);
        }
    }
}

async fn args_to_state(args: ClientArgs) -> Result<ClientState> {
    if !args.discovery_socket.ip().is_multicast() {
        return Err(Error::msg("Discovery address must be a multicast group."));
    }

    let Some(interface) = get_interface(args.interface.as_deref())? else {
        return Err(Error::msg("Cannot find requested interface."));
    };

    let interface_id = if args.discovery_socket.is_ipv6() {
        InterfaceIndexOrAddress::Index(interface.index)
    } else {
        let address = interface
            .ips
            .iter()
            .filter_map(|ip| match ip {
                IpAddr::V4(ip) => Some(ip),
                _ => None,
            })
            .next();
        if let Some(&address) = address {
            InterfaceIndexOrAddress::Address(address)
        } else {
            return Err(Error::msg(
                "In IPv4 mode the selected interface needs to have at least one IPv4 address assigned to it.",
            ));
        }
    };

    let unicast = match args.unicast_address {
        Some(ip) => {
            if ip.is_ipv6() != args.discovery_socket.is_ipv6() {
                return Err(Error::msg(
                    "Unicast address must be of the same family as the multicast groups.",
                ));
            }
            ip
        }
        None => {
            let Some(&x) = interface
                .ips
                .iter()
                .find(|&ip| ip.is_ipv6() == args.discovery_socket.is_ipv6())
            else {
                return Err(Error::msg(
                    "Cannot find any suitable unicast address on the selected interface.",
                ));
            };
            x
        }
    };

    let server = server_discovery(args.discovery_socket, interface_id).await?;

    Ok(ClientState {
        token: CancellationToken::new(),
        unicast,
        interface_id,
        interface,
        args,
        server,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let args = ClientArgs::parse();

    let state = Arc::new(args_to_state(args).await?);

    Ok(())
}
