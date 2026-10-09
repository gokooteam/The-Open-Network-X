//! M5 sync client and server: fetching block files from a peer over ADNL.
//!
//! Layering: [`block_sync`](crate::block_sync) defines WHAT is carried (the
//! three-message wire protocol; framing integrity only). This module defines
//! HOW the two sides turn-take over datagrams:
//!
//! - The follower's [`SyncClient::fetch_block`] sends one [`BlockRequest`]
//!   datagram, then receives the [`BlockResponse`] as an RLDP transfer and
//!   returns the raw block file bytes. Framing is validated here; block
//!   CONTENT is never trusted at this layer — the onxd follower loop checks
//!   the `ONXBLK05` magic, strict-decodes, matches the announcement hash,
//!   and runs `verify_block_auth` before applying (see the trust-boundary
//!   note in `block_sync`).
//! - The producer's [`SyncServer`] answers requests from its block dir.
//!
//! Design decisions (M5):
//! 1. Responses ALWAYS travel as RLDP transfers, even tiny ones. One code
//!    path handles every block size up to `MAX_BLOCK_FILE_BYTES`; the extra
//!    round trips are irrelevant next to block intervals.
//! 2. Requests are single datagrams (a 4-byte seqno never needs chunking).
//! 3. No NACK message exists in the M5 wire protocol. If the server has no
//!    such block (or it is oversized), it stays silent and the client's
//!    fetch times out; the follower retries. Adding a NACK means a wire
//!    version bump.
//! 4. The follower POLLS (`fetch_block(head + 1)` in a loop). Announcements
//!    exist in the codec for the M6 broadcast overlay; M5 does not need them.
//! 5. The server only answers explicitly configured peers (static peer list,
//!    ADR-0043). Unknown senders are ignored, never rejected with an error.
//! 6. Every received datagram is checked against the expected peer's
//!    abstract address. This is defense in depth — `verify_block_auth`
//!    remains the real gate in onxd.
//! 7. The RLDP transfer id is DERIVED from the seqno
//!    (`response_transfer_id`), not random. Retries of the same block reuse
//!    the id, so a stale frame from an earlier attempt is byte-identical to
//!    a fresh one and harmless; frames from another block's transfer carry a
//!    different id and are skipped. Block bytes are write-once per seqno,
//!    so the derivation is sound.
//! 8. Responses use RLDP redundancy 1: loss is handled by retransmission
//!    rounds, not duplicate emission, so a completed transfer leaves no
//!    duplicate datagrams behind to pollute the next fetch.
//!
//! Transport: [`SyncTransport`] abstracts datagram send/receive so the
//! protocol logic is testable without UDP. [`AdnlSyncTransport`] adapts the
//! real [`AdnlTransportNode`]; tests use an in-memory loopback pair.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use onx_primitives::{PublicKey, Uint256};
use sha2::{Digest, Sha256};
use tokio::time::timeout;

use crate::adnl_transport::AdnlTransportNode;
use crate::block_sync::{decode_response, encode_request, BlockRequest, BlockServer, SyncError};
use crate::rldp::{RldpAckFrame, RldpConfig, RldpDataFrame, RldpReceiver, RldpSender};

/// A sync peer: the ADNL identity and UDP endpoint the transport needs to
/// reach it, plus the abstract address used to authenticate inbound datagrams.
#[derive(Debug, Clone, Copy)]
pub struct SyncPeer {
    /// Peer's ed25519 public key (ADNL channel identity).
    pub public_key: PublicKey,
    /// Peer's UDP endpoint.
    pub endpoint: SocketAddr,
    /// Peer's ADNL abstract address; inbound datagrams must carry it.
    pub address: Uint256,
}

/// Datagram send/receive for the sync protocol.
///
/// The trait exists so the protocol logic is testable without UDP: the
/// production impl ([`AdnlSyncTransport`]) drives the real
/// [`AdnlTransportNode`]; tests use an in-memory loopback pair.
pub trait SyncTransport: Send + Sync {
    /// Send one datagram to a peer.
    fn send_to(
        &self,
        peer: &SyncPeer,
        bytes: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<(), SyncError>> + Send + '_;
    /// Receive one datagram. Returns the sender's abstract address and the
    /// payload. Implementations must not filter by sender; callers check.
    fn recv_from(
        &self,
    ) -> impl std::future::Future<Output = Result<(Uint256, Vec<u8>), SyncError>> + Send + '_;
}

/// [`SyncTransport`] over the real ADNL node.
pub struct AdnlSyncTransport<'a> {
    node: &'a AdnlTransportNode,
}

