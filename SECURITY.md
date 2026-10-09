# Security policy

## Reporting a vulnerability

**Please do not open a public issue, pull request or discussion for a
security problem.** devnet-1 is public, and a public report can be used
against it before a fix ships.

Report privately through GitHub's private vulnerability reporting:

1. Go to the repository's **Security** tab.
2. Click **Report a vulnerability**
   (direct link: <https://github.com/gokooteam/The-Open-Network-X/security/advisories/new>).
3. Describe the problem, how to reproduce it, and what you think the impact
   is. A failing test, a crafted block or message, or a fuzz input is the
   most useful evidence.

Only the maintainers can see the report. We discuss and fix it there, and
publish an advisory once a fix is released.

If the **Report a vulnerability** button is missing, private reporting is
not enabled yet. Open a public issue titled "Security contact request" with
**no details** about the problem, and a maintainer will set up a private
channel with you.

## What counts

Anything that can make honest nodes disagree, accept an invalid block, lose
or create value, crash, or stall, for example:

- consensus or state-transition bugs (`crates/protocol/`), including VM
  execution that differs from `docs/specification/`;
- block or message authentication bypasses (signatures, chain ID, replay);
- panics, unbounded memory or CPU use, or hangs reachable from a block, an
  external message, or a file in the node's data or message-pool directories;
- leaked secrets or unsafe defaults in `config/`, the CLI or the workflows.

Bugs with no security impact can go in a normal issue.

## Supported versions

ONX is pre-1.0. Only the latest release and `main` get fixes. devnet-1 is a
test network with no real value; it may be reset to ship a fix.

## What to expect

We aim to acknowledge a report within 3 working days and to tell you within
14 days whether we accept it and how we plan to fix it. We will credit you in
the advisory unless you ask us not to.
