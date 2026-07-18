//! iroh-backed [`PeerLink`] (P2): direct, identity-addressed P2P with NAT
//! hole-punching + relay fallback, all provided by iroh. We map our `PeerId`
//! (hex ed25519 public key) to an iroh `EndpointId`, dial by id, and carry one
//! message per bi-directional QUIC stream.
//!
//! NOTE: the live networking path can only be validated across real machines
//! on separate networks — there are no offline unit tests here. The transport
//! *logic* is covered by the `LoopbackLink` tests in [`crate::peer`].

use std::collections::HashMap;

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointId, SecretKey};
use tokio::sync::{mpsc, Mutex};

use crate::peer::{PeerError, PeerId, PeerLink};

/// ALPN identifying Hive's envelope-sync protocol.
const ALPN: &[u8] = b"hive/sync/1";
/// Max bytes accepted for a single message (one envelope), 16 MiB.
const MAX_MSG: usize = 16 * 1024 * 1024;

pub struct IrohNode {
    endpoint: Endpoint,
    local: PeerId,
    inbox: Mutex<mpsc::UnboundedReceiver<(PeerId, Vec<u8>)>>,
    conns: Mutex<HashMap<EndpointId, Connection>>,
}

fn peer_id_of(id: &EndpointId) -> PeerId {
    PeerId(id.to_string())
}

/// Build an iroh secret key from raw 32 bytes (a persisted per-install key).
pub fn secret_from_bytes(bytes: [u8; 32]) -> SecretKey {
    SecretKey::from_bytes(&bytes)
}

/// The shareable friend code for a given secret (its public EndpointId),
/// computable without binding a live endpoint.
pub fn code_for_secret(bytes: [u8; 32]) -> String {
    let pk = SecretKey::from_bytes(&bytes).public();
    PeerId(pk.to_string()).to_code()
}

impl IrohNode {
    /// Bind an endpoint with the given ed25519 secret key and start accepting
    /// inbound connections. Uses iroh's N0 preset (discovery + relay fallback).
    pub async fn bind(secret: SecretKey) -> Result<Self, PeerError> {
        let endpoint = Endpoint::builder(iroh::endpoint::presets::N0)
            .secret_key(secret)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .map_err(|e| PeerError::Transport(e.to_string()))?;
        let local = peer_id_of(&endpoint.id());

        let (tx, rx) = mpsc::unbounded_channel();
        let accept_ep = endpoint.clone();
        tokio::spawn(async move {
            loop {
                let Some(incoming) = accept_ep.accept().await else {
                    break; // endpoint closed
                };
                let tx = tx.clone();
                tokio::spawn(async move {
                    let conn = match incoming.await {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    let peer = peer_id_of(&conn.remote_id());
                    // Each inbound bi-stream carries one message.
                    loop {
                        match conn.accept_bi().await {
                            Ok((_send, mut recv)) => match recv.read_to_end(MAX_MSG).await {
                                Ok(data) => {
                                    if tx.send((peer.clone(), data)).is_err() {
                                        return;
                                    }
                                }
                                Err(_) => continue,
                            },
                            Err(_) => break, // connection closed
                        }
                    }
                });
            }
        });

        Ok(Self {
            endpoint,
            local,
            inbox: Mutex::new(rx),
            conns: Mutex::new(HashMap::new()),
        })
    }

    /// A reused (or freshly dialed) connection to `id`.
    async fn connection(&self, id: EndpointId) -> Result<Connection, PeerError> {
        let mut conns = self.conns.lock().await;
        if let Some(c) = conns.get(&id) {
            if c.close_reason().is_none() {
                return Ok(c.clone());
            }
        }
        let conn = self
            .endpoint
            .connect(id, ALPN)
            .await
            .map_err(|e| PeerError::Transport(e.to_string()))?;
        conns.insert(id, conn.clone());
        Ok(conn)
    }
}

impl PeerLink for IrohNode {
    async fn send(&self, to: &PeerId, data: Vec<u8>) -> Result<(), PeerError> {
        let id: EndpointId = to.0.parse().map_err(|_| PeerError::BadPeerId(to.0.clone()))?;
        let conn = self.connection(id).await?;
        let (mut send, _recv) = conn
            .open_bi()
            .await
            .map_err(|e| PeerError::Transport(e.to_string()))?;
        send.write_all(&data).await.map_err(|e| PeerError::Transport(e.to_string()))?;
        send.finish().map_err(|e| PeerError::Transport(e.to_string()))?;
        // Wait for the peer to acknowledge so we don't reset the stream early.
        let _ = send.stopped().await;
        Ok(())
    }

    async fn recv(&self) -> Option<(PeerId, Vec<u8>)> {
        self.inbox.lock().await.recv().await
    }

    fn local_id(&self) -> &PeerId {
        &self.local
    }
}