impl<'a> AdnlSyncTransport<'a> {
    /// Borrow a bound ADNL node for sync traffic.
    pub fn new(node: &'a AdnlTransportNode) -> Self {
        Self { node }
    }
}

impl SyncTransport for AdnlSyncTransport<'_> {
    fn send_to(
        &self,
        peer: &SyncPeer,
        bytes: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<(), SyncError>> + Send + '_ {
        let peer = *peer;
        async move {
            self.node
                .send_datagram(peer.public_key, peer.endpoint, &bytes)
                .await
                .map_err(SyncError::from)
        }
    }

    async fn recv_from(&self) -> Result<(Uint256, Vec<u8>), SyncError> {
        let (addr, payload, _src) = self.node.recv_datagram().await.map_err(SyncError::from)?;
        Ok((addr, payload))
    }
}

/// [`SyncTransport`] over a shared [`AdnlTransportNode`].
///
/// [`AdnlSyncTransport`] borrows the node, so it cannot be moved into a
/// spawned task. This variant owns an `Arc<AdnlTransportNode>` instead —
/// the same datagram semantics, but `'static`, for the daemon's sync
/// server and follower tasks.
pub struct SharedAdnlTransport {
    node: Arc<AdnlTransportNode>,
}

impl SharedAdnlTransport {
    /// Share ownership of a bound ADNL node for sync traffic.
    pub fn new(node: Arc<AdnlTransportNode>) -> Self {
        Self { node }
    }
}

impl SyncTransport for SharedAdnlTransport {
    fn send_to(
        &self,
        peer: &SyncPeer,
        bytes: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<(), SyncError>> + Send + '_ {
        let peer = *peer;
        let node = self.node.clone();
        async move {
            node.send_datagram(peer.public_key, peer.endpoint, &bytes)
                .await
                .map_err(SyncError::from)
        }
    }

    async fn recv_from(&self) -> Result<(Uint256, Vec<u8>), SyncError> {
        let (addr, payload, _src) = self.node.recv_datagram().await.map_err(SyncError::from)?;
        Ok((addr, payload))
    }
}

/// Sync-level tuning knobs.
#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// RLDP framing for response transfers.
    pub rldp: RldpConfig,
    /// Whole-fetch timeout for [`SyncClient::fetch_block`] (request sent to
    /// full response reassembled). Covers retransmission rounds.
    pub fetch_timeout: Duration,
    /// How long the response sender waits for RLDP acks after each round
    /// before retransmitting the unacknowledged frames.
    pub ack_timeout: Duration,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            // Redundancy 1: loss is handled by retransmission rounds (see
            // design decision 8); duplicate emission would leave stale
            // datagrams behind.
            rldp: RldpConfig {
                redundancy: 1,
                ..RldpConfig::default()
            },
            fetch_timeout: Duration::from_secs(30),
            ack_timeout: Duration::from_secs(2),
        }
    }
}

/// RLDP transfer id for a block response, derived from the seqno (design
/// decision 7). Both sides compute it independently; no handshake needed.
pub(crate) fn response_transfer_id(seq_no: u32) -> Uint256 {
    let mut h = Sha256::new();
    h.update(b"ONX_SYNC_RESP_V1");
    h.update(seq_no.to_be_bytes());
    Uint256(h.finalize().into())
}

/// Send `payload` to `peer` as one RLDP transfer, driving retransmission
/// rounds until every frame is acknowledged or the peer goes quiet.
async fn rldp_send<T: SyncTransport>(
    transport: &T,
    peer: &SyncPeer,
    transfer_id: Uint256,
    payload: &[u8],
    config: &RldpConfig,
    ack_timeout: Duration,
) -> Result<(), SyncError> {
    let mut sender = RldpSender::new(transfer_id, payload, *config).map_err(SyncError::from)?;
    for _ in 0..sender.max_rounds() {
        for packet in sender.next_round() {
            transport.send_to(peer, packet).await?;
        }
        if sender.is_complete() {
            return Ok(());
        }
        // Drain acks until the round times out; the receiver de-duplicates,
        // and only datagrams from our peer count.
        while let Ok(recv) = timeout(ack_timeout, transport.recv_from()).await {
            let (sender_addr, packet) = recv?;
            if sender_addr != peer.address {
                continue;
            }
            if let Ok(ack) = RldpAckFrame::decode(&packet) {
                sender.apply_ack(&ack).map_err(SyncError::from)?;
                if sender.is_complete() {
                    return Ok(());
                }
            }
        }
    }
    Err(SyncError::Timeout {
        what: "RLDP transfer acknowledgement",
    })
}

