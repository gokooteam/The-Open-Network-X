# ADR-0033: Currency rename — Onyx → Onyxi, ticker ONX → ONXI

**Status:** Accepted (2026-10-07)
**Decider:** Amethyst

## Context

Claude's audits flagged naming collisions around the project:

Direct statement (from `claude-onx-review-2026-10-06.md`, Strategic forks):

> **Ticker:** collisions common; bigger exposure is "The Open Network X" leaning on TON's brand; declare ONX_ tags/ONXG magic opaque protocol constants; not critical path.

Relayed verbally by Amethyst from Claude's third audit (2026-10-06, not in the saved files — paraphrase, not a direct quote): ONX is already OnX Finance's ticker, Onyxcoin (XCN) is an actively traded token, and "The-Open-Network-X" is one word from "The Open Network." The audit's advice was that renaming is cheap now and painful later, before any public testnet.

## Decision

- **Protocol name: unchanged.** "Open Network X" / "The Open Network X" stays exactly as is. No rename of the project, the repo, or any protocol identifiers.
- **Currency name: Onyx → Onyxi.**
- **Ticker: ONX → ONXI.**

## Rationale (Amethyst's call)

1. People will notice the distinction rather than mistake it for something that already exists — and that distinction becomes the recognition. The near-miss is the brand: playful, deliberate, "one O away from a lawsuit" is already the Gokoo identity.
2. The collision risk Claude names bites at the *token* layer (tickers are unique keys on exchanges and aggregators, where wrong-token confusion costs real money). There is no token right now. The ticker question only becomes real if and when something gets listed — a decision for that day, not this one.
3. Making the ticker *be* the currency name (ONXI = Onyxi) removes the ambiguity class entirely: nobody confuses ONXI with ONX Finance's ONX when the currency itself is called Onyxi.

## Consequences

- Pure docs-and-comments change. Verified: no consensus-critical bytes contain "Onyx" (no domain tags, no genesis material, no signed strings). No chain impact, no genesis change.
- Files updated: `README.md`, `docs/specification/economics.md`, ADRs 0012/0026, spec docs, `INSTRUCTIONS.md`, `ROADMAP.md`, and doc-comments in `onx-economics`, `onx-state-model`, `onx-stf`.
- Denomination unchanged: 1 Onyxi = 10⁹ nano-Onyxi.
- Standing language preserved: "5B initial supply," never "5B cap."
- The protocol-name question is closed. If a token ever lists, the ticker question reopens then — with ONXI as the starting position.
