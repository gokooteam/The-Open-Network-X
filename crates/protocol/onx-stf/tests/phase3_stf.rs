//! Phase 3 regression tests: block bodies and the pure state transition function.
//!
//! Covers: happy-path transfers with fee split accounting, every header
//! validation rule, every transaction rejection rule, in-process
//! determinism over randomized block sequences, and cross-process
//! determinism via the `onx-stf-probe` binary (two OS processes must print
//! byte-identical output).

use onx_data_structures::{AccountId, ShardIdent, WorkchainIdent};
use onx_primitives::{domain_hash, DomainTag, SecretKey};
use onx_state_model::{
    derive_account_id, AccountState, GenesisDocument, GenesisValidator, StorageStat,
};
use onx_stf::{
    apply_block, propose_block, txs_root, BlockBody, BlockHeader, State, StfError, Transaction,
};
use std::collections::BTreeMap;
use std::process::Command;

/// Domain tag for the phase-3 tests' deterministic signing keys.
///
/// Test-only: the preimage is public, so these keys are not secret.
const TEST_KEY_V1: DomainTag = DomainTag::from_ascii("ONX_TEST3_KEY_V1");

fn test_secret(id: &AccountId) -> SecretKey {
    let seed = domain_hash(&TEST_KEY_V1, &id.to_bytes());
    SecretKey::from_seed(&seed).expect("domain hash output is a valid seed")
}

/// Signs transactions for a test, tracking per-account nonces exactly the
/// way the STF expects them: the k-th transaction from an account carries
/// that account's k-th nonce (genesis accounts start at 0).
#[derive(Default)]
struct TxSigner {
    nonces: BTreeMap<AccountId, u64>,
}

impl TxSigner {
    fn sign(&mut self, from: AccountId, to: AccountId, amount: u128, fee: u128) -> Transaction {
        let nonce = self.next_nonce(from);
        Transaction::new_signed(from, to, amount, fee, nonce, &test_secret(&from))
    }

    fn next_nonce(&mut self, from: AccountId) -> u64 {
        let nonce = self.nonces.get(&from).copied().unwrap_or(0);
        self.nonces.insert(from, nonce + 1);
        nonce
    }
}

fn active(balance_nanos: u128, id: &AccountId) -> AccountState {
    AccountState::Active {
        balance_nanos,
        last_trans_lt: 0,
        code: None,
        data: None,
        storage_stat: StorageStat {
            cell_count: 0,
            byte_count: 0,
        },
        pubkey: test_secret(id).public_key().encode(),
        nonce: 0,
    }
}

fn test_genesis() -> (State, Vec<AccountId>, AccountId) {
    let workchain = WorkchainIdent::new(0);
    let shard = ShardIdent::root(workchain);
    let validators = vec![GenesisValidator {
        pubkey: [9u8; 32],
        stake: 1,
    }];
    let mut accounts = BTreeMap::new();
    let alice = derive_account_id("test-alice");
    let bob = derive_account_id("test-bob");
    let carol = derive_account_id("test-carol");
    accounts.insert(alice, active(10_000_000, &alice));
    accounts.insert(bob, active(5_000_000, &bob));
    accounts.insert(carol, active(0, &carol));
    let collector = derive_account_id("test-collector");
    let doc = GenesisDocument::new(workchain, shard, validators, accounts).unwrap();
    (
        State::from_genesis(&doc),
        vec![alice, bob, carol],
        collector,
    )
}

fn balance_of(state: &State, id: &AccountId) -> u128 {
    state.tree.get(id).map(|s| s.balance_nanos()).unwrap_or(0)
}

