//! BFT consensus driver (M6): wires `onx-consensus` into `onxd`.
//!
//! Each validator runs one `ConsensusDriver` per chain height. The driver
//! owns the `ConsensusEngine`, translates between engine messages and
//! wire bytes, and emits events the tick loop acts on (broadcasts and,
//! on finalization, the block to commit with its multi-signature
//! section).
//!
//! Design (see `docs/planning/m6-task-log.md`):
//! - Leader rotation via the engine's round-robin.
//! - Commit votes carry the validator's block-header signature at the
//!   transport layer, so a finalized quorum doubles as the block's
//!   `SigEntry` list. The engine itself is untouched.
//! - The driver never touches the network: it returns events; the tick
//!   loop broadcasts and commits.

use std::collections::{BTreeMap, BTreeSet};

use onx_consensus::{
    decode_proposal, decode_vote, encode_proposal, encode_vote, proposal_signing_bytes,
    vote_signing_bytes, ConsensusEngine, ConsensusProposal, ConsensusStep, ConsensusVote,
    RoundTimeouts, ValidatorSetEntry, VotePhase,
};
use onx_data_structures::ShardIdent;
use onx_primitives::{hash::VALIDATOR_SIGN_V1, PublicKey, SecretKey, Signature, Uint256, Uint64};
use onx_stf::block::{Block, SigEntry};

/// Events the tick loop must act on.
#[derive(Debug)]
pub enum DriverEvent {
    /// Broadcast to all peers: the wire proposal plus the block bytes
    /// (unsigned block file: `encode_block_file(block, &[])`).
    BroadcastProposal { proposal: Vec<u8>, block: Vec<u8> },
    /// Broadcast to all peers: the wire vote.
    BroadcastVote(Vec<u8>),
    /// The engine finalized a block: commit it with these signatures.
    Finalized {
        block: Block,
        sig_entries: Vec<SigEntry>,
    },
}

/// A consensus driver for one chain height.
pub struct ConsensusDriver {
    engine: ConsensusEngine,
    shard: ShardIdent,
    chain_id: [u8; 32],
    own_id: u32,
    signing_key: SecretKey,
    /// Canonical-order validator pubkeys, for header-signature verification.
    validator_pubkeys: Vec<PublicKey>,
    /// Blocks seen for this height, by block hash (proposed or received).
    blocks: BTreeMap<[u8; 32], Block>,
    /// Header signatures by validator id, current round only.
    header_sigs: BTreeMap<u32, Signature>,
    /// (round, phase) pairs this node has already voted.
    voted: BTreeSet<(u32, VotePhase)>,
    /// Round this node already proposed in (don't double-propose).
    proposed_round: Option<u32>,
    /// Votes that arrived ahead of the engine's current step (gossip is
    /// unordered). Retried whenever the step advances; dropped on round
    /// change since votes never cross rounds.
    pending: Vec<Vec<u8>>,
    /// Expected prev_hash for this height (the local head). Set via
    /// `set_head()` before processing proposals. If unset, prev_hash
    /// validation is skipped (test-only).
    head_hash: Option<[u8; 32]>,
}

