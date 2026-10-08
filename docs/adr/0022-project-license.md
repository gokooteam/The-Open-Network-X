# ADR-0022 — Project License Proposal

**Status:** Accepted — Apache-2.0 adopted; see Consequences for the follow-up changes made.  
**Date:** 2026-09-09
**Accepted:** 2026-09-09

## Context

README currently describes ONX as an independent implementation effort and leaves
its license undecided. A license must preserve that independent-decision posture
while making source reuse and contribution expectations clear.

## Survey

Comparable reference implementations use several models:

| Project | License model | Practical implication |
| --- | --- | --- |
| Bitcoin Core | MIT | Short, permissive terms; downstream proprietary reuse is allowed. |
| Polkadot | GPL-3.0 | Copyleft for distributed source derivatives; stronger reciprocal sharing obligations. |
| go-ethereum | LGPL-3.0 | Library-oriented weak copyleft; linking and derivative-boundary questions require care. |
| Rust ecosystem projects | MIT/Apache-2.0 dual license | Permissive reuse with Apache's express patent grant and MIT compatibility. |

The comparison is descriptive, not a claim of compatibility or affiliation. The
projects' repository license files are the source for the classifications above.

## Problem

ONX needs a future license that is understandable to independent implementers,
does not suggest authority over another network, and limits legal ambiguity for
contributors. It should not be chosen implicitly by copying a comparable project.

## Proposal

After review, adopt **Apache-2.0** for the repository. It is a permissive license
with an express patent grant and termination clause, a NOTICE mechanism for
attribution, and clear terms for an independently governed protocol
implementation. It does not impose network-source publication obligations, which
keeps independent experimentation possible while the project is still specifying
its protocol.

## Alternatives Considered

- **MIT:** simpler and widely recognized, but does not include Apache-2.0's
  explicit patent grant.
- **MIT/Apache-2.0 dual:** maximizes Rust ecosystem familiarity, but adds a
  choice ONX does not currently need and makes attribution policy less singular.
- **GPL-3.0:** offers strong reciprocity, but its derivative-work obligations
  could deter integration by independent infrastructure operators.
- **LGPL-3.0:** is better suited to a reusable library boundary than a
  repository-wide protocol implementation and retains boundary complexity.

## Consequences

Accepted. The canonical Apache-2.0 text has been added as `LICENSE`, the
`license` field in every crate's `Cargo.toml` has been updated from the
placeholder `MIT` value to `Apache-2.0`, and the README's License section now
points at this ADR instead of describing the license as pending review. No
`NOTICE` file is established by this change; one may be added later if
attribution needs arise.

## Tests

No code changes. Review verifies the eventual license text and repository
metadata exactly match the accepted decision.