#[test]
fn stf_happy_path_transfer_with_fee_split() {
    let (state, ids, collector) = test_genesis();
    let (alice, bob) = (ids[0], ids[1]);

    // Alice sends Bob 1_000_000 with fee 1_000. Fee split is 50/50 per
    // onx-economics: 500 burned, 500 to the collector.
    let mut signer = TxSigner::default();
    let tx = signer.sign(alice, bob, 1_000_000, 1_000);
    let tx_hash = tx.hash();
    let block = propose_block(&state, vec![tx], 1, collector).unwrap();
    let (next, receipts) = apply_block(&state, &block).unwrap();

    assert_eq!(balance_of(&next, &alice), 10_000_000 - 1_001_000);
    assert_eq!(balance_of(&next, &bob), 5_000_000 + 1_000_000);
    assert_eq!(balance_of(&next, &collector), 500);
    assert_eq!(next.seqno, 1);
    assert_eq!(next.last_lt, 1);
    assert_eq!(next.last_hash, block.header.hash());

    assert_eq!(receipts.0.len(), 1);
    let r = &receipts.0[0];
    assert_eq!(r.tx_hash, tx_hash);
    assert_eq!(r.fee_burned_nanos, 500);
    assert_eq!(r.fee_validator_nanos, 500);
    assert_eq!(r.sender_balance_after, 10_000_000 - 1_001_000);
    assert_eq!(r.receiver_balance_after, 6_000_000);

    // Total supply decreased by exactly the burned fee: nothing is minted.
    let supply_before: u128 = ids.iter().map(|id| balance_of(&state, id)).sum();
    let supply_after: u128 =
        ids.iter().map(|id| balance_of(&next, id)).sum::<u128>() + balance_of(&next, &collector);
    assert_eq!(supply_before - supply_after, 500);
}

#[test]
fn stf_creates_receiver_account_on_first_transfer() {
    let (state, ids, collector) = test_genesis();
    let (alice, carol) = (ids[0], ids[2]);
    assert_eq!(balance_of(&state, &carol), 0);

    let mut signer = TxSigner::default();
    let tx = signer.sign(alice, carol, 250_000, 0);
    let block = propose_block(&state, vec![tx], 1, collector).unwrap();
    let (next, _) = apply_block(&state, &block).unwrap();
    assert_eq!(balance_of(&next, &carol), 250_000);
    // Zero fee: no dust account created for the collector.
    assert!(next.tree.get(&collector).is_none());
}

#[test]
fn stf_self_transfer_nets_to_fee_only() {
    let (state, ids, collector) = test_genesis();
    let alice = ids[0];
    let mut signer = TxSigner::default();
    let tx = signer.sign(alice, alice, 1_000_000, 400);
    let block = propose_block(&state, vec![tx], 1, collector).unwrap();
    let (next, _) = apply_block(&state, &block).unwrap();
    // Debited amount+fee, credited amount: net -fee.
    assert_eq!(balance_of(&next, &alice), 10_000_000 - 400);
}

#[test]
fn stf_rejects_bad_seqno() {
    let (state, _, collector) = test_genesis();
    let block = propose_block(&state, vec![], 1, collector).unwrap();
    let mut bad = block.clone();
    bad.header.seqno = 99;
    assert!(matches!(
        apply_block(&state, &bad),
        Err(StfError::BadSeqno {
            expected: 1,
            got: 99
        })
    ));
    // And the same block cannot be applied twice.
    let (next, _) = apply_block(&state, &block).unwrap();
    assert!(matches!(
        apply_block(&next, &block),
        Err(StfError::BadSeqno { .. })
    ));
}

#[test]
fn stf_rejects_bad_prev_hash() {
    let (state, _, collector) = test_genesis();
    let block = propose_block(&state, vec![], 1, collector).unwrap();
    let mut bad = block;
    bad.header.prev_hash = [0xAA; 32];
    assert!(matches!(
        apply_block(&state, &bad),
        Err(StfError::PrevHashMismatch)
    ));
}

#[test]
fn stf_rejects_lt_regression() {
    let (state, _, collector) = test_genesis();
    // lt must be strictly greater than last_lt (0 at genesis).
    assert!(matches!(
        propose_block(&state, vec![], 0, collector),
        Err(StfError::LogicalTimeRegression { .. })
    ));
    let block = propose_block(&state, vec![], 1, collector).unwrap();
    let (next, _) = apply_block(&state, &block).unwrap();
    assert!(matches!(
        propose_block(&next, vec![], 1, collector),
        Err(StfError::LogicalTimeRegression { .. })
    ));
}

