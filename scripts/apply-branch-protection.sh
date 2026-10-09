#!/usr/bin/env bash
# Creates or updates a repository ruleset from a file in .github/rulesets/
# (default main.json, "main: require CI"; review-consensus.json is the review
# relay's merge gate). Needs a token with admin rights on the repo
# (`gh auth login` as a repo admin). See docs/guides/branch-protection.md.
#
#   scripts/apply-branch-protection.sh [owner/repo] [ruleset file]
set -euo pipefail

repo="${1:-gokooteam/The-Open-Network-X}"
file="${2:-$(dirname "$0")/../.github/rulesets/main.json}"
name="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["name"])' "$file")"

id="$(gh api "repos/$repo/rulesets" --paginate --jq ".[] | select(.name == \"$name\") | .id" | sed -n 1p)"
if [ -n "$id" ]; then
  gh api -X PUT "repos/$repo/rulesets/$id" --input "$file" --jq '"updated ruleset \(.id): \(.name)"'
else
  gh api -X POST "repos/$repo/rulesets" --input "$file" --jq '"created ruleset \(.id): \(.name)"'
fi
