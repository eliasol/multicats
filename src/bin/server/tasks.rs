use std::{
    net::{IpAddr, SocketAddr, SocketAddrV4, SocketAddrV6},
    sync::Arc,
    time::Duration,
};

use anyhow::Result;
use log::{info, trace};
use multicats::{ServerDiscovery, net::new_sender_multicast_socket};
use tokio::{io::AsyncWriteExt, net::TcpListener, select, task::JoinSet, time::sleep};

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

    let data = postcard::to_allocvec(
        &select! {
            biased;
            _ = token.cancelled() => { return Ok(()); },
            x = async {
                ServerDiscovery {
                    metadata_socket: *state.metadata_socket.wait().await,
                    request_socket: *state.request_socket.wait().await,
                    transfer_socket: state.args.transfer_socket,
                }
            } => x,
        }
    )?;

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

pub async fn metadata_server(state: Arc<ServerState>) -> Result<()> {
    let bind_address = match state.unicast {
        IpAddr::V4(ip) => SocketAddr::V4(SocketAddrV4::new(ip, 0)),
        IpAddr::V6(ip) => SocketAddr::V6(SocketAddrV6::new(ip, 0, 0, 0)),
    };
    let socket = TcpListener::bind(bind_address).await?;
    state
        .metadata_socket
        .set(socket.local_addr()?)
        .expect("Invalid global server state (metadata socket address was already set)");

    let buf = Arc::new(postcard::to_allocvec(&state.image)?);
    let token = state.token.clone();

    let mut clients = JoinSet::<()>::new();

    info!(
        "Listening for metadata transfers on {}",
        state.metadata_socket.get().unwrap()
    );

    loop {
        select! {
            biased;
            _ = token.cancelled() => break,
            _ = clients.join_next(), if !clients.is_empty() => {},
            conn = socket.accept() => if let Ok((mut stream, addr)) = conn {
                trace!("New metadata transfer to {}", addr);
                let buf = buf.clone();
                clients.spawn(async move {
                    let mut pos: usize = 0;
                    while pos < buf.len() {
                        let Ok(written) = stream.write(&buf[pos..]).await else { break; };
                        if written == 0 { break; }
                        pos += written;
                    }
                });
            }
        }
    }

    clients.shutdown().await;

    Ok(())
}
