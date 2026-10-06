#!/usr/bin/env python3
"""ONX reference implementation — independent Python implementation of the
ONX message-model block execution, derived from `docs/specification/` and
`docs/adr/0001-0007`, not from the Rust code.

This package is the "second implementation" for the project's
spec-first identity: it must agree with the Rust implementation on every
byte, while being written only from the specification documents.

Layout:
  primitives.py  — domain hashing, big-endian integer codecs, hex helpers
  ed25519.py     — pure-Python Ed25519 (RFC 8032), stdlib only
  cell.py        — Cell type and domain-separated cell hashing (spec §4.2/§4.3)
  trie.py        — Shard state trie and state-root computation (spec §4.5)
  account.py     — AccountState record encoding (spec §4.1)
  messages.py    — external/internal message codecs and identities (ADR-0002)
  chain.py       — wallet handler, delivery, bounce, block application
                   (ADR-0001, ADR-0003, ADR-0004, ADR-0007)
  gen_vectors.py — generates reference/vectors/ from this implementation
  history/       — the original hand-derivation scripts, preserved verbatim
                   (superseded by this package; not run by CI)
  vectors/       — checked-in vectors: genesis, blocks, expectations

Stdlib only. No third-party dependencies.
"""

__version__ = "0.1.0"
