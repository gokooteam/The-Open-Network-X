# ADR-0047 — Human-contributor strategy

**Status:** Accepted (2026-10-09).
**Decider:** Amethyst.

## Context

Amethyst's words, the same day she commissioned the task chain: there
should be a record of the thing being accomplished with her direction, the
direction should be documented, and that is the only way it would be
trusted. And then the second thing: a plan to bring more humans into the
protocol — what that adds, why it is necessary, it being known that her
hope is for people who actually know what they are doing to come help, a
method for being ready when they arrive, and protection for the work already
done.

Her framing of the truth underneath it: *the best systems are not built by
one person.* The solo build — M0 through M5, the spec-first discipline, the
audit machinery — is commended and it is in the git log. It got ONX this
far. It cannot finish it alone, and it should not have to.

## Decision

**ONX will be made ready for expert human contributors, deliberately and
before they arrive.** Standing directive SD3 in `MILESTONES.md`. The working
plan is `docs/planning/contributor-strategy.md`; this ADR records the
decision itself:

1. **The case is stated plainly.** Humans add what the current setup
   cannot: adversarially diverse review (models reviewing a model's work
   has a ceiling), domain depth in consensus/networking/cryptography no
   single mind holds, credibility for a network that will one day ask
   strangers to trust it with value, and a bus factor above one.
2. **Readiness comes first.** Branch protection, review gates, and
   contributor docs are finished *before* any invitation goes out — no one
   arrives to find the door unbuilt.
3. **The intent is public.** The project says openly that it wants expert
   help: README, the build-journey post, wherever she chooses.
4. **Recruitment is selective.** Engineers with shipped systems experience —
   not promoters, not randos (the standing rule holds). Warm paths through
   the TON and Rust blockchain communities; the pitch is the work itself.
5. **The existing work stays protected.** Nothing about the gates changes
   when humans arrive: PR-only, CI green, the relay's consensus, Amethyst
   merges. A human's PR gets the same audit as anyone's. The direction
   record (SD2) is the trust anchor — every change carries its
   commissioning direction, so no contributor, human or otherwise, quietly
   redirects the project. More contributors means more review bandwidth,
   not less oversight.

The commendation is part of the record too: one person plus her agents
built a spec-first chain with a live devnet and a real audit pipeline.
That is what the newcomers are being invited to build on — not to replace.
