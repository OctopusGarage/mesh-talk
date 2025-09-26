use crate::domain::models::PeerInfo;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct Node {
    pub name: String,
    pub port: u16,
    pub peers: Arc<Mutex<HashMap<SocketAddr, mpsc::Sender<String>>>>,
    pub peer_info: Arc<Mutex<HashMap<SocketAddr, PeerInfo>>>,
}
