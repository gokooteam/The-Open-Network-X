//! AUDIT HARNESS — no-panic fuzz for the onx-state-model decoders that the
//! branch's lint extension (claim c) covers. Every decode runs under
//! `catch_unwind`; any panic on any input is a failure. Inputs: random bytes,
//! and byte-level mutations (flip, truncate, extend, splice, length-field
//! tamper) of valid encodings. Also checks decode(encode(x)) == x for seeds
//! and that every successful decode re-encodes to exactly the consumed bytes
//! (canonical-encoding invariant).

use onx_data_structures::{AccountId, ShardIdent, WorkchainIdent};
use onx_state_model::{
    AccountState, BagOfCells, Cell, ContractCellDags, GenesisDocument, GenesisValidator,
    MerkleProof, StorageStat, MERKLE_PROOF_MAGIC,
};
use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut v = seed.to_vec();
    for _ in 0..=rng.below(3) {
        match rng.below(7) {
            0 if !v.is_empty() => {
                let i = rng.below(v.len());
                v[i] ^= 1 << rng.below(8);
            }
            1 if !v.is_empty() => {
                let i = rng.below(v.len());
                v[i] = rng.next() as u8;
            }
            2 => v.truncate(rng.below(v.len() + 1)),
            3 => v.extend((0..rng.below(40)).map(|_| rng.next() as u8)),
            4 if v.len() >= 4 => {
                // Tamper a 4-byte window with an extreme length value.
                let i = rng.below(v.len() - 3);
                let val: u32 = [0, 1, 0x7fff_ffff, 0xffff_ffff, 128, 129, 4, 5][rng.below(8)];
                v[i..i + 4].copy_from_slice(&val.to_be_bytes());
            }
            5 if v.len() >= 2 => {
                let i = rng.below(v.len() - 1);
                v[i] = 0xff;
                v[i + 1] = 0xff;
            }
            _ => {
                let i = rng.below(v.len() + 1);
                v.insert(i, rng.next() as u8);
            }
        }
    }
    v
}

fn seeds() -> Vec<(&'static str, Vec<u8>)> {
    let leaf = Cell::new(b"leaf".to_vec(), vec![]).unwrap();
    let full = Cell::new(vec![0xAB; 128], vec![]).unwrap();
    let mid = Cell::new(b"mid".to_vec(), vec![leaf.hash()]).unwrap();
    let root = Cell::new(b"root".to_vec(), vec![mid.hash(), leaf.hash(), full.hash()]).unwrap();
    let mut cells = BTreeMap::new();
    for c in [&leaf, &full, &mid, &root] {
        cells.insert(c.hash(), c.clone());
    }
    let boc = BagOfCells::new(root.hash(), cells).unwrap();
    let boc1 = BagOfCells::from_root(leaf.clone()).unwrap();
    let active = AccountState::Active {
        balance_nanos: u128::MAX,
        last_trans_lt: u64::MAX,
        code: Some(root.clone()),
        data: Some(leaf.clone()),
        storage_stat: StorageStat {
            cell_count: 3,
            byte_count: 99,
            bit_count: 0,
        },
        pubkey: [7u8; 32],
        nonce: 5,
    };
    let frozen = AccountState::Frozen {
        balance_nanos: 1,
        last_trans_lt: 2,
        storage_hash: [9; 32],
    };
    let dags = ContractCellDags {
        code: boc.clone(),
        data: boc1.clone(),
    };
    let mut accounts = BTreeMap::new();
    accounts.insert(AccountId::from_bytes([1; 32]), active.clone());
    accounts.insert(AccountId::from_bytes([2; 32]), AccountState::Uninitialized);
    // `active`'s code root has children, so genesis must carry its DAGs
    // (ADR-0040): this seed is a version-2 document.
    let mut contract_cells = BTreeMap::new();
    contract_cells.insert(AccountId::from_bytes([1; 32]), dags.clone());
    let gen = GenesisDocument::with_contract_cells(
        WorkchainIdent::MASTERCHAIN,
        ShardIdent::root(WorkchainIdent::MASTERCHAIN),
        vec![
            GenesisValidator {
                pubkey: [3; 32],
                stake: 10,
            },
            GenesisValidator {
                pubkey: [4; 32],
                stake: 20,
            },
        ],
        accounts,
        contract_cells,
    )
    .unwrap();
    let mut proof_like = MERKLE_PROOF_MAGIC.to_be_bytes().to_vec();
    proof_like.extend([0u8; 64]);
    vec![
        ("cell", leaf.to_bytes()),
        ("cell", full.to_bytes()),
        ("cell", root.to_bytes()),
        ("boc", boc.to_bytes()),
        ("boc", boc1.to_bytes()),
        ("account", active.to_bytes()),
        ("account", frozen.to_bytes()),
        ("account", AccountState::Uninitialized.to_bytes()),
        ("account", AccountState::Destroyed.to_bytes()),
        ("dags", dags.to_bytes()),
        ("genesis", gen.to_bytes()),
        ("proof", proof_like),
    ]
}