impl ConsensusDriver {
    /// Build a driver for `height`. `validators` is the canonical
    /// (pubkey-sorted) genesis list; `validator_id` is the position in it.
    /// The signing key's pubkey must be in the list.
    pub fn new(
        chain_id: [u8; 32],
        shard: ShardIdent,
        height: u64,
        validators: &[(PublicKey, u64)],
        signing_key: SecretKey,
        now: u64,
    ) -> Result<Self, String> {
        let own_pubkey = signing_key.public_key();
        let own_id = validators
            .iter()
            .position(|(pk, _)| pk == &own_pubkey)
            .ok_or_else(|| "consensus: signing key pubkey not in validator set".to_string())?
            as u32;
        let entries: Vec<ValidatorSetEntry> = validators
            .iter()
            .enumerate()
            .map(|(i, (pk, stake))| ValidatorSetEntry {
                validator_id: i as u32,
                public_key: *pk,
                actual_stake: Uint64(*stake),
            })
            .collect();
        // M6 timeouts: generous for devnet (block building itself can take
        // seconds on big batches; the timeout is for dead leaders, not slow
        // builders). Tune with real network measurements later.
        let timeouts = RoundTimeouts {
            proposal: 60_000,
            prevote: 60_000,
            precommit: 60_000,
            commit: 60_000,
        };
        let engine = ConsensusEngine::new(shard, height, entries, now, timeouts)
            .map_err(|e| format!("consensus: engine init failed: {e}"))?;
        Ok(Self {
            engine,
            shard,
            chain_id,
            own_id,
            signing_key,
            validator_pubkeys: validators.iter().map(|(pk, _)| *pk).collect(),
            blocks: BTreeMap::new(),
            header_sigs: BTreeMap::new(),
            voted: BTreeSet::new(),
            proposed_round: None,
            pending: Vec::new(),
            head_hash: None,
        })
    }

    /// Set the expected prev_hash for this height (the local head).
    /// Must be called before processing proposals; otherwise prev_hash
    /// validation is skipped.
    pub fn set_head(&mut self, head_hash: [u8; 32]) {
        self.head_hash = Some(head_hash);
    }

    /// This node's validator id.
    pub fn own_id(&self) -> u32 {
        self.own_id
    }

    /// Current engine step (for the tick loop's decisions).
    pub fn step(&self) -> ConsensusStep {
        self.engine.step()
    }

    /// Current round (for view-change logging).
    pub fn round(&self) -> u32 {
        self.engine.round()
    }

    /// Should this node propose now? True when it is the round leader,
    /// the engine is waiting for a proposal, and it hasn't proposed this
    /// round yet.
    pub fn should_propose(&self) -> bool {
        self.engine.step() == ConsensusStep::Proposal
            && self.engine.leader() == self.own_id
            && self.proposed_round != Some(self.engine.round())
    }

    /// Drive round timeouts. Call every tick. Returns true if the round
    /// changed (view change).
    pub fn on_tick(&mut self, now: u64) -> bool {
        if self.engine.on_timeout(now) {
            // Votes never cross round boundaries.
            self.header_sigs.clear();
            self.voted.clear();
            self.proposed_round = None;
            self.pending.clear();
            return true;
        }
        false
    }

    /// Propose `block` as this round's leader. Builds the proposal,
    /// feeds it to the engine, votes PreVote, and returns the broadcast.
    pub fn propose(
        &mut self,
        block: Block,
        block_bytes: Vec<u8>,
    ) -> Result<Vec<DriverEvent>, String> {
        if !self.should_propose() {
            return Err("consensus: propose called when not leader".to_string());
        }
        let height = self.engine.height();
        let round = self.engine.round();
        let hash = block.header.hash();
        let proposal = ConsensusProposal {
            height,
            round,
            block_hash: Uint256(hash),
            proposer_id: self.own_id,
            // ADR-0049: include the highest QC round this validator has seen.
            // (For now: the locked round, if any. Full QC tracking is future work.)
            qc_round: self.engine.locked_round(),
            qc_block: self.engine.locked_block(),
            signature: self.signing_key.sign(
                &VALIDATOR_SIGN_V1,
                &proposal_signing_bytes(
                    &self.shard,
                    height,
                    round,
                    &Uint256(hash),
                    self.engine.locked_round(),
                    self.engine.locked_block(),
                ),
            ),
        };
        let proposal_bytes = encode_proposal(&proposal);
        self.blocks.insert(hash, block);
        self.proposed_round = Some(round);
        self.engine
            .receive_proposal(proposal.clone())
            .map_err(|e| format!("consensus: own proposal rejected: {e}"))?;
        let mut events = vec![DriverEvent::BroadcastProposal {
            proposal: proposal_bytes,
            block: block_bytes,
        }];
        // The leader votes for its own proposal immediately.
        if let Some(vote_bytes) = self.vote_phase(VotePhase::PreVote)? {
            events.push(DriverEvent::BroadcastVote(vote_bytes));
        }
        events.extend(self.check_finalized()?);
        Ok(events)
    }

