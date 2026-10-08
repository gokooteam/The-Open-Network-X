#!/bin/sh
# Pull-deploy one website file from this repository onto a web host.
#
# Run it from cron on the machine that serves the site, e.g. every 10 minutes:
#
#   */10 * * * * /path/to/site-pull-deploy.sh site/index.html /home/USER/public_html/index.html >> ~/onx-deploy.log 2>&1
#   */10 * * * * /path/to/site-pull-deploy.sh explorer/index.html /var/www/on-x-scan.com/index.html >> ~/onx-deploy.log 2>&1
#
# What it does, in order, and why:
#  1. Asks GitHub for the newest commit on the branch (default main).
#  2. Refuses to deploy unless that commit's `site-and-docs` check run
#     (.github/workflows/docs.yml), created by GitHub Actions, completed
#     successfully, so a page whose
#     facts disagree with the repository never goes out.
#  3. Downloads the file *at that commit SHA* (immutable URL, so GitHub's
#     raw-file cache can't hand back an old copy).
#  4. Sanity-checks it (size, doctype, closing </html>).
#  5. Replaces the served file atomically (write next to it, then rename),
#     keeping the previous copy as <file>.prev, only if it changed.
#
# Needs: POSIX sh, curl, sha256sum (or shasum). No GitHub token: the repository
# is public, and the two unauthenticated API calls per run stay well inside
# GitHub's 60-requests-per-hour limit at a 10-minute interval.
#
# Usage: site-pull-deploy.sh <repo path> <served file> [branch]
# Exit status: 0 deployed or already current; 1 refused or failed.

set -eu

REPO="gokooteam/The-Open-Network-X"
CHECK_NAME="site-and-docs"
# Only check runs created by GitHub Actions count. Any other GitHub App with
# checks:write on the repository could post a run with the same name.
ACTIONS_APP_ID=15368
MIN_BYTES=20000

src_path=${1:?usage: site-pull-deploy.sh <repo path> <served file> [branch]}
dest=${2:?usage: site-pull-deploy.sh <repo path> <served file> [branch]}
branch=${3:-main}

log() { printf '%s %s: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$src_path" "$*"; }
sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

api() {
  curl -fsS --max-time 30 -H "Accept: application/vnd.github+json" \
    -H "User-Agent: onx-site-pull-deploy" "https://api.github.com/repos/$REPO/$1"
}

# 1. Newest commit on the branch.
# The first "sha" in the response is the commit's own (the API returns
# compact JSON, so match on keys, not on line layout).
sha=$(api "commits/$branch" | grep -o '"sha": *"[0-9a-f]\{40\}"' | head -n 1 | grep -o '[0-9a-f]\{40\}' || true)
if [ -z "$sha" ]; then
  log "could not read the newest commit on $branch from the GitHub API; nothing changed"
  exit 1
fi

# 2. The docs/site check must have passed on exactly that commit.
runs=$(api "commits/$sha/check-runs?check_name=$CHECK_NAME&app_id=$ACTIONS_APP_ID")
total=$(printf '%s' "$runs" | grep -o '"total_count": *[0-9]*' | head -n 1 | grep -o '[0-9]*$' || true)
# One "conclusion" per check run: "success", another word, or null while running.
conclusions=$(printf '%s' "$runs" | grep -o '"conclusion": *\("[a-z_]*"\|null\)' | sed 's/.*: *//; s/"//g' | sort -u)
if [ "${total:-0}" -lt 1 ] || [ "$conclusions" != "success" ]; then
  seen=$(printf '%s' "$conclusions" | tr '\n' ' ')
  log "commit ${sha%"${sha#???????}"}: check '$CHECK_NAME' has not passed (runs: ${total:-0}, conclusions: ${seen:-none}); not deploying"
  exit 1
fi

# 3. Fetch the file at that commit.
dir=$(dirname "$dest")
tmp="$dir/.onx-deploy.$$"
trap 'rm -f "$tmp"' EXIT
curl -fsS --max-time 60 -o "$tmp" "https://raw.githubusercontent.com/$REPO/$sha/$src_path"

# 4. Sanity checks: a truncated download or an error page must never go live.
size=$(wc -c < "$tmp" | tr -d ' ')
if [ "$size" -lt "$MIN_BYTES" ]; then
  log "downloaded file is only $size bytes; not deploying"
  exit 1
fi
if ! head -c 64 "$tmp" | grep -qi '^<!doctype html>'; then
  log "downloaded file does not start with <!doctype html>; not deploying"
  exit 1
fi
if ! tail -c 64 "$tmp" | grep -qi '</html>'; then
  log "downloaded file does not end with </html> (truncated?); not deploying"
  exit 1
fi

# 5. Replace only if it changed.
if [ -f "$dest" ] && [ "$(sha256 "$tmp")" = "$(sha256 "$dest")" ]; then
  exit 0
fi
[ -f "$dest" ] && cp -p "$dest" "$dest.prev"
chmod 644 "$tmp"
mv -f "$tmp" "$dest"
trap - EXIT
log "deployed commit $sha (sha256 $(sha256 "$dest"))"