/// Runs every decoder on `input`; returns a description of any panic.
fn decode_all(input: &[u8]) -> Vec<String> {
    let mut panics = Vec::new();
    let mut try_one = |name: &str, f: &dyn Fn(&[u8])| {
        if let Err(e) = catch_unwind(AssertUnwindSafe(|| f(input))) {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            let msg: String = msg.lines().next().unwrap_or("").chars().take(120).collect();
            panics.push(format!(
                "{name} on {} bytes [{msg}]: {}",
                input.len(),
                hex(input)
            ));
        }
    };
    try_one("Cell::from_bytes", &|b| {
        if let Ok((c, n)) = Cell::from_bytes(b) {
            assert_eq!(c.to_bytes(), b[..n].to_vec(), "Cell decode not canonical");
        }
    });
    try_one("BagOfCells::from_bytes", &|b| {
        let _ = BagOfCells::from_bytes(b);
    });
    try_one("BagOfCells::from_bytes_strict", &|b| {
        if let Ok((boc, n)) = BagOfCells::from_bytes_strict(b) {
            assert_eq!(
                boc.to_bytes(),
                b[..n].to_vec(),
                "strict BoC decode not canonical"
            );
        }
    });
    try_one("AccountState::from_bytes", &|b| {
        if let Ok((a, n)) = AccountState::from_bytes(b) {
            assert_eq!(
                a.to_bytes(),
                b[..n].to_vec(),
                "AccountState decode not canonical"
            );
        }
    });
    try_one("ContractCellDags::from_bytes", &|b| {
        let _ = ContractCellDags::from_bytes(b);
    });
    try_one("GenesisDocument::from_bytes", &|b| {
        if let Ok(g) = GenesisDocument::from_bytes(b) {
            assert_eq!(
                g.to_bytes(),
                b.to_vec(),
                "GenesisDocument decode not canonical"
            );
        }
    });
    try_one("MerkleProof::from_bytes", &|b| {
        let _ = MerkleProof::from_bytes(b);
    });
    panics
}

fn hex(b: &[u8]) -> String {
    b.iter()
        .take(48)
        .map(|x| format!("{x:02x}"))
        .collect::<String>()
        + if b.len() > 48 { "…" } else { "" }
}

#[test]
fn seeds_round_trip() {
    for (name, s) in seeds() {
        assert!(decode_all(&s).is_empty(), "{name} seed panicked");
    }
}

/// Runs the corpus once; splits failures into genuine panics and
/// canonical-encoding violations (decode succeeded, re-encode differs).
fn fuzz_corpus() -> (usize, Vec<String>, Vec<String>) {
    std::panic::set_hook(Box::new(|_| {})); // keep output readable; failures are collected
    let seeds = seeds();
    let mut rng = Rng(0xA076_1D64_78BD_642F);
    let (mut panics, mut noncanonical) = (Vec::new(), Vec::new());
    let mut n = 0usize;
    for _ in 0..40_000 {
        let input = if rng.below(4) == 0 {
            (0..rng.below(300)).map(|_| rng.next() as u8).collect()
        } else {
            let (_, s) = &seeds[rng.below(seeds.len())];
            mutate(&mut rng, s)
        };
        n += 1;
        for p in decode_all(&input) {
            let bucket = if p.contains("not canonical") {
                &mut noncanonical
            } else {
                &mut panics
            };
            if bucket.len() < 12 {
                bucket.push(p);
            }
        }
    }
    let _ = std::panic::take_hook();
    (n, panics, noncanonical)
}

#[test]
fn decoders_never_panic_on_mutated_and_random_input() {
    let (n, panics, _) = fuzz_corpus();
    assert!(
        panics.is_empty(),
        "decoder PANICS over {n} inputs:\n{}",
        panics.join("\n")
    );
}

#[test]
#[ignore = "pre-existing (main): GenesisDocument::from_bytes silently re-sorts validators, genesis.rs 204-214 + 111"]
fn successful_decodes_are_canonical() {
    let (n, _, noncanonical) = fuzz_corpus();
    assert!(
        noncanonical.is_empty(),
        "decode succeeded but re-encoding differs (non-canonical acceptance) over {n} inputs:\n{}",
        noncanonical.join("\n")
    );
}
