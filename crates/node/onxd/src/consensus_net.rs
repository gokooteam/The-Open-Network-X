//! Consensus gossip transport (M6).
//!
//! Validators broadcast proposals and votes over direct TCP connections.
//! This is deliberately simple: length-prefixed messages, connect on
//! broadcast, no encryption. Production should use the ADNL transport;
//! see the module docs for the hardening plan.
//!
//! The tick loop (sync) talks to the network task (async tokio) via
//! `std::sync::mpsc` channels: non-blocking `try_recv` on inbound each
//! tick, `send` on outbound after the tick.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use crate::producer::InboundConsensusMsg;

/// Outbound consensus message for the network.
#[derive(Debug)]
pub enum OutboundConsensusMsg {
    Proposal(Vec<u8>, Vec<u8>),
    Vote(Vec<u8>),
}

/// Envelope kinds.
const KIND_PROPOSAL: u8 = 0;
const KIND_VOTE: u8 = 1;

/// Encode an outbound message: [4B BE len][1B kind][payload].
pub fn encode_envelope(msg: &OutboundConsensusMsg) -> Vec<u8> {
    let mut payload = Vec::new();
    match msg {
        OutboundConsensusMsg::Proposal(proposal, block) => {
            payload.push(KIND_PROPOSAL);
            payload.extend_from_slice(&(proposal.len() as u32).to_be_bytes());
            payload.extend_from_slice(proposal);
            payload.extend_from_slice(&(block.len() as u32).to_be_bytes());
            payload.extend_from_slice(block);
        }
        OutboundConsensusMsg::Vote(vote) => {
            payload.push(KIND_VOTE);
            payload.extend_from_slice(vote);
        }
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    out
}

/// Decode an envelope payload into an inbound message.
pub fn decode_envelope(payload: &[u8]) -> Result<InboundConsensusMsg, String> {
    if payload.is_empty() {
        return Err("empty consensus envelope".to_string());
    }
    match payload[0] {
        KIND_PROPOSAL => {
            let rest = &payload[1..];
            if rest.len() < 4 {
                return Err("truncated proposal envelope".to_string());
            }
            let plen = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            let rest = &rest[4..];
            if rest.len() < plen + 4 {
                return Err("truncated proposal bytes".to_string());
            }
            let proposal = rest[..plen].to_vec();
            let rest = &rest[plen..];
            let blen = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            let rest = &rest[4..];
            if rest.len() < blen {
                return Err("truncated block bytes".to_string());
            }
            let block = rest[..blen].to_vec();
            Ok(InboundConsensusMsg::Proposal(proposal, block))
        }
        KIND_VOTE => Ok(InboundConsensusMsg::Vote(payload[1..].to_vec())),
        k => Err(format!("unknown consensus envelope kind {k}")),
    }
}

/// Spawn the consensus network task. Returns the channel pair for the
/// tick loop: received messages come in on `inbound_rx`, broadcasts go
/// out on `outbound_tx`.
pub fn spawn_consensus_net(
    bind_addr: SocketAddr,
    peers: Vec<SocketAddr>,
) -> (Receiver<InboundConsensusMsg>, Sender<OutboundConsensusMsg>) {
    let (inbound_tx, inbound_rx) = std::sync::mpsc::channel();
    let (outbound_tx, outbound_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        if let Err(e) = run_net(bind_addr, peers, inbound_tx, outbound_rx) {
            eprintln!("consensus-net: fatal: {e}");
        }
    });

    (inbound_rx, outbound_tx)
}

fn run_net(
    bind_addr: SocketAddr,
    peers: Vec<SocketAddr>,
    inbound_tx: Sender<InboundConsensusMsg>,
    outbound_rx: Receiver<OutboundConsensusMsg>,
) -> Result<(), String> {
    let listener = TcpListener::bind(bind_addr)
        .map_err(|e| format!("consensus-net: bind {bind_addr}: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("consensus-net: nonblocking: {e}"))?;
    eprintln!(
        "consensus-net: listening on {bind_addr}, {} peers",
        peers.len()
    );

    loop {
        // Accept inbound connections (non-blocking).
        match listener.accept() {
            Ok((stream, addr)) => {
                let tx = inbound_tx.clone();
                std::thread::spawn(move || handle_inbound(stream, addr, tx));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => eprintln!("consensus-net: accept error: {e}"),
        }

        // Broadcast outbound messages.
        match outbound_rx.try_recv() {
            Ok(msg) => {
                let bytes = encode_envelope(&msg);
                for peer in &peers {
                    if let Err(e) = send_to_peer(*peer, &bytes) {
                        eprintln!("consensus-net: send to {peer} failed: {e}");
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err("outbound channel closed".to_string());
            }
        }

        std::thread::sleep(Duration::from_millis(10));
    }
}

fn handle_inbound(
    mut stream: TcpStream,
    addr: SocketAddr,
    inbound_tx: Sender<InboundConsensusMsg>,
) {
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    loop {
        let mut len_buf = [0u8; 4];
        if stream.read_exact(&mut len_buf).is_err() {
            return;
        }
        let len = u32::from_be_bytes(len_buf) as usize;
        if len == 0 || len > 16 * 1024 * 1024 {
            eprintln!("consensus-net: bad message len {len} from {addr}");
            return;
        }
        let mut payload = vec![0u8; len];
        if stream.read_exact(&mut payload).is_err() {
            return;
        }
        match decode_envelope(&payload) {
            Ok(msg) => {
                if inbound_tx.send(msg).is_err() {
                    return;
                }
            }
            Err(e) => eprintln!("consensus-net: bad envelope from {addr}: {e}"),
        }
    }
}

fn send_to_peer(peer: SocketAddr, bytes: &[u8]) -> Result<(), String> {
    let mut stream = TcpStream::connect_timeout(&peer, Duration::from_secs(5))
        .map_err(|e| format!("connect: {e}"))?;
    stream.write_all(bytes).map_err(|e| format!("write: {e}"))?;
    stream.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrip_proposal() {
        let msg = OutboundConsensusMsg::Proposal(vec![1, 2, 3], vec![4, 5]);
        let enc = encode_envelope(&msg);
        let len = u32::from_be_bytes([enc[0], enc[1], enc[2], enc[3]]) as usize;
        let decoded = decode_envelope(&enc[4..4 + len]).unwrap();
        match decoded {
            InboundConsensusMsg::Proposal(p, b) => {
                assert_eq!(p, vec![1, 2, 3]);
                assert_eq!(b, vec![4, 5]);
            }
            _ => panic!("wrong kind"),
        }
    }

    #[test]
    fn envelope_roundtrip_vote() {
        let msg = OutboundConsensusMsg::Vote(vec![7, 8, 9]);
        let enc = encode_envelope(&msg);
        let len = u32::from_be_bytes([enc[0], enc[1], enc[2], enc[3]]) as usize;
        let decoded = decode_envelope(&enc[4..4 + len]).unwrap();
        match decoded {
            InboundConsensusMsg::Vote(v) => assert_eq!(v, vec![7, 8, 9]),
            _ => panic!("wrong kind"),
        }
    }

    #[test]
    fn envelope_rejects_bad_kind() {
        assert!(decode_envelope(&[0xff, 0x00]).is_err());
    }

    #[test]
    fn envelope_rejects_truncated() {
        assert!(decode_envelope(&[KIND_PROPOSAL, 0x00]).is_err());
    }
}
