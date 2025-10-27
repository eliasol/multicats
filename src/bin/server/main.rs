mod image;
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
    ImageMetadata,
    net::{NetworkInterface, get_interface},
};
use socket2::InterfaceIndexOrAddress;
use tokio::{sync::SetOnce, try_join};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
struct ServerArgs {
    file: PathBuf,
    #[clap(long, default_value_t = SocketAddr::from_str("[ff18::1]:7890").unwrap())]
    discovery_socket: SocketAddr,
    #[clap(long, default_value_t = SocketAddr::from_str("[ff18::2]:7891").unwrap())]
    transfer_socket: SocketAddr,
    #[clap(long)]
    unicast_address: Option<IpAddr>,
    #[clap(long)]
    interface: Option<String>,
    #[clap(long, default_value_t = 1)]
    max_hops: u32,
    #[clap(long, default_value_t = 1000)]
    discovery_interval: u64,
    #[clap(long, default_value_t = 5 * 1024 * 1024)]
    chunk_size: usize,
}

struct ServerState {
    token: CancellationToken,
    unicast: IpAddr,
    interface: NetworkInterface,
    interface_id: InterfaceIndexOrAddress,
    metadata_socket: SetOnce<SocketAddr>,
    request_socket: SetOnce<SocketAddr>,
    image: ImageMetadata,
    args: ServerArgs,
}

fn args_to_state(args: ServerArgs) -> Result<ServerState> {
    if args.discovery_socket.is_ipv6() != args.transfer_socket.is_ipv6() {
        return Err(Error::msg(
            "Discovery and transfer sockets must be of the same family.",
        ));
    }

    if !args.discovery_socket.ip().is_multicast() || !args.transfer_socket.ip().is_multicast() {
        return Err(Error::msg(
            "Discovery and transfer addresses must be multicast groups.",
        ));
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

    Ok(ServerState {
        token: CancellationToken::new(),
        unicast,
        interface_id,
        interface,
        metadata_socket: SetOnce::new(),
        request_socket: SetOnce::new(),
        image: image::compute_image_metadata(&args.file, args.chunk_size)?,
        args,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(Env::default().default_filter_or("info")).init();

    let args = ServerArgs::parse();
    let state = Arc::new(args_to_state(args)?);

    let discovery_task = tasks::spawn(tasks::server_discovery(state.clone()));
    let metadata_task = tasks::spawn(tasks::metadata_server(state.clone()));

    try_join!(discovery_task, metadata_task)?;

    Ok(())
}
