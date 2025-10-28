use std::sync::Arc;

use anyhow::{Error, Result};
use log::info;
use multicats::{ImageMetadata, ServerDiscovery, net::new_receiver_multicast_socket};
use tokio::{io::AsyncReadExt, net::TcpStream, select};

use crate::ClientState;

pub async fn spawn<T, R>(future: T) -> Result<R>
where
    T: Future<Output = Result<R>> + Send + 'static,
    R: Send + 'static,
{
    tokio::spawn(future).await?
}

pub async fn server_discovery(state: Arc<ClientState>) -> Result<()> {
    let socket =
        new_receiver_multicast_socket(state.args.discovery_socket, state.interface_id).await?;

    let token = state.token.clone();
    let mut buf = [0u8; size_of::<ServerDiscovery>()];

    info!(
        "Listening for server discovery on interface {} on group {}",
        state.interface.name, state.args.discovery_socket
    );

    loop {
        let size = select! {
            biased;
            _ = token.cancelled() => { return Err(Error::msg("Server discovery cancelled")); },
            size = socket.recv(&mut buf) => size?,
        };

        if let Ok(server) = postcard::from_bytes::<ServerDiscovery>(&buf[0..size])
            && server.metadata_socket.is_ipv6() == state.unicast.is_ipv6()
            && server.request_socket.is_ipv6() == state.unicast.is_ipv6()
            && server.transfer_socket.is_ipv6() == state.unicast.is_ipv6()
        {
            info!(
                "Discovered server for transfer on socket {}",
                server.transfer_socket
            );
            state
                .server
                .set(server)
                .expect("Invalid global state (server was already discovered).");
            return Ok(());
        }
    }
}

pub async fn metadata_transfer(state: Arc<ClientState>) -> Result<()> {
    let cancelled_error: Result<()> = Err(Error::msg("Metadata transfer cancelled"));
    let token = state.token.clone();

    let server = select! {
        x = state.server.wait() => x,
        _ = token.cancelled() => { return cancelled_error; },
    };

    info!(
        "Connecting to image metadata server at {}",
        server.metadata_socket
    );
    let mut socket = select! {
        x = TcpStream::connect(server.metadata_socket) => x?,
        _ = token.cancelled() => { return cancelled_error; },
    };

    info!("Retrieving image metadata from server");

    loop {
        let mut buf = Vec::<u8>::new();

        select! {
            x = socket.read_to_end(&mut buf) => x?,
            _ = token.cancelled() => { return cancelled_error; },
        };

        if let Ok(metadata) = postcard::from_bytes::<ImageMetadata>(&buf) {
            let image_size: u64 = metadata.chunks.iter().map(|chunk| chunk.size as u64).sum();
            info!(
                "Received metadata for an image of size {} bytes subdivided into {} chunks",
                image_size,
                metadata.chunks.len()
            );
            state
                .image
                .set(metadata)
                .expect("Invalid global state (image was already retrieved)");
            return Ok(());
        }
    }
}
