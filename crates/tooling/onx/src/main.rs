//! `onx` — the deterministic-replay command.
//!
//! ```text
//! onx replay --genesis genesis.toml --blocks ./blocks/ [--data-dir ./onx-data]
//! ```
//!
//! Loads the canonical genesis document (Phase 2), reads block files from
//! the directory in lexicographic filename order, executes each through the
//! pure STF (Phase 3), and persists via the atomic store (Phase 4).
//!
//! Fail-closed throughout: a corrupt block file, an invalid block, or a
//! fork aborts the replay with a non-zero exit code and never advances the
//! head past the last fully-committed block. Re-running over
//! already-committed blocks is a no-op (idempotent resume).

use clap::{Parser, Subcommand};
use onx::blockfile::decode_block_file;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(name = "onx", version = "0.1.0", about = "ONX deterministic replay")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Deterministically replay a chain: genesis + block files → state roots.
    Replay {
        /// Path to the human-readable genesis config (TOML).
        #[arg(long)]
        genesis: PathBuf,
        /// Directory of block files (`block-00000001.blk`, …), replayed in
        /// lexicographic filename order.
        #[arg(long)]
        blocks: PathBuf,
        /// Directory holding the chain database (`chain.redb`).
        /// Re-running against the same data dir resumes idempotently.
        #[arg(long, default_value = "onx-data")]
        data_dir: PathBuf,
    },
}

fn hex(bytes: &[u8; 32]) -> String {
    hex::encode(bytes)
}

fn replay(genesis_path: &Path, blocks_dir: &Path, data_dir: &Path) -> Result<(), String> {
    // 1. Genesis: parse the human config, build the canonical document.
    let config = onx_genesis::parse_config(genesis_path).map_err(|e| format!("genesis: {e}"))?;
    let doc = onx_genesis::build_genesis_document(&config).map_err(|e| format!("genesis: {e}"))?;
    let chain_id = doc.genesis_hash();
    println!("chain_id={}", hex(&chain_id));

    // 2. Atomic store. init_genesis is idempotent for the same genesis and
    //    refuses a database initialized with a different one.
    let store = onx_storage::ChainStore::open(data_dir.join("chain.redb"))
        .map_err(|e| format!("storage: {e}"))?;
    store
        .init_genesis(&doc)
        .map_err(|e| format!("genesis init: {e}"))?;

    let mut state = store
        .load_state()
        .map_err(|e| format!("storage: {e}"))?
        .ok_or_else(|| "storage: no state after genesis init".to_string())?;
    let genesis_root = state.state_root().map_err(|e| format!("state root: {e}"))?;
    println!("genesis_root={}", hex(&genesis_root));

    // 3. Block files, in deterministic lexicographic filename order.
    let mut files = Vec::new();
    let entries = std::fs::read_dir(blocks_dir)
        .map_err(|e| format!("blocks dir {}: {e}", blocks_dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("blocks dir: {e}"))?;
        let path = entry.path();
        let ftype = entry.file_type().map_err(|e| format!("blocks dir: {e}"))?;
        if !ftype.is_file() {
            return Err(format!(
                "blocks dir: not a regular file: {}",
                path.display()
            ));
        }
        files.push(path);
    }
    files.sort();

    // 4. Execute + persist, one block at a time.
    for path in &files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bytes = std::fs::read(path).map_err(|e| format!("{name}: read: {e}"))?;
        let block = decode_block_file(&bytes).map_err(|e| format!("{name}: {e}"))?;
        let header_hash = block.header.hash();

        // Idempotent resume: already committed → skip; conflicting hash → fork.
        match store
            .block_hash_for_seqno(block.header.seqno)
            .map_err(|e| format!("storage: {e}"))?
        {
            Some(existing) if existing == header_hash => {
                println!("seqno={} status=skipped", block.header.seqno);
                continue;
            }
            Some(_) => {
                return Err(format!(
                    "{name}: fork detected at seqno {}: committed hash differs",
                    block.header.seqno
                ));
            }
            None => {}
        }

        // commit_block runs the pure STF first (fail-closed validation of
        // seqno, prev-hash, workchain, lt, msgs_root, claimed state root and
        // every external message), then persists everything atomically.
        store
            .commit_block(&state, &block)
            .map_err(|e| format!("{name}: rejected: {e}"))?;
        state = store
            .load_state()
            .map_err(|e| format!("storage: {e}"))?
            .ok_or_else(|| "storage: no state after commit".to_string())?;
        let root = state.state_root().map_err(|e| format!("state root: {e}"))?;
        println!(
            "seqno={} block={} root={}",
            block.header.seqno,
            hex(&header_hash),
            hex(&root)
        );
    }

    let final_root = state.state_root().map_err(|e| format!("state root: {e}"))?;
    println!("final_seqno={}", state.seqno);
    println!("final_block_hash={}", hex(&state.last_hash));
    println!("final_state_root={}", hex(&final_root));
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Replay {
            genesis,
            blocks,
            data_dir,
        } => replay(&genesis, &blocks, &data_dir),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("onx replay failed: {err}");
            ExitCode::from(1)
        }
    }
}
