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

The file is served from the repo so deploys need no uploads. On the web box,
add once to `~/.bashrc`:

```bash
deploy-explorer() { deploy-html on-x-scan.com 'https://raw.githubusercontent.com/gokooteam/The-Open-Network-X/main/explorer/index.html'; }
```

Then `deploy-explorer` after every push that touches this directory.
Note: GitHub caches raw files for a few minutes — if a deploy shows the old
version, wait and re-run.

## Versioning

The footer names the ONX commit the explorer was built against. Bump it in
the same commit as any explorer change. For devnet-1, record the SHA-256 of
the deployed file before publishing.
