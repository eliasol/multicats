use std::{
    collections::{BTreeMap, BTreeSet},
    io::SeekFrom,
    net::{IpAddr, SocketAddr, SocketAddrV4, SocketAddrV6},
    sync::Arc,
    time::Duration,
};

use anyhow::{Error, Result, bail};
use log::{info, warn};
use multicats::{
    Capacity, ChunkData, ChunkRequest, ImageMetadata, ServerDiscovery,
    net::new_receiver_multicast_socket,
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    net::{TcpStream, UdpSocket},
    select,
    sync::mpsc::{Receiver, Sender, channel},
    time::{Instant, sleep, sleep_until},
    try_join,
};
use twox_hash::XxHash3_64;

use crate::{ClientState, chunk::ChunkAssembler};

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

        if let Ok(mut server) = postcard::from_bytes::<ServerDiscovery>(&buf[0..size])
            && server.metadata_socket.is_ipv6() == state.unicast.is_ipv6()
            && server.request_socket.is_ipv6() == state.unicast.is_ipv6()
            && server.transfer_socket.is_ipv6() == state.unicast.is_ipv6()
        {
            info!(
                "Discovered server for transfer on socket {}",
                server.transfer_socket
            );

            for socket in [
                &mut server.metadata_socket,
                &mut server.transfer_socket,
                &mut server.request_socket,
            ] {
                if let SocketAddr::V6(socket) = socket
                    && socket.ip().is_unicast_link_local()
                {
                    socket.set_scope_id(state.interface.index);
                }
            }

            state
                .server
                .set(server)
                .expect("Invalid global state (server was already discovered).");
            return Ok(());
        }
    }
}

pub async fn metadata_transfer(state: Arc<ClientState>) -> Result<()> {
    let token = state.token.clone();

    let server = select! {
        biased;
        _ = token.cancelled() => { return Ok(()); },
        x = state.server.wait() => x,
    };

    info!(
        "Connecting to image metadata server at {}",
        server.metadata_socket
    );
    let mut socket = TcpStream::connect(server.metadata_socket).await?;

    info!("Retrieving image metadata from server");

    loop {
        let mut buf = Vec::<u8>::new();

        select! {
            biased;
            _ = token.cancelled() => { return Ok(()); },
            x = socket.read_to_end(&mut buf) => x?,
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

async fn chunk_receiver(state: &Arc<ClientState>, to_disk: Sender<(u64, Vec<u8>)>) -> Result<()> {
    let server = state.server.wait().await;
    let socket = new_receiver_multicast_socket(server.transfer_socket, state.interface_id).await?;

    let req_socket = UdpSocket::bind(match state.unicast {
        IpAddr::V6(ip) => SocketAddr::V6(SocketAddrV6::new(
            ip,
            0,
            0,
            if ip.is_unicast_link_local() {
                state.interface.index
            } else {
                0
            },
        )),
        IpAddr::V4(ip) => SocketAddr::V4(SocketAddrV4::new(ip, 0)),
    })
    .await?;

    req_socket.connect(server.request_socket).await?;

    let mut assemblers = BTreeMap::<usize, ChunkAssembler>::new();
    let mut missing = BTreeSet::<usize>::new();
    (0..state.image.wait().await.chunks.len()).for_each(|i| {
        missing.insert(i);
    });
    let mut buf = vec![0u8; 2500 - 40 - 8];

    while !missing.is_empty() {
        let read_size = select! {
            biased;
            _ = state.token.cancelled() => break,
            _ = sleep(Duration::from_millis(100)) => {
                let req: ChunkRequest = missing.iter().take(ChunkRequest::CAPACITY).copied().collect();
                req_socket.send(postcard::to_slice(&req, &mut buf)?).await?;
                continue;
            }
            x = socket.recv(&mut buf) => x?,
        };

        let fragment = match postcard::from_bytes::<ChunkData>(&buf[0..read_size]) {
            Ok(x) => x,
            Err(postcard::Error::DeserializeUnexpectedEnd) => {
                buf.resize(2 * buf.len(), 0);
                continue;
            }
            _ => continue,
        };

        let chunk = &state.image.get().unwrap().chunks[fragment.chunk];

        if !assemblers.contains_key(&fragment.chunk) {
            while assemblers.len() > 40 {
                assemblers.pop_first();
            }

            assemblers.insert(fragment.chunk, ChunkAssembler::new(chunk.size));
        }

        let assembler = assemblers.get_mut(&fragment.chunk).unwrap();

        if assembler
            .add_fragment(fragment.offset, fragment.data)
            .is_err()
        {
            continue;
        }

        if assembler.is_complete() {
            let assembler = assemblers.remove(&fragment.chunk).unwrap();
            let chunk_data = assembler.complete();

            if XxHash3_64::oneshot(&chunk_data) != chunk.hash {
                warn!("Corrupted chunk (hash doesn't match), discarding");
                continue;
            }

            if to_disk.send((chunk.offset, chunk_data)).await.is_err() {
                break;
            }

            missing.remove(&fragment.chunk);
        }
    }

    Ok(())
}

async fn disk_writer(
    state: &Arc<ClientState>,
    mut from_net: Receiver<(u64, Vec<u8>)>,
) -> Result<()> {
    let mut file = File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&state.args.file)
        .await?;
    let Some(image_size) = state
        .image
        .wait()
        .await
        .chunks
        .iter()
        .map(|chunk| chunk.offset + chunk.size as u64)
        .max()
    else {
        return Ok(());
    };

    file.set_len(image_size).await.unwrap_or_else(|e| {
        warn!("Unable to resize file ({})", e);
    });

    if file.metadata().await?.len() < image_size {
        bail!("File too small to fit image");
    }

    let mut last_count: u64 = 0;
    let mut count: u64 = 0;
    let mut time = Instant::now();

    loop {
        let (offset, data) = select! {
            biased;
            _ = state.token.cancelled() => { return Ok(()) },
            _ = sleep_until(time + Duration::from_secs(1)) => {
                let now = Instant::now();
                info!(
                    "Receiving image... {} bytes left ({} Mb/s)",
                    image_size - count,
                    (count - last_count) as f32 / (1024f32 * 128f32) / (now - time).as_secs_f32()
                );
                time = now;
                last_count = count;
                continue;
            },
            x = from_net.recv() => if let Some(x) = x { x } else { break },
        };
        file.seek(SeekFrom::Start(offset)).await?;
        let mut written: usize = 0;
        while written < data.len() {
            let x = file.write(&data[written..]).await?;
            if x == 0 {
                bail!("Failed to write chunk to disk");
            }
            written += x;
        }
        count += written as u64;
    }

    Ok(())
}

pub async fn chunk_transfer(state: Arc<ClientState>) -> Result<()> {
    let (sx, rx) = channel(128);

    try_join!(chunk_receiver(&state, sx), disk_writer(&state, rx),)?;

    Ok(())
}
