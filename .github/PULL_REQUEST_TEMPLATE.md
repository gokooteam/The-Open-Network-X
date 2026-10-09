## Consensus impact
<!-- Tick one. breaking: a node with this PR and a node without it compute
different state for the same blocks. adjacent: touches consensus code but
results are unchanged. none: no consensus code. The Claude audit checks it. -->
- [ ] breaking
- [ ] adjacent
- [ ] none

Could this behave differently on two honest nodes given the same inputs?
<!-- Iteration order, floating point, time, locale, platform width, randomness. -->

## Specification and decision record
- [ ] I identified the applicable `docs/specification/` section.
- [ ] If no specification existed, this PR adds it before implementation.
- [ ] I added an ADR for a significant interpretation or deviation.
- [ ] I implemented the complete specified structure, or documented each tracked exception.
- [ ] I described any ambiguity or deviation explicitly below.

## Checks
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo build --workspace --all-targets`
- [ ] `cargo test --workspace --all-targets`

## Notes
<!-- State the specification section implemented and any deliberate deviations. -->

## Prompts
<!-- Exact prompt(s) that orchestrated this change — verbatim, not paraphrased. Required for every PR not triggered by a GitHub Action (the workflow file is the prompt for those). See CONTRIBUTING.md "Prompt documentation". -->

## Review relay
<!-- After the relay reaches consensus, Gokoo does a live verification pass (Turn 4): checks the relay's claims, runs scoped tests, posts GO/NO-GO. Advisory; Amethyst merges. -->