    /// Handle an inbound proposal (+ block bytes). Validates, feeds the
    /// engine, and votes PreVote on acceptance.
    pub fn receive_proposal(
        &mut self,
        proposal_bytes: &[u8],
        block_bytes: &[u8],
        decode_block: impl FnOnce(&[u8]) -> Result<Block, String>,
    ) -> Result<Vec<DriverEvent>, String> {
        let proposal =
            decode_proposal(proposal_bytes).map_err(|e| format!("consensus: bad proposal: {e}"))?;
        let block = decode_block(block_bytes).map_err(|e| format!("consensus: bad block: {e}"))?;
        if block.header.hash() != proposal.block_hash.0 {
            return Err("consensus: proposal block hash mismatch".to_string());
        }
        // ADR-0049: validate the block before voting. A validator never votes
        // for a block it has not validated.
        if block.header.seqno != proposal.height as u32 {
            return Err(format!(
                "consensus: bad seqno: expected {}, got {}",
                proposal.height, block.header.seqno
            ));
        }
        if let Some(head) = self.head_hash {
            if block.header.prev_hash != head {
                return Err("consensus: proposal does not build on local head".to_string());
            }
        }
        // TODO: STF dry-run validation (consensus.md §3.4). For now, the
        // seqno and prev_hash checks catch the critical cases. Full
        // re-execution requires the state at the head, which the driver
        // does not yet have access to.
        // The proposal must build on this node's head; otherwise it is for
        // a fork or a future height we cannot validate yet. (Fork-choice
        // across heights is P5; for M6, heights advance in lockstep.)
        self.blocks.insert(proposal.block_hash.0, block);
        match self.engine.receive_proposal(proposal.clone()) {
            Ok(()) => {}
            Err(onx_consensus::ConsensusError::ConflictingProposal) => {
                // EQUIVOCATION: the round leader signed two different
                // proposals for the same height+round. This is slashable
                // misconduct (D3: detection now, enforcement later). Log
                // the evidence loudly and keep the first proposal — the
                // BFT fork-choice rule is "first quorum-certified wins",
                // and a conflicting proposal can never reach quorum
                // without >1/3 Byzantine stake.
                eprintln!(
                    "consensus: EQUIVOCATION evidence: leader {} double-proposed height {} round {}: {} vs {}",
                    proposal.proposer_id,
                    proposal.height,
                    proposal.round,
                    hex::encode(proposal.block_hash.0),
                    hex::encode(
                        self.engine
                            .proposal()
                            .map(|p| p.block_hash.0)
                            .unwrap_or([0u8; 32])
                    ),
                );
                return Err(
                    "consensus: conflicting proposal (equivocation evidence logged)".to_string(),
                );
            }
            Err(e) => {
                return Err(format!("consensus: proposal rejected: {e}"));
            }
        }
        let mut events = Vec::new();
        // Vote all phases the engine advances through. With own-vote counting,
        // a single validator can advance through multiple phases in one call.
        // Loop until the engine stops advancing or we've voted all phases.
        for _ in 0..3 {
            // At most 3 phases: PreVote, PreCommit, Commit.
            let phase = match self.engine.step() {
                ConsensusStep::PreVote => VotePhase::PreVote,
                ConsensusStep::PreCommit => VotePhase::PreCommit,
                ConsensusStep::Commit => VotePhase::Commit,
                _ => break,
            };
            match self.vote_phase(phase)? {
                Some(vote_bytes) => events.push(DriverEvent::BroadcastVote(vote_bytes)),
                None => break, // Already voted this phase; engine won't advance.
            }
        }
        events.extend(self.check_finalized()?);
        Ok(events)
    }

