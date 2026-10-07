//! AUDIT HARNESS — no-panic fuzz for the onx-stf / onx-storage decoders
//! (claim c's crates). Structured seeds built from the documented wire
//! layouts, then mutated; every decode runs under `catch_unwind`.

use onx_stf::{BlockHeader, ExternalMessage, EXT_BODY_PREFIX_LEN};
use onx_storage::decode_body;
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
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// chain_id(32) from(32) nonce(8) kind(1) to(32) amount(16) fee(16)
/// msg_len(4) message(msg_len) pubkey(32) || signature(64)
fn ext_message(rng: &mut Rng) -> Vec<u8> {
    let msg_len = [0usize, 1, 7, 64][rng.below(4)];
    let mut v = rng.bytes(72);
    v.push(rng.below(4) as u8);
    v.extend(rng.bytes(64));
    v.extend((msg_len as u32).to_be_bytes());
    v.extend(rng.bytes(msg_len + 32 + 64));
    assert_eq!(v.len(), EXT_BODY_PREFIX_LEN + msg_len + 32 + 64);
    v
}

fn body(rng: &mut Rng) -> Vec<u8> {
    let n = rng.below(4);
    let mut v = (n as u32).to_be_bytes().to_vec();
    for _ in 0..n {
        let m = ext_message(rng);
        v.extend((m.len() as u32).to_be_bytes());
        v.extend(m);
    }
    v
}

fn mutate(rng: &mut Rng, mut v: Vec<u8>) -> Vec<u8> {
    for _ in 0..=rng.below(3) {
        match rng.below(5) {
            0 if !v.is_empty() => {
                let i = rng.below(v.len());
                v[i] ^= 1 << rng.below(8);
            }
            1 => v.truncate(rng.below(v.len() + 1)),
            2 => {
                let k = rng.below(16);
                v.extend(rng.bytes(k));
            }
            3 if v.len() >= 4 => {
                let i = rng.below(v.len() - 3);
                let val: u32 = [0, 1, 0xffff_ffff, 0x7fff_ffff, 0x1_0000][rng.below(5)];
                v[i..i + 4].copy_from_slice(&val.to_be_bytes());
            }
            _ => {}
        }
    }
    v
}

#[test]
fn stf_and_storage_decoders_never_panic() {
    std::panic::set_hook(Box::new(|_| {}));
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut panics = Vec::new();
    for i in 0..60_000 {
        let input = match i % 3 {
            0 => mutate(&mut rng, ext_message(&mut Rng(i as u64 + 1))),
            1 => mutate(&mut rng, body(&mut Rng(i as u64 + 7))),
            _ => {
                let n = [0usize, 147, 148, 149, rng.below(400)][rng.below(5)];
                let raw = rng.bytes(n);
                mutate(&mut rng, raw)
            }
        };
        for (name, r) in [
            (
                "ExternalMessage::from_bytes",
                catch_unwind(AssertUnwindSafe(|| {
                    let _ = ExternalMessage::from_bytes(&input);
                })),
            ),
            (
                "ExternalMessage::body_from_bytes",
                catch_unwind(AssertUnwindSafe(|| {
                    let _ = ExternalMessage::body_from_bytes(&input);
                })),
            ),
            (
                "BlockHeader::from_bytes",
                catch_unwind(AssertUnwindSafe(|| {
                    let _ = BlockHeader::from_bytes(&input);
                })),
            ),
            (
                "decode_body",
                catch_unwind(AssertUnwindSafe(|| {
                    let _ = decode_body(&input);
                })),
            ),
        ] {
            if r.is_err() && panics.len() < 10 {
                panics.push(format!("{name} panicked on {} bytes", input.len()));
            }
        }
    }
    let _ = std::panic::take_hook();
    assert!(panics.is_empty(), "{}", panics.join("\n"));
}
