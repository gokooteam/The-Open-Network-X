# ONX Specification — Block Sync Between Nodes

**Status:** Draft
**Scope:** The M5 wire protocol by which a follower fetches committed
blocks from a producer. Covers message formats, transport mapping, and
the trust boundary. The follower's apply path (signature verification,
replay, persistence, crash resume) is specified with the `onxd` follower
loop, not here.

## 1. Reference

- `WHITEPAPER.md` §3.3: overlay broadcast and streaming (the M6
  generalization of the M5 announcement path).
- `docs/adr/0042-networking-unfreeze.md`: the unfreeze decision; the
  mempool file-drop path stays.
- `docs/adr/0043-static-peer-discovery.md`: M5 peers come from a static
  list; no DHT.
- `docs/adr/0044-direct-submission-no-gossip.md`: no mempool gossip;
  messages reach the producer by direct submission.
- `docs/specification/networking-adnl.md` §4.2–§4.3: ADNL datagrams and
  RLDP chunking (the transports this protocol rides).
- `docs/specification/blocks.md`: block structure; `ONXBLK05`
  authenticated headers (ADR-0032).

## 2. Requirement

M5 requires that a follower on a different machine sync from genesis
through peers, verify `ONXBLK05` signatures itself, persist the blocks,
and reach the same state root as the producer. That needs a wire
protocol with exactly three properties: the producer can announce new
blocks, the follower can fetch any block by sequence number, and no
byte received from a peer is trusted without verification.

## 3. ONX interpretation

Three messages, one envelope. The envelope is
`version:uint8 | msg_type:uint8 | payload_len:uint32be | payload`, with
`version = 1`. All multi-byte integers are big-endian. Decoders are
strict and fail closed: wrong version, unknown type, length mismatch,
trailing bytes, or total size over 8 MiB is a rejection, never a
partial parse.

- **BlockAnnouncement** (`0x01`): `seq_no:uint32be | block_hash:[uint8;32]
  | state_root:[uint8;32]` (exactly 68 bytes). The producer broadcasts
  one per committed block to every configured peer. `block_hash` is the
  `onx_stf::BlockHeader::hash()` of the committed block; `state_root` is
  informational until the block verifies.
- **BlockRequest** (`0x02`): `seq_no:uint32be` (exactly 4 bytes). The
  follower asks a peer for one block's bytes.
- **BlockResponse** (`0x03`): `seq_no:uint32be | block_len:uint32be |
  block_bytes[block_len]`. The producer answers with the raw
  `block-{seqno:08}.blk` file bytes. `block_len` must equal the actual
  trailing length; the response seqno must equal the request seqno.

Transport mapping: announcements go to every connected peer (the M5
"overlay" is the producer's static peer list — ADR-0043; a real
broadcast overlay is M6 work). Requests and responses travel
point-to-point over the ADNL channel to the peer. A response larger
than one ADNL datagram (~64 KB) MUST be chunked through RLDP
(`networking-adnl.md` §4.3); the 8 MiB codec cap bounds the reassembled
total, not the datagram.

Trust boundary: the sync layer guarantees framing integrity only. The
follower MUST, for every fetched block, in order: check the
`ONXBLK05` magic, strict-decode the file, confirm the recomputed block
hash (`onx_stf::BlockHeader::hash()`) matches the announcement, then run
`verify_block_auth` against the genesis validator set with the
chain-bound preimage (ADR-0032 §5), and only then apply through the same
deterministic path as `onx replay`. The hash check comes before
signature verification deliberately: it is a cheap filter against a
mismatched or stale response, and the signature check remains the gate
before anything is applied. A peer that serves corrupted, forged,
out-of-order, or wrong-chain blocks is rejected at the first failing
check, and the follower moves on to the next peer. Network reachability,
timing, and duplicate arrival never alter validity.

## 4. Serialization

Reference implementation: `crates/node/onx-networking/src/block_sync.rs`
(`encode_announcement`, `encode_request`, `encode_response`,
`decode_announcement`, `decode_request`, `decode_response`,
`BlockServer::handle_request`). The file name served for a request is
`block-{seqno:08}.blk`, fully determined by the `u32` seqno — no path
traversal is possible through it.

## 5. Malformed-input behavior

Reject: envelope shorter than 6 bytes; `version != 1`; unknown
`msg_type`; `payload_len` disagreeing with actual trailing bytes;
total over 8 MiB; announcement/request payloads of the wrong fixed
length; response with `block_len` disagreeing with trailing bytes;
response seqno not matching the request; request for a seqno with no
block file; block file over `MAX_BLOCK_FILE_BYTES` (8 MiB minus the
14-byte response framing overhead — a larger file would encode to a
response no follower can decode). All rejections are
deterministic and carry no partial state.

## 6. Test plan

Codec round-trips for all three messages; every §5 rejection with a
dedicated vector; server serves the exact file bytes for a known
seqno; server rejects unknown seqnos, wrong-type messages, and
garbage; file-name traversal safety across the `u32` range;
announcement hash mismatch detected before signature verification
(follower loop); adversarial peer battery — corrupted, forged,
out-of-order, wrong-chain blocks and mid-transfer disconnects — at the
follower loop level, where verification lives.
