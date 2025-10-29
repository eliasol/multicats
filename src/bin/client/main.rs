mod chunk;
mod tasks;

use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    str::FromStr,
    sync::Arc,
};

use anyhow::{Error, Result};
use clap::Parser;
use env_logger::Env;
use multicats::{
    ImageMetadata, ServerDiscovery,
    net::{NetworkInterface, get_interface},
};
use socket2::InterfaceIndexOrAddress;
use tokio::{sync::SetOnce, try_join};
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
    server: SetOnce<ServerDiscovery>,
    image: SetOnce<ImageMetadata>,
}

fn args_to_state(args: ClientArgs) -> Result<ClientState> {
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

    Ok(ClientState {
        token: CancellationToken::new(),
        unicast,
        interface_id,
        interface,
        args,
        server: SetOnce::new(),
        image: SetOnce::new(),
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let args = ClientArgs::parse();

    let state = Arc::new(args_to_state(args)?);

    let server_discovery = tasks::spawn(tasks::server_discovery(state.clone()));
    let metadata_transfer = tasks::spawn(tasks::metadata_transfer(state.clone()));
    let chunk_transfer = tasks::spawn(tasks::chunk_transfer(state.clone()));

    try_join!(server_discovery, metadata_transfer, chunk_transfer)?;

    Ok(())
}