#[test]
fn stf_rejects_tampered_body_and_header() {
    let (state, ids, collector) = test_genesis();
    let mut signer = TxSigner::default();
    let tx = signer.sign(ids[0], ids[1], 100, 0);
    let block = propose_block(&state, vec![tx], 1, collector).unwrap();

    // Mutate the body after proposing: txs_root no longer matches.
    let mut tampered = block.clone();
    tampered.body.transactions[0].amount_nanos = 101;
    assert!(matches!(
        apply_block(&state, &tampered),
        Err(StfError::TxsRootMismatch { .. })
    ));

    // Lie about the resulting state root.
    let mut lying = block.clone();
    lying.header.state_root = [0xBB; 32];
    assert!(matches!(
        apply_block(&state, &lying),
        Err(StfError::StateRootMismatch { .. })
    ));

    // The honest block still applies.
    assert!(apply_block(&state, &block).is_ok());
}

#[test]
fn stf_rejects_invalid_transactions() {
    let (state, ids, collector) = test_genesis();
    let (alice, bob) = (ids[0], ids[1]);
    let stranger = derive_account_id("test-stranger");

    // Zero amount.
    let mut signer = TxSigner::default();
    let bad: Vec<Transaction> = vec![signer.sign(alice, bob, 0, 10)];
    assert!(matches!(
        propose_block(&state, bad, 1, collector),
        Err(StfError::ZeroAmount)
    ));

    // Insufficient funds (alice has 10M).
    let mut bad_signer = TxSigner::default();
    let bad = vec![bad_signer.sign(alice, bob, 9_999_999, 2)];
    assert!(matches!(
        propose_block(&state, bad, 1, collector),
        Err(StfError::InsufficientFunds { .. })
    ));

    // Unknown sender.
    let mut bad_signer = TxSigner::default();
    let bad = vec![bad_signer.sign(stranger, bob, 1, 0)];
    assert!(matches!(
        propose_block(&state, bad, 1, collector),
        Err(StfError::SenderNotSpendable(_))
    ));

    // Empty block is valid (no-op block, still advances the chain).
    let block = propose_block(&state, vec![], 1, collector).unwrap();
    let (next, receipts) = apply_block(&state, &block).unwrap();
    assert_eq!(next.seqno, 1);
    assert!(receipts.0.is_empty());
    assert_eq!(next.state_root().unwrap(), state.state_root().unwrap());
}

/// Same account touched twice in one block: allowed, ordered by tx index.
#[test]
fn stf_allows_intra_block_multi_touch() {
    let (state, ids, collector) = test_genesis();
    let (alice, bob) = (ids[0], ids[1]);
    let mut signer = TxSigner::default();
    let txs = vec![
        signer.sign(alice, bob, 1_000_000, 0),
        signer.sign(alice, bob, 2_000_000, 0),
        signer.sign(bob, alice, 500_000, 100),
    ];
    let block = propose_block(&state, txs, 1, collector).unwrap();
    let (next, receipts) = apply_block(&state, &block).unwrap();
    assert_eq!(receipts.0.len(), 3);
    // Alice: 10M - 1M - 2M + 500k = 7_500_000 (the 100 fee is paid by Bob).
    assert_eq!(balance_of(&next, &alice), 7_500_000);
    // Bob: 5M + 1M + 2M - 500k - 100(fee) = 7_499_900
    assert_eq!(balance_of(&next, &bob), 7_499_900);
    // Collector got half the 100 fee.
    assert_eq!(balance_of(&next, &collector), 50);
}

#[test]
fn stf_header_commits_to_ordered_set() {
    // Same transactions in different order => different txs_root.
    let (state, ids, collector) = test_genesis();
    let mut signer = TxSigner::default();
    let tx1 = signer.sign(ids[0], ids[1], 1, 0);
    let mut signer = TxSigner::default();
    let tx2 = signer.sign(ids[1], ids[0], 1, 0);
    let r1 = txs_root(&[tx1.clone(), tx2.clone()]);
    let r2 = txs_root(&[tx2.clone(), tx1.clone()]);
    assert_ne!(r1, r2);

    // Header round-trips through canonical bytes.
    let block = propose_block(&state, vec![tx1.clone(), tx2.clone()], 1, collector).unwrap();
    let bytes = block.header.to_bytes();
    assert_eq!(bytes.len(), onx_stf::block::BLOCK_HEADER_BYTE_LEN);
    let back = BlockHeader::from_bytes(&bytes).unwrap();
    assert_eq!(back, block.header);
    assert_eq!(back.hash(), block.header.hash());

    // Transaction round-trips too. V3 transactions are variable-length
    // (kind byte + length-prefixed message); a plain transfer is
    // 109 + 64 = 173 bytes.
    let tx_bytes = tx1.to_bytes();
    assert_eq!(tx_bytes.len(), 173);
    assert_eq!(Transaction::from_bytes(&tx_bytes).unwrap(), tx1);
}