/// Receive one RLDP transfer from `peer`, reassembling frames and checking
/// the payload digest before returning. Stray datagrams (wrong sender,
/// undecodable, or a stale transfer id) are skipped; the overall timeout
/// bounds the wait.
async fn rldp_recv<T: SyncTransport>(
    transport: &T,
    peer: &SyncPeer,
    expected_transfer_id: Uint256,
    overall_timeout: Duration,
) -> Result<Vec<u8>, SyncError> {
    timeout(overall_timeout, async {
        let mut receiver: Option<RldpReceiver> = None;
        loop {
            let (sender_addr, packet) = transport.recv_from().await?;
            if sender_addr != peer.address {
                continue;
            }
            let frame = match RldpDataFrame::decode(&packet) {
                Ok(f) => f,
                Err(_) => continue, // stray non-RLDP datagram
            };
            if frame.transfer_id != expected_transfer_id {
                continue; // stale frame from another block's transfer
            }
            let r = match receiver.as_mut() {
                Some(r) => r,
                None => receiver.insert(RldpReceiver::from_frame(&frame).map_err(SyncError::from)?),
            };
            // Ingest first, then ack: the ack must reflect the frame just
            // received, otherwise the sender never sees progress.
            let accepted = r.ingest(frame).map_err(SyncError::from)?;
            let ack_bytes = r.ack().encode();
            transport.send_to(peer, ack_bytes).await?;
            if let Some(payload) = accepted {
                return Ok(payload);
            }
        }
    })
    .await
    .map_err(|_| SyncError::Timeout {
        what: "block response transfer",
    })?
}

/// Follower side: fetch block files from one peer.
pub struct SyncClient<T: SyncTransport> {
    transport: T,
    peer: SyncPeer,
    config: SyncConfig,
}

impl<T: SyncTransport> SyncClient<T> {
    /// Fetch blocks from `peer` over `transport`.
    pub fn new(transport: T, peer: SyncPeer, config: SyncConfig) -> Self {
        Self {
            transport,
            peer,
            config,
        }
    }

    /// Fetch the raw block file bytes for `seq_no`.
    ///
    /// Sends one [`BlockRequest`] datagram, reassembles the RLDP response,
    /// and strictly validates the response framing (including the seqno
    /// match). Returns [`SyncError::Timeout`] if the peer does not answer —
    /// the caller retries; a missing block is not an error here.
    /// The bytes are NOT content-validated; see the module docs.
    pub async fn fetch_block(&self, seq_no: u32) -> Result<Vec<u8>, SyncError> {
        let req = BlockRequest { seq_no };
        self.transport
            .send_to(&self.peer, encode_request(&req))
            .await?;
        let bytes = rldp_recv(
            &self.transport,
            &self.peer,
            response_transfer_id(seq_no),
            self.config.fetch_timeout,
        )
        .await?;
        decode_response(&bytes, seq_no)
    }
}

/// Producer side: answer block requests from a block dir.
pub struct SyncServer<T: SyncTransport> {
    transport: T,
    /// Address -> peer, for replies. Only configured peers are served.
    peers: HashMap<Uint256, SyncPeer>,
    blocks: BlockServer,
    config: SyncConfig,
}

impl<T: SyncTransport> SyncServer<T> {
    /// Serve `blocks_dir` to `peers` over `transport`.
    pub fn new(
        transport: T,
        peers: Vec<SyncPeer>,
        blocks_dir: PathBuf,
        config: SyncConfig,
    ) -> Self {
        Self {
            transport,
            peers: peers.into_iter().map(|p| (p.address, p)).collect(),
            blocks: BlockServer::new(blocks_dir),
            config,
        }
    }

