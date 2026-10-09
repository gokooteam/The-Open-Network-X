# ADR-0046 — Project logo: the Onyxi mark

**Status:** Accepted (2026-10-09).
**Decider:** Amethyst.

## Context

Open Network X had no visual identity — the project site used a
placeholder SVG glyph (three nodes and lines) and the explorer a CSS
gradient swatch. Amethyst commissioned three logo options from a design
arm: (1) a faceted black onyx stone with pale bands forming an X, (2) an
X node monogram, (3) a tracked-out ONX wordmark. She selected the onyx
mark and refined it herself into two variants: a compact near-square
stone and a taller elongated stone, both replacing the pale bands with
a glowing blue edge light.

## Decision

**The official logo is the Onyxi mark: a faceted black onyx stone with blue
glowing edges.** The compact variant is the primary mark (the "spinning"
one); the tall variant is the secondary mark. Both are centered at the
top of `README.md`, with the primary mark featured.

Placement:

- `README.md` — both variants centered at the top, primary mark
  featured at 220 px.
- `site/index.html` (on-x.live) — the primary mark replaces the
  placeholder SVG avatar, inlined as base64 (the hosts deploy only the
  single HTML file), with a slow 36 s CSS rotation honoring
  `prefers-reduced-motion`.
- `explorer/index.html` (on-x-scan.com) — the primary mark replaces the
  CSS gradient swatch in the header, inlined as base64, static.
- Source files live at `docs/assets/onx-logo-compact.jpg` and
  `docs/assets/onx-logo-tall.jpg`.

## Consequences

- The placeholder `#mark` SVG symbol is redefined to embed the Onyxi mark
  (a data-URI `<image>`), so the existing `<use href="#mark"/>` call sites —
  rail logo, feed header, post avatars — pick it up with no further changes.
  The explorer's gradient `.mark` styling is retired.
- The sites stay single-file and tracker-free: the marks are data URIs,
  no external image requests.
- If an animated version of the primary mark is produced later, it
  replaces the static file in `docs/assets/` and the README reference;
  the inlined site copies are regenerated from it.