/// Deterministic xorshift64* (mirrors the probe binary; no rand dependency).
struct XorShift64(u64);
impl XorShift64 {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
}

/// Randomized block sequences applied twice from the same genesis must
/// produce identical roots, hashes, and receipts.
#[test]
fn stf_randomized_sequences_deterministic_in_process() {
    fn run_once() -> (State, Vec<onx_stf::Receipts>) {
        let (genesis_state, ids, collector) = test_genesis();
        let mut signer = TxSigner::default();
        let mut state = genesis_state;
        let mut all_receipts = Vec::new();
        let mut rng = XorShift64(0xDEAD_BEEF_CAFE_1234);
        let mut mirror: BTreeMap<AccountId, u128> = BTreeMap::new();
        for id in &ids {
            mirror.insert(*id, balance_of(&state, id));
        }
        mirror.insert(collector, 0);
        for b in 1..=15u64 {
            let n = 1 + (rng.next() % 6) as usize;
            let mut txs = Vec::new();
            for _ in 0..n {
                let from = ids[(rng.next() as usize) % ids.len()];
                let to = ids[(rng.next() as usize) % ids.len()];
                let bal = mirror[&from];
                let fee = (rng.next() % 500) as u128;
                let spendable = bal.saturating_sub(fee);
                if spendable == 0 {
                    continue;
                }
                let amount = 1 + (rng.next() as u128 % spendable);
                let (burned, val_fee) = onx_economics::split_transaction_fee(fee);
                let _ = burned;
                mirror.insert(from, bal - amount - fee);
                *mirror.entry(to).or_insert(0) += amount;
                *mirror.entry(collector).or_insert(0) += val_fee;
                let nonce = signer.next_nonce(from);
                txs.push(Transaction::new_signed(
                    from,
                    to,
                    amount,
                    fee,
                    nonce,
                    &test_secret(&from),
                ));
            }
            let block = propose_block(&state, txs, b, collector).unwrap();
            let (next, receipts) = apply_block(&state, &block).unwrap();
            all_receipts.push(receipts);
            state = next;
        }
        (state, all_receipts)
    }

    let (s1, r1) = run_once();
    let (s2, r2) = run_once();
    assert_eq!(s1.state_root(), s2.state_root());
    assert_eq!(s1.last_hash, s2.last_hash);
    assert_eq!(r1, r2);
}

/// Cross-process determinism: the probe binary run in two separate OS
/// processes must print byte-identical output. (Per-process hash seeds and
/// allocator behavior only show up across process boundaries — exactly the
/// replay scenario.)
#[test]
fn stf_cross_process_determinism() {
    let bin = env!("CARGO_BIN_EXE_onx-stf-probe");
    let out_a = Command::new(bin).output().expect("probe must run");
    let out_b = Command::new(bin).output().expect("probe must run");
    assert!(out_a.status.success(), "probe run A failed");
    assert!(out_b.status.success(), "probe run B failed");
    assert_eq!(
        out_a.stdout, out_b.stdout,
        "probe output diverged across processes"
    );
    let text = String::from_utf8(out_a.stdout).unwrap();
    assert!(text.contains("final_state_root="));
    assert!(text.contains("final_seqno=25"));
}

/// An empty-body block still commits to a well-defined txs_root.
#[test]
fn stf_empty_body_has_stable_txs_root() {
    let e1 = txs_root(&[]);
    let e2 = BlockBody {
        transactions: vec![],
    }
    .txs_root();
    assert_eq!(e1, e2);
    assert_ne!(e1, [0u8; 32]);
}