    /// Serve a single inbound datagram: answer it if it is a well-formed
    /// block request from a known peer for a servable block, ignore it
    /// otherwise. Never fails on peer misbehavior — only on transport
    /// failure of our own socket.
    pub async fn serve_one(&self) -> Result<(), SyncError> {
        let (sender_addr, bytes) = self.transport.recv_from().await?;
        let Some(peer) = self.peers.get(&sender_addr) else {
            return Ok(()); // unknown sender: ignore (design decision 5)
        };
        let req = match crate::block_sync::decode_request(&bytes) {
            Ok(r) => r,
            Err(_) => return Ok(()), // not a request: ignore
        };
        let response = match self.blocks.handle_request(&bytes) {
            Ok(b) => b,
            // Unknown block or oversized: stay silent. M5 has no NACK;
            // the client times out and retries.
            Err(_) => return Ok(()),
        };
        // The transfer id is derived from the requested seqno so the
        // client can filter stale frames (design decision 7).
        rldp_send(
            &self.transport,
            peer,
            response_transfer_id(req.seq_no),
            &response,
            &self.config.rldp,
            self.config.ack_timeout,
        )
        .await
    }

    /// Serve forever. Returns only on our own transport failure.
    pub async fn run(&self) -> Result<(), SyncError> {
        loop {
            self.serve_one().await?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_sync::{decode_request, encode_response, BlockResponse};
    use crate::testutil::LoopbackTransport;
    use onx_primitives::SecretKey;

    fn test_peer(address: Uint256, seed: u8) -> SyncPeer {
        let secret = SecretKey::from_seed(&[seed; 32]).unwrap();
        SyncPeer {
            public_key: secret.public_key(),
            endpoint: "127.0.0.1:0".parse().unwrap(),
            address,
        }
    }

    fn test_config() -> SyncConfig {
        SyncConfig {
            fetch_timeout: Duration::from_millis(500),
            ack_timeout: Duration::from_millis(100),
            ..SyncConfig::default()
        }
    }

    /// Write `block-{seqno:08}.blk` with `bytes` into a fresh temp dir.
    fn block_dir(seq_no: u32, bytes: &[u8]) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "onx-sync-test-{nanos}-{}-{seq_no}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("block-{seq_no:08}.blk")), bytes).unwrap();
        dir
    }

    #[tokio::test]
    async fn fetch_block_round_trip() {
        let addr_server = Uint256([0xA1; 32]);
        let addr_client = Uint256([0xB2; 32]);
        let (tx_server, tx_client) = LoopbackTransport::pair(addr_server, addr_client);
        let peer_server = test_peer(addr_server, 0x11);
        let peer_client = test_peer(addr_client, 0x22);

        let block_bytes = vec![0x42u8; 1024];
        let dir = block_dir(1, &block_bytes);
        let server = SyncServer::new(tx_server, vec![peer_client], dir.clone(), test_config());
        let server_task = tokio::spawn(async move { server.serve_one().await });

        let client = SyncClient::new(tx_client, peer_server, test_config());
        let fetched = client.fetch_block(1).await.expect("fetch works");
        assert_eq!(fetched, block_bytes);

        server_task.await.unwrap().expect("serve_one ok");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn fetch_block_large_response_uses_rldp() {
        let addr_server = Uint256([0xA1; 32]);
        let addr_client = Uint256([0xB2; 32]);
        let (tx_server, tx_client) = LoopbackTransport::pair(addr_server, addr_client);
        let peer_server = test_peer(addr_server, 0x11);
        let peer_client = test_peer(addr_client, 0x22);

        // 200 KiB >> one 64 KiB datagram: forces multi-frame RLDP.
        let block_bytes: Vec<u8> = (0..200 * 1024).map(|i| (i % 251) as u8).collect();
        let dir = block_dir(7, &block_bytes);
        let server = SyncServer::new(tx_server, vec![peer_client], dir.clone(), test_config());
        let server_task = tokio::spawn(async move { server.serve_one().await });

        let client = SyncClient::new(tx_client, peer_server, test_config());
        let fetched = client.fetch_block(7).await.expect("large fetch works");
        assert_eq!(fetched, block_bytes);

        server_task.await.unwrap().expect("serve_one ok");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn fetch_missing_block_times_out() {
        let addr_server = Uint256([0xA1; 32]);
        let addr_client = Uint256([0xB2; 32]);
        let (tx_server, tx_client) = LoopbackTransport::pair(addr_server, addr_client);
        let peer_server = test_peer(addr_server, 0x11);
        let peer_client = test_peer(addr_client, 0x22);

        let dir = block_dir(1, &[0x42; 16]); // no block 9
        let server = SyncServer::new(tx_server, vec![peer_client], dir.clone(), test_config());
        let server_task = tokio::spawn(async move { server.serve_one().await });

        let client = SyncClient::new(tx_client, peer_server, test_config());
        let err = client
            .fetch_block(9)
            .await
            .expect_err("missing block times out");
        assert!(
            matches!(err, SyncError::Timeout { .. }),
            "expected Timeout, got {err:?}"
        );

        // The server saw the request, had no block, stayed silent.
        server_task.await.unwrap().expect("serve_one ok");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn server_ignores_garbage_and_unknown_senders() {
        let addr_server = Uint256([0xA1; 32]);
        let addr_client = Uint256([0xB2; 32]);
        let addr_stranger = Uint256([0xC3; 32]);
        let (tx_server, tx_client) = LoopbackTransport::pair(addr_server, addr_client);
        let peer_client = test_peer(addr_client, 0x22);

        let dir = block_dir(1, &[0x42; 16]);

        // 1. Garbage from a known peer: queued first, ignored, no response.
        tx_client
            .send_to(&test_peer(addr_server, 0x11), vec![0xFF; 20])
            .await
            .unwrap();
        // 2. Well-formed request from an unknown sender: queued second.
        // Inject it straight into the server's inbox (the stranger is on a
        // separate loopback pair, so it cannot address the server directly —
        // the point is the server checks the sender address). Queued before
        // the transport moves into the server below.
        tx_server.inject(addr_stranger, encode_request(&BlockRequest { seq_no: 1 }));

        let server = SyncServer::new(tx_server, vec![peer_client], dir.clone(), test_config());
        server.serve_one().await.expect("garbage ignored");
        server.serve_one().await.expect("unknown sender ignored");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn fetch_rejects_wrong_seqno_response() {
        let addr_server = Uint256([0xA1; 32]);
        let addr_client = Uint256([0xB2; 32]);
        let (tx_server, tx_client) = LoopbackTransport::pair(addr_server, addr_client);
        let peer_server = test_peer(addr_server, 0x11);
        let peer_client = test_peer(addr_client, 0x22);

        // Rogue server: answers with a response for the WRONG seqno.
        let server_task = tokio::spawn(async move {
            let (_sender, req_bytes) = tx_server.recv_from().await.unwrap();
            let req = decode_request(&req_bytes).unwrap();
            let bad = encode_response(&BlockResponse {
                seq_no: req.seq_no + 1,
                block_bytes: vec![0x99; 32],
            });
            rldp_send(
                &tx_server,
                &peer_client,
                response_transfer_id(req.seq_no),
                &bad,
                &RldpConfig {
                    redundancy: 1,
                    ..RldpConfig::default()
                },
                Duration::from_millis(100),
            )
            .await
            .unwrap();
        });

        let client = SyncClient::new(tx_client, peer_server, test_config());
        let err = client
            .fetch_block(1)
            .await
            .expect_err("wrong seqno rejected");
        assert!(
            matches!(err, SyncError::SeqNoMismatch { .. }),
            "expected SeqNoMismatch, got {err:?}"
        );
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn consecutive_fetches_do_not_see_stale_frames() {
        let addr_server = Uint256([0xA1; 32]);
        let addr_client = Uint256([0xB2; 32]);
        let (tx_server, tx_client) = LoopbackTransport::pair(addr_server, addr_client);
        let peer_server = test_peer(addr_server, 0x11);
        let peer_client = test_peer(addr_client, 0x22);

        let dir = block_dir(1, &[0x42; 16]);
        std::fs::write(dir.join("block-00000002.blk"), [0x43; 16]).unwrap();
        let server = SyncServer::new(tx_server, vec![peer_client], dir.clone(), test_config());
        let server_task = tokio::spawn(async move {
            server.serve_one().await.unwrap();
            server.serve_one().await.unwrap();
        });

        let client = SyncClient::new(tx_client, peer_server, test_config());
        let b1 = client.fetch_block(1).await.expect("fetch 1");
        assert_eq!(b1, [0x42; 16]);
        // The second fetch must not pick up stale frames from the first
        // transfer (transfer ids differ per seqno).
        let b2 = client.fetch_block(2).await.expect("fetch 2");
        assert_eq!(b2, [0x43; 16]);

        server_task.await.unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
