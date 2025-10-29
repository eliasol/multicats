pub mod net;

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

pub trait Capacity {
    const CAPACITY: usize;
}

pub type ChunkRequest = heapless::Vec<usize, 40>;

impl<T, const N: usize> Capacity for heapless::Vec<T, N> {
    const CAPACITY: usize = N;
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ChunkData<'a> {
    pub chunk: usize,
    pub offset: usize,
    pub data: &'a [u8],
}

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
