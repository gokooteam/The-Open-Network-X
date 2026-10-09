# Websites

ONX has two websites. Each one is a single static HTML file in this
repository, and the repository is the only source for either.

| Domain | File | What it is |
| --- | --- | --- |
| [on-x.live](https://on-x.live/) | [`site/index.html`](index.html) | Project site: what ONX is, how it is built, current status |
| [on-x-scan.com](https://on-x-scan.com/) | [`explorer/index.html`](../explorer/index.html) | Block explorer for devnet-1 ([`explorer/README.md`](../explorer/README.md)) |

Neither page loads external scripts, fonts or trackers. on-x.live makes one
optional request to the GitHub API to show the newest commit on `main`; the
explorer reads block files from `data.on-x-scan.com`.

## How the sites stay accurate

Three layers, each catching what the one before it can't:

1. **Generated facts.** Everything on on-x.live that the repository can
   answer is generated from it by [`scripts/site.py`](../scripts/site.py):
   specification and ADR counts and lists (titles and statuses from the
   files), workspace version, Rust toolchain, crate count, block format and
   storage schema (from the Rust constants), the current milestone (the ⏭ row
   of `MILESTONES.md`), devnet-1's chain ID (from the explorer config), and
   which ADR records the license. These sit between
   `<!--gen:KEY-->…<!--/gen:KEY-->` markers; never edit inside them by hand.

   Facts that need a run are *measured* and stored in
   [`state.json`](state.json) with the commit they were measured at: the
   test count, the `onx replay` output in the replay demo, and the commit a
   person last reviewed the hand-written statuses against. The page always
   names that commit, and its script tells visitors how many commits `main`
   has moved since.

2. **CI on every PR** ([`docs.yml`](../.github/workflows/docs.yml), job
   `site-and-docs`) runs `python3 scripts/site.py check`, which fails when:
   - a generated region is stale (for example, a PR added an ADR or bumped
     the version but didn't regenerate the site);
   - a link on either site into this repository points at a path that
     doesn't exist;
   - a retired term reappears (`Onyx` instead of `Onyxi`,
     `docs/decisions/ADR-…`, `ONXBLK04`, …) outside a dated post marked
     `<!--history-->`;
   - the probe console names a test that no longer exists;
   - the explorer's protocol constants (block magic, header length, domain
     tags, limits) disagree with the Rust code;
   - the recorded test run has failures.

   The same job checks every Markdown link in the repository
   (`scripts/check-doc-links.py`) and every `WHITEPAPER.md §` citation.

3. **Deployment from `main`, and a monitor.**
   [`scripts/site-pull-deploy.sh`](../scripts/site-pull-deploy.sh) runs from
   cron on each web host. It deploys the newest commit on `main` only if that
   commit's `site-and-docs` check passed, fetches the file by commit SHA,
   sanity-checks it, and swaps it in atomically. The
   [`Site monitor`](../.github/workflows/site-monitor.yml) workflow fetches
   both domains every six hours, several times per URL, and opens (or
   updates, or closes) a `site-drift` issue when what is served differs from
   `main` or TLS fails.

## Day-to-day

```sh
# After changing ADRs, specs, the version, MILESTONES.md's current milestone,
# or the explorer's devnet config:
python3 scripts/site.py build
python3 scripts/site.py check

# When you want the page to show a fresh test count and replay output
# (both are recorded against HEAD, so commit first):
cargo test --workspace --all-targets > /tmp/tests.log 2>&1
cargo test -p onx --test phase5_replay replay_prints_vectors_for_freezing -- --nocapture > /tmp/replay.log 2>&1
python3 scripts/site.py build --tests-log /tmp/tests.log --replay-log /tmp/replay.log

# After re-reading the hand-written statuses (progress list, architecture
# layers, status card) and correcting anything that changed:
python3 scripts/site.py build --reviewed

# What is actually being served right now:
python3 scripts/site.py live
```

Hand-written content (posts, layer descriptions, the progress list) is not
generated. When a PR changes what one of them describes, update it in the
same PR, and use `--reviewed` once you have read the status sections end to
end. A new dated post goes at the top of the feed; the post count updates
itself.

## Deployment

Step-by-step for the current hosts (cPanel cron jobs, the DNS fix for
on-x-scan.com, and what is known about the devnet VM):
[`docs/guides/hosting.md`](../docs/guides/hosting.md). In general, set it up
once per host. Both hosts need `curl` and outbound HTTPS to
`api.github.com` and `raw.githubusercontent.com`.

```cron
# on-x.live host
*/10 * * * * /path/to/site-pull-deploy.sh site/index.html /path/to/on-x.live/docroot/index.html >> $HOME/onx-deploy.log 2>&1
# on-x-scan.com host(s): every server the domain resolves to
*/10 * * * * /path/to/site-pull-deploy.sh explorer/index.html /path/to/on-x-scan.com/docroot/index.html >> $HOME/onx-deploy.log 2>&1
```

Copy the script from `scripts/` to the host (it has no other dependencies).
Each run makes two unauthenticated GitHub API calls; at a 10-minute interval
that is 12 of the 60 requests per hour GitHub allows per IP.

The file must be served exactly as committed. Don't wrap it, minify it, or
inject anything; the monitor compares bytes, and a wrapped copy is drift.

## Known issues (found 2026-10-08)

**Status 2026-10-09:** on-x-scan.com now has one A record
(`198.54.114.221`), and both sites are deployed by the cron jobs from
[`docs/guides/hosting.md`](../docs/guides/hosting.md). Kept below for
history until the `Site monitor` reports both domains clean.

These are on the hosting side, not in this repository, so they need someone
with access to the DNS and the servers:

- **on-x-scan.com resolves to two different servers.** The apex has two A
  records, `192.241.136.73` (nginx/Ubuntu, the same server as
  `data.on-x-scan.com`) and `198.54.114.221` (LiteSpeed). About half of all
  visits therefore land on each. Observed from here:
  - roughly half of HTTPS handshakes for `on-x-scan.com` and
    `www.on-x-scan.com` present the certificate for **`data.on-x-scan.com`**
    only, so browsers show a certificate error;
  - over HTTPS the LiteSpeed server serves an **old explorer**
    (`ONXBLK04`, 148-byte header, built against `c9cb650`, testnet pinned to
    chain ID `fd336cc5…` and source `https://data.on-x-scan.com/`). It can't
    decode devnet-1's `ONXBLK05` blocks;
  - over HTTP the two servers answer differently too (nginx serves the
    explorer from `f1a88fb`; LiteSpeed answers with a 301 page).

  Fix ([`docs/guides/hosting.md`](../docs/guides/hosting.md) walks
  through it): decide which server hosts the explorer and remove the other A record
  (and the `www` CNAME target, which follows the apex), or make both servers
  serve the same file with a certificate valid for `on-x-scan.com` and
  `www.on-x-scan.com`. Then run the deploy script there. `python3
  scripts/site.py live` should report every fetch as a match.

- **on-x.live was edited outside the repository.** Its served copy (last
  modified 2026-10-06) was imported verbatim in this repository's history and
  then corrected; until the deploy cron is installed on its host, it keeps
  serving the old copy (currency still "Onyx", 26 links to the retired
  `docs/decisions/` paths, test count and build from 2026-10-05).
