pub mod net;

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChunkMetadata {
    pub offset: u64,
    pub size: usize,
    pub hash: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ImageMetadata {
    pub chunks: Box<[ChunkMetadata]>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ServerDiscovery {
    pub metadata_socket: SocketAddr,
    pub request_socket: SocketAddr,
    pub transfer_socket: SocketAddr,
}
