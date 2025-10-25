use std::{
    net::{IpAddr, SocketAddr, SocketAddrV4, SocketAddrV6},
    sync::Arc,
    time::Duration,
};

use anyhow::Result;
use log::info;
use multicats::{ServerDiscovery, net::new_sender_multicast_socket};
use tokio::{select, time::sleep};

use crate::ServerState;

pub async fn spawn(handle: impl Future<Output = Result<()>> + Send + 'static) -> Result<()> {
    tokio::spawn(handle).await?
}

pub async fn server_discovery(state: Arc<ServerState>) -> Result<()> {
    let bind_address = match state.unicast {
        IpAddr::V4(x) => SocketAddr::V4(SocketAddrV4::new(x, 0)),
        IpAddr::V6(x) => SocketAddr::V6(SocketAddrV6::new(
            x,
            0,
            0,
            if x.is_unicast_link_local() {
                state.interface.index
            } else {
                0
            },
        )),
    };
    let socket = new_sender_multicast_socket(
        state.args.discovery_socket,
        bind_address,
        state.interface_id,
        state.args.max_hops,
    )
    .await?;

    let token = state.token.clone();

    let data = postcard::to_allocvec(&ServerDiscovery {
        request_socket: *state.request_socket.wait().await,
        transfer_socket: state.args.transfer_socket,
    })?;

    info!(
        "Start sending discovery packets every {} milliseconds on interface {} from address {}",
        state.args.discovery_interval, state.interface.name, state.unicast
    );

    loop {
        let _ = socket.send(data.as_slice()).await?;

        select! {
            biased;
            _ = token.cancelled() => return Ok(()),
            _ = sleep(Duration::from_millis(state.args.discovery_interval)) => {},
        }
    }
}
