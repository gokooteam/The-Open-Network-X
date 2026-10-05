//! Phase 2 regression: genesis construction must be byte-identical across
//! separate OS processes. (In-process determinism is necessary but not
//! sufficient — per-process hash seeds and allocator behavior only show up
//! across process boundaries, which is exactly the replay scenario: two
//! nodes, two runs, one chain.)

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn fresh_temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "onx-genesis-xproc-{}-{}-{}",
        tag,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_genesis_binary(cwd: &std::path::Path) -> Vec<u8> {
    let bin = env!("CARGO_BIN_EXE_onx-genesis");
    let config = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/genesis.toml");
    let status = Command::new(bin)
        .args(["--config", config, "--out", "gen"])
        .current_dir(cwd)
        .status()
        .expect("failed to spawn onx-genesis binary");
    assert!(status.success(), "onx-genesis binary exited with {status}");
    fs::read(cwd.join("gen").join("genesis.boc")).expect("genesis.boc must exist after run")
}

#[test]
fn genesis_bytes_identical_across_processes() {
    let dir_a = fresh_temp("a");
    let dir_b = fresh_temp("b");

    let bytes_a = run_genesis_binary(&dir_a);
    let bytes_b = run_genesis_binary(&dir_b);

    assert_eq!(
        bytes_a,
        bytes_b,
        "genesis construction diverged across processes ({} vs {} bytes)",
        bytes_a.len(),
        bytes_b.len()
    );
    assert!(
        bytes_a.starts_with(b"ONXG"),
        "genesis output must be the canonical binary document"
    );

    let _ = fs::remove_dir_all(&dir_a);
    let _ = fs::remove_dir_all(&dir_b);
}
