#!/usr/bin/env bash
# Creates or updates the "main: require CI" repository ruleset from
# .github/rulesets/main.json. Needs a token with admin rights on the repo
# (`gh auth login` as a repo admin). See docs/guides/branch-protection.md.
set -euo pipefail

repo="${1:-gokooteam/The-Open-Network-X}"
file="$(dirname "$0")/../.github/rulesets/main.json"
name="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["name"])' "$file")"

id="$(gh api "repos/$repo/rulesets" --jq ".[] | select(.name == \"$name\") | .id" | head -n1)"
if [ -n "$id" ]; then
  gh api -X PUT "repos/$repo/rulesets/$id" --input "$file" --jq '"updated ruleset \(.id): \(.name)"'
else
  gh api -X POST "repos/$repo/rulesets" --input "$file" --jq '"created ruleset \(.id): \(.name)"'
fi
