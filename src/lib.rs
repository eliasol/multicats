pub mod net;

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ServerDiscovery {
    pub request_socket: SocketAddr,
    pub transfer_socket: SocketAddr,
}
