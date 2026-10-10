# Open Network X — Project Philosophy

What we believe about how to build a trustworthy network, and why we build the way we do.

This is not marketing. It is the reasoning behind our decisions, written down so that future contributors — human or otherwise — understand not just what we do, but why.

---

## Accountability by construction

> Every protocol message carries enough context that equivocation is cryptographically provable by anyone, no trusted observer needed. The goal isn't to make cheating hard — it's to make it irrational, because detection is certain.

We do not merely resist attack. We structure the protocol so that attacking generates undeniable proof of the attack. A rational actor, knowing detection is certain, does not cheat. (ADR-0050)

## Spec first, code second

We write down what the system should do before we build it. The specification is the source of truth; the implementation is an attempt to match it. When they disagree, the spec wins until we deliberately change it.

This discipline is what lets us say "the whitepaper either has locking or it doesn't" instead of patching symptoms.

## Adversarial review is not optional

Every significant change passes through independent reviewers who are instructed to find what's wrong — not to approve what's right. The council exists because one perspective is insufficient, and because the builder cannot review their own work.

Review is not a gate to pass. It is part of how the work gets better.

## The gate binds its creator

The review process constrains everyone, including the project's founder. A system its creator can casually bypass is a suggestion box, not a review system. The override exists for genuine emergencies; the norm is that the process holds.

## Evidence over assertion

"Done" means proven, not claimed. Task logs, PR descriptions, and design documents must point to evidence — test output, proof scripts, review verdicts. A TODO against a MUST is a blocker, not a footnote.

The strongest proof is what the founder can see run on her own machine.

## Honest about what we don't know

When the whitepaper is ambiguous, we say so. When we deviate from it, we document why. When a reviewer finds something we missed, we fix it and record the lesson. The history of our mistakes is as valuable as the history of our decisions.

## Better through understanding, not assertion

We do not claim to be better than existing networks. We build faithfully first, understand deeply, and let improvements emerge from that understanding. "Better" is something you prove with a running network, not something you declare in a whitepaper.

## The work is the point

We build in the open, document our reasoning, and preserve our disagreements. If this project matters in ten years, it won't be because of who built it — it will be because the work was sound and the record was honest.

---

*This document grows as the project teaches us what we believe. When a new principle earns its place through experience — not theory — it goes here.*
