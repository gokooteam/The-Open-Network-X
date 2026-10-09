//! Test-only in-memory datagram network implementing [`SyncTransport`].
//!
//! No UDP involved (the sandbox blocks it). This is the transport the
//! sync-protocol tests and the onxd follower tests drive; production uses
//! [`AdnlSyncTransport`](crate::sync::AdnlSyncTransport) or
//! [`SharedAdnlTransport`](crate::sync::SharedAdnlTransport).

use crate::block_sync::SyncError;
use crate::sync::{SyncPeer, SyncTransport};
use onx_primitives::Uint256;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, Mutex as AsyncMutex};

/// In-memory datagram network: address -> inbox.
type InboxMap = HashMap<Uint256, mpsc::UnboundedSender<(Uint256, Vec<u8>)>>;

#[derive(Clone, Default)]
struct LoopbackNetwork {
    inboxes: Arc<Mutex<InboxMap>>,
}

/// [`SyncTransport`] over the loopback network. Fully deterministic;
/// no UDP involved.
pub struct LoopbackTransport {
    network: LoopbackNetwork,
    address: Uint256,
    inbox: AsyncMutex<mpsc::UnboundedReceiver<(Uint256, Vec<u8>)>>,
}

impl LoopbackTransport {
    /// Build a connected pair: `pair.0` sends as `addr_a`, `pair.1` as
    /// `addr_b`. Datagrams sent to any other address fail.
    pub fn pair(addr_a: Uint256, addr_b: Uint256) -> (Self, Self) {
        let network = LoopbackNetwork::default();
        let (tx_a, rx_a) = mpsc::unbounded_channel();
        let (tx_b, rx_b) = mpsc::unbounded_channel();
        network.inboxes.lock().unwrap().insert(addr_a, tx_a);
        network.inboxes.lock().unwrap().insert(addr_b, tx_b);
        (
            Self {
                network: network.clone(),
                address: addr_a,
                inbox: AsyncMutex::new(rx_a),
            },
            Self {
                network,
                address: addr_b,
                inbox: AsyncMutex::new(rx_b),
            },
        )
    }

    /// This endpoint's abstract address.
    pub fn address(&self) -> Uint256 {
        self.address
    }

    /// Test hook: deliver a datagram straight into this endpoint's inbox,
    /// as if it arrived from `sender`. Used to simulate senders the
    /// network would never route (strangers, spoofed addresses).
    pub fn inject(&self, sender: Uint256, bytes: Vec<u8>) {
        let inboxes = self.network.inboxes.lock().unwrap();
        inboxes
            .get(&self.address)
            .expect("loopback: no inbox for this endpoint")
            .send((sender, bytes))
            .expect("loopback: inbox closed");
    }
}

/// Adversarial-server helpers: a rogue peer that answers block requests
/// with attacker-chosen bytes over valid RLDP framing. The bytes are NOT
/// validated here — that is the follower's job.
pub mod rogue {
    use super::LoopbackTransport;
    use crate::block_sync::encode_response;
    use crate::block_sync::{decode_request, BlockResponse};
    use crate::rldp::{RldpConfig, RldpSender};
    use crate::sync::{response_transfer_id, SyncPeer, SyncTransport};

    fn rldp_config() -> RldpConfig {
        RldpConfig {
            redundancy: 1,
            // Tiny chunks: even a 1KB block file is multi-frame, so
            // serve_partial can cut a transfer mid-flight.
            chunk_size: 64,
            ..RldpConfig::default()
        }
    }

    /// Blast the RLDP frames of one block response at `peer` (no
    /// ack-driven retransmission — the loopback is reliable, and a rogue
    /// doesn't do retransmission rounds).
    async fn blast(
        transport: &LoopbackTransport,
        peer: &SyncPeer,
        seq_no: u32,
        response: &[u8],
        max_frames: usize,
    ) {
        let mut sender = RldpSender::new(response_transfer_id(seq_no), response, rldp_config())
            .expect("rogue: response builds");
        for (i, packet) in sender.next_round().into_iter().enumerate() {
            if i >= max_frames {
                break;
            }
            transport.send_to(peer, packet).await.expect("rogue: send");
        }
    }

    /// Receive one block request on `transport`, then answer `peer` with
    /// `block_bytes` wrapped in a well-formed response for the requested
    /// seqno. The content is attacker-chosen: forged signatures, corrupt
    /// files, wrong-chain blocks — whatever the test supplies.
    pub async fn serve_one(transport: &LoopbackTransport, peer: &SyncPeer, block_bytes: &[u8]) {
        let (_sender, req_bytes) = transport.recv_from().await.expect("rogue: recv request");
        let req = decode_request(&req_bytes).expect("rogue: request decodes");
        let response = encode_response(&BlockResponse {
            seq_no: req.seq_no,
            block_bytes: block_bytes.to_vec(),
        });
        blast(transport, peer, req.seq_no, &response, usize::MAX).await;
    }

    /// Like [`serve_one`], but stop after `max_frames` RLDP frames and go
    /// silent — the peer "disconnects mid-transfer". The client must time
    /// out (not hang, not accept a partial block).
    pub async fn serve_partial(
        transport: &LoopbackTransport,
        peer: &SyncPeer,
        block_bytes: &[u8],
        max_frames: usize,
    ) {
        let (_sender, req_bytes) = transport.recv_from().await.expect("rogue: recv request");
        let req = decode_request(&req_bytes).expect("rogue: request decodes");
        let response = encode_response(&BlockResponse {
            seq_no: req.seq_no,
            block_bytes: block_bytes.to_vec(),
        });
        blast(transport, peer, req.seq_no, &response, max_frames).await;
    }
}

impl SyncTransport for LoopbackTransport {
    fn send_to(
        &self,
        peer: &SyncPeer,
        bytes: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<(), SyncError>> + Send + '_ {
        let peer = *peer;
        async move {
            let inboxes = self.network.inboxes.lock().unwrap();
            let tx = inboxes
                .get(&peer.address)
                .ok_or_else(|| SyncError::Transport("loopback: unknown peer".into()))?;
            tx.send((self.address, bytes))
                .map_err(|_| SyncError::Transport("loopback: inbox closed".into()))
        }
    }

    async fn recv_from(&self) -> Result<(Uint256, Vec<u8>), SyncError> {
        self.inbox
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| SyncError::Transport("loopback: inbox closed".into()))
    }
}
