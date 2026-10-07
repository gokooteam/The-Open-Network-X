Open Network X — Development Instructions

Purpose

These instructions govern the design, research, implementation, testing, and documentation of Open Network X (ONX).

ONX is an independent implementation effort based primarily on the architecture and technical concepts described in the original TON white paper.

The objective is not to reproduce an existing codebase.

The objective is to independently implement the underlying protocol architecture.

---

1. Fundamental Principle

Start from the specification, not the implementation.

When solving a protocol problem, the first question must be:

«What does the reference specification require?»

Not:

«How does the existing TON implementation do this?»

Existing implementations may be examined as secondary research material, but they are not automatically authoritative for ONX.

---

2. Project Identity

Always refer to this project as:

Open Network X

Short form:

ONX

The native currency is:

Onyxi

Do not describe ONX as:

- TON
- a TON fork
- an official TON network
- the successor to TON
- a replacement for TON
- an official TON implementation

ONX is independent.

---

3. White Paper Authority

The repository's white paper is the primary historical and architectural reference.

When implementing a protocol feature:

1. Locate the relevant white-paper section.
2. Determine what behavior is explicitly specified.
3. Separate explicit requirements from interpretation.
4. Record unresolved ambiguities.
5. Design the ONX implementation.
6. Write tests demonstrating the intended behavior.

Do not silently substitute assumptions from an existing implementation.

---

4. Preserve the Reference

The original white paper must remain unchanged.

Do not edit, rewrite, modernize, or reinterpret the reference document itself.

If ONX needs an interpretation, create a separate document.

For example:

docs/
├── specification/
│   ├── architecture.md
│   ├── consensus.md
│   ├── sharding.md
│   ├── networking.md
│   └── virtual-machine.md
│
└── decisions/
    ├── ADR-0001-example.md
    └── ADR-0002-example.md

The reference remains the reference.

ONX documentation explains ONX.

---

5. Independent Implementation

Do not begin implementation by copying source code from another blockchain implementation.

The preferred workflow is:

Reference
   ↓
Protocol requirement
   ↓
ONX design
   ↓
ONX interface
   ↓
ONX implementation
   ↓
Tests

Not:

Existing implementation
   ↓
Copy
   ↓
Rename
   ↓
Modify

This distinction is fundamental to the project.

---

6. Existing Implementations

Existing TON implementations may be studied when useful for:

- understanding historical engineering decisions
- identifying practical interpretations
- discovering edge cases
- comparing protocol behavior
- researching undocumented behavior
- validating interoperability concepts

However, such research must not automatically become an ONX requirement.

When existing behavior differs from the reference specification, document the difference.

---

7. Ambiguity

The white paper will contain areas where the intended implementation is unclear.

Never hide ambiguity.

When an important ambiguity is discovered:

1. Document it.
2. Identify the relevant reference section.
3. Describe the possible interpretations.
4. Select an ONX interpretation.
5. Explain why that interpretation was selected.
6. Give the decision a permanent identifier when appropriate.

Example:

Decision: ONX-ADR-0001
Topic: Validator election interpretation
Reference: White paper, Section X
Status: Accepted

---

8. Deviations

ONX may eventually deviate from the reference design.

A deviation is acceptable only when it is explicit.

Every significant deviation should answer:

- What does the reference describe?
- What does ONX do instead?
- Why?
- What problem does the change solve?
- What compatibility consequences exist?
- Could the decision be reversed later?

Never disguise a deviation as faithful implementation.

---

9. Protocol Layers

Keep protocol concerns separated.

At minimum, distinguish between:

Cryptography
Consensus
Networking
Blockchain
State
Sharding
Virtual Machine
Smart Contracts
Storage
Economics
Node Software
Developer Tooling
Applications

Do not allow application-layer assumptions to silently become consensus rules.

---

10. Consensus-Critical Code

Consensus-critical code receives the highest level of scrutiny.

Any code affecting:

- block validity
- transaction validity
- state transitions
- validator selection
- signatures
- consensus
- message ordering
- shard configuration
- cryptographic verification

must be deterministic and independently testable.

Avoid hidden sources of nondeterminism.

Never use local machine state, wall-clock assumptions, random behavior, or network-dependent behavior in consensus logic unless explicitly required by the protocol.

---

11. Cryptography

Cryptographic primitives must never be invented casually.

Use established, reviewed primitives wherever possible.

Document:

- algorithm
- parameters
- serialization
- hashing
- signing
- key formats
- domain separation
- verification rules

Cryptographic behavior that affects consensus must have extensive tests.

---

12. Serialization

Serialization is part of the protocol.

Never treat encoding as an incidental implementation detail.

For every consensus-relevant data structure, define:

- canonical representation
- field ordering
- integer encoding
- length encoding
- optional fields
- hashing behavior
- malformed-input behavior

Equivalent semantic objects must not accidentally produce different protocol representations.

---

13. Networking

