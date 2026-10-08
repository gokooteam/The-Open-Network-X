# ONX Fuzzing

The ONX fuzz package provides coverage-guided regression targets for protocol
boundaries that process untrusted input. It implements TASK-018 using
`cargo-fuzz` and libFuzzer.

## Targets

- `boc_parser` parses arbitrary Bag-of-Cells encodings, including malformed
  cell entries and cyclic reference graphs.
- `tvm_execution` runs arbitrary bounded TVM bytecode against a fixed message
  and execution context, exercising decoder and stack-limit failure paths.
- `block_header` parses block headers and feeds arbitrary public-key/signature
  pairs through BFT vote verification.

Run a target locally from the repository root. `cargo-fuzz` needs a nightly
toolchain, and `rust-toolchain.toml` pins stable, so select nightly
explicitly (CI uses the dated nightly in `.github/workflows/fuzz.yml`):

```sh
rustup toolchain install nightly-2026-09-20 --profile minimal
cargo install cargo-fuzz --locked
cargo +nightly-2026-09-20 fuzz run boc_parser
cargo +nightly-2026-09-20 fuzz run boc_parser -- -max_total_time=60  # bounded
```

## CI

`.github/workflows/fuzz.yml` runs every target for 60 seconds, one matrix job
per target, on each pull request and push to `main`. Runs start from an empty
corpus. A panic, abort, sanitizer report, timeout or out-of-memory fails the
job, and the crashing input is uploaded as the `fuzz-artifacts-<target>`
workflow artifact. Reproduce it locally with
`cargo +nightly-2026-09-20 fuzz run <target> <artifact file>`.

To replay saved regressions, place corpus inputs under
`fuzz/corpus/<target>/` and run the corresponding target. Crash artifacts are
written below `fuzz/artifacts/`; inspect and minimize them before adding a
regression input to the corpus.
