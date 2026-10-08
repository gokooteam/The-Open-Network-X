# ADR-0025 — Hypercube Fast-Path Adoption and Cross-Workchain Rules

**Status:** Accepted
**Date:** 2026-09-10

## Context

`docs/specification/architecture.md`'s open questions **ONX-ARCH-011** (Instant Hypercube Routing / fast-path adoption triggers) and **ONX-ARCH-012** (cross-workchain fee conversion rates and message queue expiration bounds) were marked as deferred/future questions in the baseline architecture specification.

This ADR resolves both questions by establishing explicit protocol rules for fast-path activation and cross-workchain message lifecycle rules.

## Decision

1. **Resolution of ONX-ARCH-011 (Hypercube Fast-Path Adoption):**
   - Instant Hypercube Routing ("fast path" direct inter-shard relay with off-chain Merkle proofs) is **enabled** when the destination or transit neighbor shard output queue latency exceeds `FAST_PATH_LATENCY_THRESHOLD = 8` blocks, or when message priority is marked as high-urgency.
   - Fast-path messages MUST include a valid Merkle proof of the originating shard's committed output queue message state.
   - When fast-path routing is active, intermediate transit hops are bypassed and the destination shard directly executes the message upon Merkle proof verification.

2. **Resolution of ONX-ARCH-012 (Cross-Workchain Fees and Queue Expiry):**
   - Cross-workchain forwarding fee conversion factors are governed by masterchain-committed exchange rate ratios:
     $$\text{fee}_{\text{target}} = \text{fee}_{\text{source}} \times \frac{\text{rate}_{\text{target}}}{\text{rate}_{\text{source}}}$$
   - Cross-workchain messages have an explicit expiration logical-time window (`lt_expiry = created_lt + MAX_CROSS_WORKCHAIN_LT_WINDOW`, default `MAX_CROSS_WORKCHAIN_LT_WINDOW = 1,000,000` logical time units).
   - If a cross-workchain message remains unexecuted past `created_lt + MAX_CROSS_WORKCHAIN_LT_WINDOW`, destination validators MUST deterministically generate a bounce transaction refunding remaining gas minus transit fees back to the source address.

## Consequences

- Resolves **ONX-ARCH-011** and **ONX-ARCH-012**.
- Updates `docs/specification/architecture.md` and `docs/specification/transactions.md` to record the resolutions.
