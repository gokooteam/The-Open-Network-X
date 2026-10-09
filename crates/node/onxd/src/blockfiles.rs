//! Atomic block-file output, shared by the producer and the follower.
//!
//! Block files are written atomically (temp file + rename) AFTER the
//! database commit. A crash in between leaves a gap in `blocks/`; the
//! database is the source of truth and any missing (or torn) block file
//! is regenerated from it at startup. Both loops run the same recovery.

use onx::blockfile::{block_file_name, decode_block_file, encode_block_file};
use onx_stf::block::Block;
use onx_storage::ChainStore;
use std::fs;
use std::path::{Path, PathBuf};

fn temp_block_path(blocks_dir: &Path, seqno: u32) -> PathBuf {
    blocks_dir.join(format!(
        ".tmp-block-{:08}-{}.blk",
        seqno,
        std::process::id()
    ))
}

/// Atomically write a block file: write + fsync a temp file in the same
/// directory, then rename over the target. A crash mid-write leaves only
/// a temp file, which [`sweep_temp_block_files`] removes at startup —
/// never a torn `.blk` file.
pub(crate) fn atomic_write_block_file(
    blocks_dir: &Path,
    seqno: u32,
    bytes: &[u8],
) -> Result<(), String> {
    use std::io::Write;
    let tmp = temp_block_path(blocks_dir, seqno);
    {
        let mut f = fs::File::create(&tmp)
            .map_err(|e| format!("blockfiles: cannot create temp block file: {e}"))?;
        f.write_all(bytes)
            .map_err(|e| format!("blockfiles: cannot write temp block file: {e}"))?;
        f.sync_all()
            .map_err(|e| format!("blockfiles: cannot fsync temp block file: {e}"))?;
    }
    fs::rename(&tmp, blocks_dir.join(block_file_name(seqno)))
        .map_err(|e| format!("blockfiles: cannot publish block file: {e}"))?;
    Ok(())
}

/// Remove temp files left by crashed block-file writes.
pub(crate) fn sweep_temp_block_files(blocks_dir: &Path) -> Result<usize, String> {
    let mut swept = 0;
    let entries =
        fs::read_dir(blocks_dir).map_err(|e| format!("blockfiles: cannot read blocks dir: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("blockfiles: dir entry failed: {e}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(".tmp-block-") {
            fs::remove_file(entry.path())
                .map_err(|e| format!("blockfiles: cannot sweep temp file: {e}"))?;
            swept += 1;
        }
    }
    Ok(swept)
}

/// Regenerate block files missing from `blocks_dir`, from the database.
///
/// A crash between the atomic DB commit and the block-file write leaves a
/// block committed but its `.blk` file absent — a gap `onx replay` would
/// choke on. The database holds every committed header and body, so files
/// are rebuilt here at startup. Existing files are verified against the
/// committed header hash and rewritten on mismatch (covers torn files
/// written before atomic writes existed).
pub(crate) fn regenerate_missing_block_files(
    store: &ChainStore,
    blocks_dir: &Path,
) -> Result<usize, String> {
    let head_seqno = match store
        .head()
        .map_err(|e| format!("blockfiles: head lookup failed: {e}"))?
    {
        Some((seqno, _)) => seqno,
        None => return Ok(0), // genesis only: no blocks to regenerate
    };
    let mut regenerated = 0;
    for seqno in 1..=head_seqno {
        let path = blocks_dir.join(block_file_name(seqno));
        let needs_write = match fs::read(&path) {
            Ok(bytes) => match decode_block_file(&bytes) {
                Ok(signed) => {
                    let committed = store
                        .block_hash_for_seqno(seqno)
                        .map_err(|e| format!("blockfiles: block hash lookup failed: {e}"))?;
                    // Compare the header hash AND the signature section: sigs
                    // sit outside the hashed header, so a file with a
                    // corrupted sig section would otherwise never be repaired.
                    let committed_sigs = store
                        .get_block_sigs(&committed.ok_or_else(|| {
                            format!("blockfiles: no committed block at seqno {seqno}")
                        })?)
                        .map_err(|e| format!("blockfiles: sig lookup failed: {e}"))?;
                    let file_sigs = onx::auth::encode_sig_section(&signed.sig_entries);
                    committed != Some(signed.block.header.hash())
                        || committed_sigs != Some(file_sigs)
                }
                Err(_) => true, // undecodable: rewrite
            },
            Err(_) => true, // missing: rewrite
        };
        if !needs_write {
            continue;
        }
        let hash = store
            .block_hash_for_seqno(seqno)
            .map_err(|e| format!("blockfiles: block hash lookup failed: {e}"))?
            .ok_or_else(|| format!("blockfiles: no committed block at seqno {seqno}"))?;
        let header = store
            .get_block_header(&hash)
            .map_err(|e| format!("blockfiles: header lookup failed: {e}"))?
            .ok_or_else(|| format!("blockfiles: missing header for seqno {seqno}"))?;
        let body = store
            .get_block_body(&hash)
            .map_err(|e| format!("blockfiles: body lookup failed: {e}"))?
            .ok_or_else(|| format!("blockfiles: missing body for seqno {seqno}"))?;
        let sig_bytes = store
            .get_block_sigs(&hash)
            .map_err(|e| format!("blockfiles: sig lookup failed: {e}"))?
            .ok_or_else(|| format!("blockfiles: missing sigs for seqno {seqno}"))?;
        let sig_entries = onx::auth::decode_sig_section(&sig_bytes)
            .map_err(|e| format!("blockfiles: bad stored sigs for seqno {seqno}: {e}"))?;
        let block = Block { header, body };
        atomic_write_block_file(blocks_dir, seqno, &encode_block_file(&block, &sig_entries))?;
        regenerated += 1;
        eprintln!("blockfiles: regenerated block file for seqno {seqno}");
    }
    Ok(regenerated)
}
