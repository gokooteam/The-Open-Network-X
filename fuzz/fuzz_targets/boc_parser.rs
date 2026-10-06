#![no_main]

use libfuzzer_sys::fuzz_target;
use onx_state_model::BagOfCells;

// Exercises untrusted BoC parsing, including declared cell counts, cell
// lengths, reference resolution, hash validation, and DAG cycle detection.
// Both the canonical decoder and the strict variant (which additionally
// requires every referenced cell to be present) must never panic or abort.
fuzz_target!(|data: &[u8]| {
    let _ = BagOfCells::from_bytes(data);
    let _ = BagOfCells::from_bytes_strict(data);
});