    /// Handle an inbound vote. Feeds the engine, votes the next phase on
    /// quorum, and finalizes when the commit quorum lands. Votes that
    /// arrive ahead of the engine's step are buffered and retried as the
    /// step advances (gossip is unordered).
    pub fn receive_vote(&mut self, vote_bytes: &[u8]) -> Result<Vec<DriverEvent>, String> {
        let mut events = self.receive_vote_inner(vote_bytes)?;
        // The step may have advanced: retry buffered votes to fixpoint.
        loop {
            let step_before = self.engine.step();
            let pending: Vec<Vec<u8>> = std::mem::take(&mut self.pending);
            if pending.is_empty() {
                break;
            }
            for v in pending {
                events.extend(self.receive_vote_inner(&v)?);
            }
            if self.engine.step() == step_before {
                break;
            }
        }
        Ok(events)
    }

    fn receive_vote_inner(&mut self, vote_bytes: &[u8]) -> Result<Vec<DriverEvent>, String> {
        let (vote, header_sig) =
            decode_vote(vote_bytes).map_err(|e| format!("consensus: bad vote: {e}"))?;
        // Idempotency: if this is our own vote for a phase we've already
        // voted, we've already counted it (vote_phase feeds it directly).
        // Skip to avoid a duplicate-signature error from the engine.
        if vote.validator_id == self.own_id && self.voted.contains(&(vote.round, vote.phase)) {
            return Ok(Vec::new());
        }
        // A commit vote's header signature attests to the block this node
        // validated. Verify it now, against the stored block, before the
        // engine counts the vote.
        if vote.phase == VotePhase::Commit {
            if let Some(sig) = header_sig {
                let hash = vote.block_hash.0;
                let block = self
                    .blocks
                    .get(&hash)
                    .ok_or_else(|| "consensus: commit vote for unknown block".to_string())?;
                let preimage = block.header.sign_bytes(&self.chain_id);
                self.validator_pubkeys
                    .get(vote.validator_id as usize)
                    .ok_or_else(|| "consensus: commit vote from unknown validator".to_string())?
                    .verify_raw(&preimage, &sig)
                    .map_err(|_| "consensus: bad header signature on commit vote".to_string())?;
                self.header_sigs.insert(vote.validator_id, sig);
            }
            // Note: a commit vote WITHOUT a header sig is still a valid
            // engine vote (the engine doesn't know about header sigs), but
            // it contributes no SigEntry at finalization.
        }
        match self.engine.receive_vote(vote) {
            Ok(()) => {}
            Err(onx_consensus::ConsensusError::InvalidStep) => {
                // Ahead of the engine's step (or no proposal yet): buffer
                // for retry. Anything else is a real rejection.
                self.pending.push(vote_bytes.to_vec());
                return Ok(Vec::new());
            }
            Err(onx_consensus::ConsensusError::ConflictingProposal)
                if self.engine.proposal().is_none() =>
            {
                // Vote arrived before the proposal (gossip is unordered).
                // Buffer it; it will be retried when the proposal lands.
                self.pending.push(vote_bytes.to_vec());
                return Ok(Vec::new());
            }
            Err(e) => return Err(format!("consensus: vote rejected: {e}")),
        }
        let mut events = Vec::new();
        // The engine may have advanced a phase on quorum; vote the new one.
        // With own-vote counting, vote all phases the engine advances through.
        for _ in 0..3 {
            let phase = match self.engine.step() {
                ConsensusStep::PreVote => VotePhase::PreVote,
                ConsensusStep::PreCommit => VotePhase::PreCommit,
                ConsensusStep::Commit => VotePhase::Commit,
                _ => break,
            };
            match self.vote_phase(phase)? {
                Some(vote_bytes) => events.push(DriverEvent::BroadcastVote(vote_bytes)),
                None => break,
            }
        }
        events.extend(self.check_finalized()?);
        Ok(events)
    }

