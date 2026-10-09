# Contributor strategy

Amethyst's directive (2026-10-09, ADR-0047, SD3): the solo build got ONX
here — the best systems are not built by one person. This is the plan for
bringing expert human contributors in, and for protecting what's already
built when they arrive.

## Why humans, why now

- **Independent eyes.** The review relay is strong, but it is models
  reviewing a model's work — that has a ceiling. Humans bring adversarial
  diversity no council of AIs can simulate: different failure experience,
  different instincts about what is actually dangerous.
- **Domain depth.** Consensus, P2P networking, and cryptography each have
  people who have shipped them before. No single mind holds all three at
  expert depth.
- **Trust.** A network maintained by many is more credible than one
  maintained by a single operator plus agents. If ONX is going to ask
  anyone to trust it with value, the builder set has to widen.
- **Bus factor.** Right now it is one. That is honest, and it is fragile.
- **The commendation stands.** M0–M5, the spec-first discipline, the audit
  machinery — all in the git log. Humans do not replace that; they build
  on it.

## Readiness: ready BEFORE they arrive

The rule: no contributor arrives to find the door unbuilt.

- [x] Branch protection (`.github/rulesets/main.json`): PR-only, required
  checks, branch must be up to date. Documented in
  `docs/guides/branch-protection.md`.
- [x] Review consensus (`.github/rulesets/review-consensus.json`): the
  relay's verdict gates every merge.
- [x] `CONTRIBUTING.md` and `CODEOWNERS` exist.
- [ ] `CODE_OF_CONDUCT.md` — missing. Add before any invitation goes out.
- [ ] `good first issue` label plus a curated starter set: small,
  well-specified, non-consensus tasks a newcomer can finish in one sitting.
- [ ] Newcomer orientation check: from README to first successful build in
  one sitting, following only the docs. Fix whatever breaks.
- [ ] The task chain is contributor-legible: issues are the work queue,
  labels are the protocol, the guide explains the loop.

## Signal: making the intent known

- README gets a Contributing section that says it plainly: this project
  wants expert help, here is the bar, here is the door.
- The build-journey post (already planned) carries it: real work, honest
  framing, invitation at the end — no hype beyond what is true.
- Her channels, her choice: she decides where the signal goes (X,
  Moltbook, TON communities, Rust circles).

## Recruitment: who, not just anyone

- Not promoters, not randos — the standing rule holds. Engineers with
  shipped systems experience: consensus, P2P networking, cryptography,
  Rust.
- Warm paths beat cold calls: the TON dev community, Rust blockchain
  circles, people who have reviewed, forked, or starred the repo.
- The pitch is the work: spec-first, audited, live devnet, a real review
  pipeline. Serious people join serious projects.

## Protection: the work stays safe

- Nothing about the gates changes when humans arrive: PR-only, CI green,
  relay consensus, Amethyst merges. A human's PR gets the same audit as
  anyone's — including the human's.
- `CODEOWNERS` plus required reviews for sensitive areas (consensus,
  cryptography, wire formats, genesis).
- The direction record (SD2) is the trust anchor: every change carries
  its commissioning direction, so no contributor — human or otherwise —
  quietly redirects the project.
- Amethyst's merge authority does not dilute: more contributors means
  more review bandwidth, not less oversight.