Separate the network protocol from application behavior.

Document:

- peer discovery
- peer identity
- message formats
- authentication
- transport
- routing
- synchronization
- block propagation
- shard communication
- failure behavior

Network behavior must never be assumed to be consensus behavior unless explicitly specified.

---

14. Sharding

Sharding is a fundamental architectural concern of ONX.

Do not implement sharding merely as a database partitioning technique.

The design must account for protocol-level concepts including:

- shard identity
- shard state
- shard boundaries
- message routing
- cross-shard communication
- shard splitting
- shard merging
- validator responsibility
- synchronization
- consistency

Every major sharding mechanism must have tests.

---

15. Masterchain and Workchains

Keep the conceptual distinction between:

Masterchain
Workchain
Shardchain

clear throughout the codebase.

Do not collapse these concepts merely because a simplified implementation is easier.

If the MVP temporarily simplifies them, explicitly label the simplification as such.

---

16. Virtual Machine

The virtual machine must be treated as a protocol component.

Its behavior should eventually be specified independently of the host programming language.

Document:

- instruction semantics
- memory/state model
- execution limits
- gas/resource accounting
- exceptions
- serialization
- deterministic execution
- contract state transitions

A contract executed on two independent nodes must produce the same protocol result.

---

17. Smart Contracts

Smart contracts are state machines governed by protocol rules.

Do not allow host-language behavior to accidentally define contract behavior.

The VM must define contract execution.

The host implementation merely executes that definition.

---

18. Economics

Onyxi economics must be explicitly specified.

Do not introduce tokenomics simply because a conventional blockchain normally has them.

Economic rules should be derived from protocol requirements and explicit ONX decisions.

This includes:

- issuance
- supply
- validator rewards
- transaction fees
- storage fees
- staking
- penalties
- inflation/deflation
- denomination
- minimum balances
- economic security

---

19. Testing Philosophy

Every protocol component should eventually have multiple classes of tests.

Unit tests

Test individual components.

Property tests

Test invariants across broad input ranges.

Determinism tests

Ensure independent executions produce identical results.

Integration tests

Test interactions between protocol components.

Network tests

Test multiple nodes.

Adversarial tests

Test malformed messages, invalid blocks, conflicting states, malicious peers, and Byzantine behavior.

Compatibility tests

Where compatibility is intentionally supported, verify it explicitly.

---

20. Documentation Is Part of the Protocol

If behavior matters enough to implement, it matters enough to document.

Avoid undocumented consensus rules.

Avoid comments that merely restate code.

Prefer documentation that explains:

- why a rule exists
- what specification section motivates it
- what invariants must hold
- what assumptions are being made

---

21. AI Development Rules

AI coding agents working in this repository must follow the same principles as human contributors.

An AI agent must not:

- invent protocol behavior without documenting it
- silently copy another implementation
- claim compatibility without testing it
- remove difficult protocol features merely because they are inconvenient
- change consensus behavior without identifying the change
- treat existing TON source code as automatically authoritative
- introduce dependencies without justification
- rewrite architectural decisions without documentation

Before implementing a major feature, an AI agent should identify:

Reference:
Requirement:
ONX interpretation:
Implementation:
Tests:
Known limitations:

---

22. Do Not Optimize Prematurely

The first implementation should prioritize:

1. Correctness
2. Determinism
3. Auditability
4. Testability
5. Specification fidelity
6. Performance

Optimization comes later.

A fast incorrect blockchain is not useful.

---

23. Build Incrementally

Do not attempt to build the complete network simultaneously.

The preferred progression is approximately:

Protocol primitives
        ↓
Data structures
        ↓
State model
        ↓
Transactions
        ↓
Blocks
        ↓
Execution
        ↓
Consensus
        ↓
Networking
        ↓
Multiple nodes
        ↓
Sharding
        ↓
Validator infrastructure
        ↓
Economic system
        ↓
Public test network

The exact sequence may change as the architecture becomes better understood.

---

24. Every Major Decision Leaves a Record

Important architectural decisions should be recorded permanently.

Use Architecture Decision Records where appropriate.

Recommended format:

docs/adr/NNNN-title.md

Each ADR should contain:

# ADR-NNNN — Title

Status:

Context:

Reference:

Problem:

Decision:

Alternatives:

Consequences:

Implementation:

Tests:

---

25. The Golden Rule

When uncertain, stop and investigate.

Do not fill a protocol gap with an assumption and move on.

Ask:

«Is this specified?»

If yes, implement it.

If no:

«Is there a defensible interpretation?»

If yes, document it.

If no:

«Create an explicit ONX protocol decision.»

---

26. Definition of Success

ONX is successful when it can demonstrate, through code and tests, that it independently implements the architectural concepts it claims to implement.

The goal is not to look like an existing blockchain.

The goal is to understand the design deeply enough to build it independently.

---

Build the protocol first.

Build the network second.

Build the ecosystem after the protocol is trustworthy.