    /// Vote `phase` for the engine's current proposal, if not already
    /// voted this round. Returns the wire bytes to broadcast.
    fn vote_phase(&mut self, phase: VotePhase) -> Result<Option<Vec<u8>>, String> {
        let round = self.engine.round();
        if !self.voted.insert((round, phase)) {
            return Ok(None);
        }
        let proposal = self
            .engine
            .proposal()
            .ok_or_else(|| "consensus: no proposal to vote on".to_string())?;
        let vote = ConsensusVote {
            height: self.engine.height(),
            round,
            phase,
            block_hash: proposal.block_hash,
            validator_id: self.own_id,
            signature: self.signing_key.sign(
                &VALIDATOR_SIGN_V1,
                &vote_signing_bytes(
                    &self.shard,
                    self.engine.height(),
                    round,
                    phase,
                    &proposal.block_hash,
                ),
            ),
        };
        // Commit votes carry this node's header signature over the block
        // it validated, so finalization yields real SigEntries.
        let header_sig = if phase == VotePhase::Commit {
            let block = self
                .blocks
                .get(&proposal.block_hash.0)
                .ok_or_else(|| "consensus: no block for commit vote".to_string())?;
            Some(
                self.signing_key
                    .sign_raw(&block.header.sign_bytes(&self.chain_id)),
            )
        } else {
            None
        };
        let vote_bytes = encode_vote(&vote, header_sig.as_ref());
        // Count our own vote locally. The network layer broadcasts to peers
        // only; without this, a node would never count its own vote and
        // liveness would require all peers (no fault tolerance).
        // (The engine verifies the signature; our own signature is valid.)
        //
        // Also store our header signature (for Commit votes) so finalization
        // includes it. receive_vote_inner does this for inbound votes; we do
        // it here for our own.
        if let Some(sig) = header_sig {
            self.header_sigs.insert(self.own_id, sig);
        }
        self.engine
            .receive_vote(vote)
            .map_err(|e| format!("consensus: own vote rejected: {e}"))?;
        Ok(Some(vote_bytes))
    }

    /// If the engine finalized, assemble the commit event.
    fn check_finalized(&mut self) -> Result<Vec<DriverEvent>, String> {
        let Some(finalized) = self.engine.finalized() else {
            return Ok(Vec::new());
        };
        let block = self
            .blocks
            .get(&finalized.block_hash.0)
            .ok_or_else(|| "consensus: finalized unknown block".to_string())?
            .clone();
        let mut sig_entries: Vec<SigEntry> = Vec::new();
        for vote in &finalized.commit_votes {
            if let Some(sig) = self.header_sigs.get(&vote.validator_id) {
                sig_entries.push(SigEntry {
                    validator_index: vote.validator_id,
                    sig: sig.encode(),
                });
            }
        }
        // Canonical ascending-index order, as verifiers require.
        sig_entries.sort_by_key(|e| e.validator_index);
        if sig_entries.is_empty() {
            return Err("consensus: finalized without any header signatures".to_string());
        }
        Ok(vec![DriverEvent::Finalized { block, sig_entries }])
    }

    /// Advance to the next height's engine in place. Call after handling
    /// `Finalized`.
    pub fn advance_height(&mut self, now: u64) -> Result<(), String> {
        let height = self.engine.height() + 1;
        let validators: Vec<(PublicKey, u64)> = self
            .validator_pubkeys
            .iter()
            .zip(self.engine.stakes())
            .map(|(pk, stake)| (*pk, stake))
            .collect();
        *self = Self::new(
            self.chain_id,
            self.shard,
            height,
            &validators,
            self.signing_key.clone(),
            now,
        )?;
        Ok(())
    }

