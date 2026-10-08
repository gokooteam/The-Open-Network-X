# ONX Explorer (static)

`index.html` is the reference block explorer for Open Network X: one
self-contained HTML file, no external scripts, fonts or trackers. Everything
it shows is verified in the browser against the pinned genesis hash before it
is displayed.

## Wiring

The network configuration lives in `window.ONX_CONFIG`, the first script in
the file:

- `chainId`: the network's genesis hash (64 hex chars). Data from any other
  chain is flagged, never silently shown.
- `source`: `{ files: 'https://host/path/' }` serving what `onxd` writes
  (`genesis.boc`, `block-00000001.blk`, …). A cross-origin host must send
  `Access-Control-Allow-Origin` on **every** response, 404s included
  (in nginx: `add_header ... always;`), otherwise a missing block looks like
  a network error instead of "no block yet".

## Deploy

Served at [on-x-scan.com](https://on-x-scan.com/). The file is deployed
from this repository, never edited on the server:
[`scripts/site-pull-deploy.sh`](../scripts/site-pull-deploy.sh) runs from
cron on each server the domain resolves to, and installs
`explorer/index.html` from the newest commit on `main` once that commit's
`site-and-docs` check has passed. Setup, and the hosting problems found on
2026-10-08 (two A records, certificate errors on about half of HTTPS
connections, and an old explorer on one of the servers), are in [`site/README.md`](../site/README.md).

`python3 scripts/site.py check` (run in CI) fails if this file's protocol
constants (`FORMAT.BLOCK_MAGIC`, `HEADER_LEN`, cell and message limits,
`GAS_PER_NANO`, every `TAG` domain tag) disagree with the Rust code, or if a
link here points at a repository path that doesn't exist.
`python3 scripts/site.py live` shows what each server is actually serving.

## Versioning

The footer names the ONX commit the explorer was built against: the commit
whose protocol formats it decodes. Bump it in the same commit as any
explorer change. For devnet-1, record the SHA-256 of
the deployed file before publishing.
