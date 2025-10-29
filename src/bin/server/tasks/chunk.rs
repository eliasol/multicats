use std::{
    collections::BTreeSet,
    io::SeekFrom,
    net::{IpAddr, SocketAddr, SocketAddrV4, SocketAddrV6},
    ops::Bound,
    sync::Arc,
    time::Duration,
};

use anyhow::{Result, ensure};
use log::{info, warn};
use multicats::{ChunkData, ChunkRequest, net::new_sender_multicast_socket};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt},
    net::UdpSocket,
    select,
    sync::mpsc::{Receiver, Sender, channel},
    time::{Instant, sleep_until},
    try_join,
};

use crate::ServerState;

async fn request_listener(
    state: &Arc<ServerState>,
    bind: SocketAddr,
    sender: Sender<usize>,
) -> Result<()> {
    let socket = UdpSocket::bind(bind).await?;
    state
        .request_socket
        .set(socket.local_addr()?)
        .expect("Invalid global state (request socket was already set)");

    let mut buf = vec![0u8; 128];

    info!("Listening for chunk requests on {}", socket.local_addr()?);

    loop {
        select! {
            biased;
            _ = state.token.cancelled() => break,
            sz = socket.recv(&mut buf) => {
                let Ok(sz) = sz else { continue };
                let chunk_ids: ChunkRequest = match postcard::from_bytes(&buf[0..sz]) {
                    Ok(x) => x,
                    Err(postcard::Error::DeserializeUnexpectedEnd) => {
                        buf.resize(2 * buf.len(), 0);
                        continue;
                    },
                    _ => continue,
                };
                for chunk_id in chunk_ids {
                    if chunk_id >= state.image.chunks.len() {
                        warn!("Received request for chunk id {} which is invalid", chunk_id);
                    }
                    if sender.send(chunk_id).await.is_err() { break }
                }
            },
        }
    }

    Ok(())
}

async fn chunk_dispatcher(
    state: &Arc<ServerState>,
    bind: SocketAddr,
    mut receiver: Receiver<usize>,
) -> Result<()> {
    let max_fragment_size: usize = {
        let mut l = u16::MIN;
        let mut r = u16::MAX;

        let test_buf = [0u8; u16::MAX as usize];
        let mut test_buf2 = [0u8; u16::MAX as usize];

        while r - l > 1 {
            let m = (r - l) / 2 + l;

            if let Ok(x) = postcard::to_slice(
                &ChunkData {
                    chunk: usize::MAX,
                    offset: usize::MAX,
                    data: &test_buf[0..m as usize],
                },
                &mut test_buf2,
            ) && x.len() <= state.args.max_udp_payload_size as usize
            {
                l = m;
            } else {
                r = m;
            }
        }

        l as usize
    };

    let mut file = File::open(&state.args.file).await?;

    let socket = new_sender_multicast_socket(
        state.args.transfer_socket,
        bind,
        state.interface_id,
        state.args.max_hops,
    )
    .await?;

    let mut queue: BTreeSet<usize> = BTreeSet::new();
    let mut last_id: usize = 0;
    let mut sleep = Instant::now();

    let mut send_buf: Box<[u8]> =
        vec![0u8; state.args.max_udp_payload_size as usize].into_boxed_slice();
    let mut chunk_buf: Box<[u8]> = vec![0u8; state.args.chunk_size].into_boxed_slice();

    loop {
        while let Ok(x) = receiver.try_recv() {
            queue.insert(x);
        }

        let next = queue
            .range((Bound::Excluded(last_id), Bound::Unbounded))
            .next()
            .or_else(|| queue.first())
            .copied();

        let next = match next {
            Some(x) => {
                queue.remove(&x);
                x
            }
            None => {
                select! {
                    biased;
                    _ = state.token.cancelled() => break,
                    x = receiver.recv() => if let Some(x) = x { sleep = Instant::now(); x } else { break },
                }
            }
        };

        last_id = next;

        let chunk = &state.image.chunks[next];
        file.seek(SeekFrom::Start(chunk.offset)).await?;

        let mut count: usize = 0;

        while count < chunk.size {
            let bytes_read = file.read(&mut chunk_buf[count..chunk.size]).await?;
            ensure!(
                bytes_read != 0,
                "Invalid global state (chunk extends beyond file boundaries)"
            );
            count += bytes_read;
        }

        let mut count: usize = 0;
        while count < chunk.size {
            let frag_size = (chunk.size - count).min(max_fragment_size);
            let data = ChunkData {
                chunk: next,
                offset: count,
                data: &chunk_buf[count..count + frag_size],
            };
            let send = postcard::to_slice(&data, &mut send_buf)?;

            sleep_until(sleep).await;
            let sent = socket.send(send).await?;
            ensure!(sent == send.len(), "Failed to send chunk fragment");
            sleep += 8 * sent as u32 * Duration::from_secs(1) / state.args.flood_speed;
            count += frag_size;
        }
    }

    Ok(())
}

pub async fn chunk_request_server(state: Arc<ServerState>) -> Result<()> {
    let bind_address = match state.unicast {
        IpAddr::V4(ip) => SocketAddr::V4(SocketAddrV4::new(ip, 0)),
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
    };

    let (sx, rx) = channel::<usize>(256);

    try_join!(
        request_listener(&state, bind_address, sx),
        chunk_dispatcher(&state, bind_address, rx),
    )?;

    Ok(())
}