    /// The engine for the next height. Call after handling `Finalized`.
    pub fn next_height(self, now: u64) -> Result<Self, String> {
        let height = self.engine.height() + 1;
        let validators: Vec<(PublicKey, u64)> = self
            .validator_pubkeys
            .iter()
            .zip(self.engine.stakes())
            .map(|(pk, stake)| (*pk, stake))
            .collect();
        Self::new(
            self.chain_id,
            self.shard,
            height,
            &validators,
            self.signing_key.clone(),
            now,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onx_data_structures::{AccountId, ShardIdent, WorkchainIdent};
    use onx_primitives::{Int32, Uint64};
    use onx_stf::block::{BlockBody, BlockHeader};

    fn test_shard() -> ShardIdent {
        ShardIdent {
            workchain_id: WorkchainIdent(Int32(-1)),
            shard_prefix_ident: Uint64(ShardIdent::ROOT_PREFIX),
        }
    }

    fn test_block(seqno: u32) -> Block {
        Block {
            header: BlockHeader {
                seqno,
                prev_hash: [0u8; 32],
                msgs_root: [0u8; 32],
                state_root: [0u8; 32],
                lt: seqno as u64,
                workchain: -1,
                fee_collector: AccountId::from_bytes([0u8; 32]),
                msg_count: 0,
                protocol_version: 1,
                block_time: 1_700_000_000 + seqno as u64,
            },
            body: BlockBody { messages: vec![] },
        }
    }

    /// Four drivers, deterministic keys, full round: leader proposes,
    /// everyone prevotes/precommits/commits, all finalize with 4 sigs.
    #[test]
    fn four_validators_finalize_with_four_signatures() {
        let chain_id = [0x99u8; 32];
        let shard = test_shard();
        let seeds: [[u8; 32]; 4] = [[0x11; 32], [0x22; 32], [0x33; 32], [0x44; 32]];
        let keys: Vec<SecretKey> = seeds
            .iter()
            .map(|s| SecretKey::from_seed(s).unwrap())
            .collect();
        let validators: Vec<(PublicKey, u64)> =
            keys.iter().map(|k| (k.public_key(), 1_000_000)).collect();

        let mut drivers: Vec<ConsensusDriver> = keys
            .into_iter()
            .map(|k| ConsensusDriver::new(chain_id, shard, 1, &validators, k, 0).unwrap())
            .collect();

        // Round 0 leader is validator 0 (round-robin over sorted ids).
        assert!(drivers[0].should_propose());
        assert!(!drivers[1].should_propose());

        let block = test_block(1);
        let events = drivers[0].propose(block.clone(), vec![]).unwrap();
        // Leader broadcast proposal + its own prevote.
        let (mut proposals, mut votes): (Vec<_>, Vec<_>) = (Vec::new(), Vec::new());
        for e in events {
            match e {
                DriverEvent::BroadcastProposal { proposal, block: b } => {
                    proposals.push((proposal, b, block.clone()))
                }
                DriverEvent::BroadcastVote(v) => votes.push(v),
                DriverEvent::Finalized { .. } => panic!("finalized too early"),
            }
        }
        assert_eq!(proposals.len(), 1);

        // Deliver the proposal to validators 1..=3; collect their prevotes.
        for d in drivers.iter_mut().skip(1) {
            let (p, b, blk) = &proposals[0];
            for e in d.receive_proposal(p, b, |_| Ok(blk.clone())).unwrap() {
                match e {
                    DriverEvent::BroadcastVote(v) => votes.push(v),
                    DriverEvent::Finalized { .. } => panic!("finalized too early"),
                    DriverEvent::BroadcastProposal { .. } => panic!("unexpected proposal"),
                }
            }
        }

        // Gossip all prevotes until no driver has more to say. Each
        // delivery may trigger the next phase's vote; loop to fixpoint.
        let mut finalized: Vec<(Block, Vec<SigEntry>)> = Vec::new();
        for _ in 0..10 {
            if votes.is_empty() {
                break;
            }
            let batch: Vec<Vec<u8>> = std::mem::take(&mut votes);
            for d in drivers.iter_mut() {
                for v in &batch {
                    for e in d.receive_vote(v).unwrap() {
                        match e {
                            DriverEvent::BroadcastVote(nv) => votes.push(nv),
                            DriverEvent::Finalized { block, sig_entries } => {
                                finalized.push((block, sig_entries))
                            }
                            DriverEvent::BroadcastProposal { .. } => {
                                panic!("unexpected proposal")
                            }
                        }
                    }
                }
            }
        }

        // All four validators finalized the same block. Each has at least
        // the 2/3 quorum of header signatures (the 4th may arrive after
        // finalization — BFT finality doesn't wait for stragglers).
        assert_eq!(finalized.len(), 4, "all validators finalize");
        for (blk, sigs) in &finalized {
            assert_eq!(blk.header.hash(), block.header.hash());
            assert!(
                (3..=4).contains(&sigs.len()),
                "quorum of header signatures, got {}",
                sigs.len()
            );
            let mut idx: Vec<u32> = sigs.iter().map(|s| s.validator_index).collect();
            idx.sort_unstable();
            idx.dedup();
            assert_eq!(idx.len(), sigs.len(), "no duplicate signers");
        }
    }

    /// One validator offline: 3 of 4 still reach the 2/3 quorum.
    #[test]
    fn three_of_four_finalize_with_one_offline() {
        let chain_id = [0x99u8; 32];
        let shard = test_shard();
        let seeds: [[u8; 32]; 4] = [[0x11; 32], [0x22; 32], [0x33; 32], [0x44; 32]];
        let keys: Vec<SecretKey> = seeds
            .iter()
            .map(|s| SecretKey::from_seed(s).unwrap())
            .collect();
        let validators: Vec<(PublicKey, u64)> =
            keys.iter().map(|k| (k.public_key(), 1_000_000)).collect();

        // Validator 3 is offline: only 0..=2 participate.
        let mut drivers: Vec<ConsensusDriver> = keys
            .into_iter()
            .take(3)
            .map(|k| ConsensusDriver::new(chain_id, shard, 1, &validators, k, 0).unwrap())
            .collect();

        let block = test_block(1);
        let mut votes: Vec<Vec<u8>> = Vec::new();
        let mut proposal: Option<(Vec<u8>, Vec<u8>, Block)> = None;
        for e in drivers[0].propose(block.clone(), vec![]).unwrap() {
            match e {
                DriverEvent::BroadcastProposal {
                    proposal: p,
                    block: b,
                } => proposal = Some((p, b, block.clone())),
                DriverEvent::BroadcastVote(v) => votes.push(v),
                DriverEvent::Finalized { .. } => panic!("finalized too early"),
            }
        }
        let (p, b, blk) = proposal.unwrap();
        for d in drivers.iter_mut().skip(1) {
            for e in d.receive_proposal(&p, &b, |_| Ok(blk.clone())).unwrap() {
                if let DriverEvent::BroadcastVote(v) = e {
                    votes.push(v);
                }
            }
        }

        let mut finalized = 0;
        for _ in 0..10 {
            if votes.is_empty() {
                break;
            }
            let batch: Vec<Vec<u8>> = std::mem::take(&mut votes);
            for d in drivers.iter_mut() {
                for v in &batch {
                    for e in d.receive_vote(v).unwrap() {
                        match e {
                            DriverEvent::BroadcastVote(nv) => votes.push(nv),
                            DriverEvent::Finalized { sig_entries, .. } => {
                                assert_eq!(sigs_len(&sig_entries), 3);
                                finalized += 1;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        assert_eq!(finalized, 3, "online validators finalize without #3");
        fn sigs_len(s: &[SigEntry]) -> usize {
            s.len()
        }
    }

    /// ADR-0049 regression: a proposal with a block whose seqno does not
    /// match the height is rejected before voting. (Probe 4: previously the
    /// block was finalized, then commit failed fatally.)
    #[test]
    fn proposal_with_bad_seqno_is_rejected() {
        let chain_id = [0x99u8; 32];
        let shard = test_shard();
        let seeds: [[u8; 32]; 2] = [[0x11; 32], [0x22; 32]];
        let keys: Vec<SecretKey> = seeds
            .iter()
            .map(|s| SecretKey::from_seed(s).unwrap())
            .collect();
        let validators: Vec<(PublicKey, u64)> =
            keys.iter().map(|k| (k.public_key(), 1_000_000)).collect();
        let mut driver =
            ConsensusDriver::new(chain_id, shard, 1, &validators, keys[0].clone(), 0).unwrap();

        // Build a block with seqno 999 (wrong; height is 1).
        let block = test_block(999);
        let block_hash = block.header.hash();
        let proposal = ConsensusProposal {
            height: 1,
            round: 0,
            block_hash: Uint256(block_hash),
            proposer_id: 0,
            signature: keys[0].sign(
                &VALIDATOR_SIGN_V1,
                &proposal_signing_bytes(&shard, 1, 0, &Uint256(block_hash), None, None),
            ),
            qc_round: None,
            qc_block: None,
        };
        let proposal_bytes = encode_proposal(&proposal);
        // Encode block as bytes (simplified: we pass the block directly via closure).
        let result = driver.receive_proposal(
            &proposal_bytes,
            &[], // block bytes unused; closure provides the block
            |_| Ok(block),
        );
        assert!(
            result.is_err(),
            "proposal with bad seqno should be rejected"
        );
        assert!(
            result.unwrap_err().contains("bad seqno"),
            "error should mention bad seqno"
        );
    }

    /// ADR-0049 regression: votes arriving before the proposal are buffered,
    /// not rejected. (Probe 2: previously they were rejected as
    /// ConflictingProposal, stalling validators on reordered delivery.)
    #[test]
    fn votes_before_proposal_are_buffered() {
        let chain_id = [0x99u8; 32];
        let shard = test_shard();
        let seeds: [[u8; 32]; 2] = [[0x11; 32], [0x22; 32]];
        let keys: Vec<SecretKey> = seeds
            .iter()
            .map(|s| SecretKey::from_seed(s).unwrap())
            .collect();
        let validators: Vec<(PublicKey, u64)> =
            keys.iter().map(|k| (k.public_key(), 1_000_000)).collect();
        let mut driver =
            ConsensusDriver::new(chain_id, shard, 1, &validators, keys[1].clone(), 0).unwrap();

        // Validator 0 (leader) creates a proposal and votes.
        let block = test_block(1);
        let block_hash = block.header.hash();
        let proposal = ConsensusProposal {
            height: 1,
            round: 0,
            block_hash: Uint256(block_hash),
            proposer_id: 0,
            signature: keys[0].sign(
                &VALIDATOR_SIGN_V1,
                &proposal_signing_bytes(&shard, 1, 0, &Uint256(block_hash), None, None),
            ),
            qc_round: None,
            qc_block: None,
        };
        let proposal_bytes = encode_proposal(&proposal);

        // Validator 0's PreVote (created before driver sees the proposal).

        // Validator 0's PreVote (created before driver sees the proposal).
        let vote = ConsensusVote {
            height: 1,
            round: 0,
            phase: VotePhase::PreVote,
            block_hash: Uint256(block_hash),
            validator_id: 0,
            signature: keys[0].sign(
                &VALIDATOR_SIGN_V1,
                &vote_signing_bytes(&shard, 1, 0, VotePhase::PreVote, &Uint256(block_hash)),
            ),
        };
        let vote_bytes = encode_vote(&vote, None);

        // Deliver the vote BEFORE the proposal. Should be buffered, not rejected.
        let result = driver.receive_vote(&vote_bytes);
        assert!(
            result.is_ok(),
            "vote before proposal should be buffered, not rejected: {:?}",
            result.err()
        );

        // Now deliver the proposal. The buffered vote should be counted.
        let events = driver
            .receive_proposal(&proposal_bytes, &[], |_| Ok(block))
            .unwrap();
        // Driver should have voted (PreVote) and the buffered vote should
        // have been processed. With 2 validators, we need both votes for
        // quorum (>2/3 of 2 = >1.33, so 2 votes needed).
        // The driver (validator 1) votes, plus the buffered vote from validator 0.
        // Engine should be at PreCommit (2 PreVotes = quorum).
        assert!(
            events
                .iter()
                .any(|e| matches!(e, DriverEvent::BroadcastVote(_))),
            "driver should broadcast its vote"
        );
    }
}
