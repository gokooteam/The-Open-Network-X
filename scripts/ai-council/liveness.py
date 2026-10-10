#!/usr/bin/env python3
"""
Council liveness check: who actually showed up?

Instead of pinging APIs or checking app installations, this script looks at
recent PR activity and reports which council members posted reviews or
review-related comments. Observed behavior is the ground truth.

Usage:
    python3 scripts/ai-council/liveness.py [--prs N] [--json]

Compares observed activity against the registry's expected participants
and flags discrepancies (expected but absent, or active but unregistered).
"""

import argparse
import json
import subprocess
import sys
import urllib.request
from pathlib import Path

# Map GitHub usernames/bot names to registry participant IDs.
# These are the accounts that post on behalf of each council member.
BOT_MAP = {
    "coderabbitai[bot]": "coderabbit",
    "greptile-apps[bot]": "greptile",
    "devin-ai-integration[bot]": "devin",
    "claude[bot]": "claude-turn-1",
    "github-advanced-security[bot]": "copilot-scan",
}

REGISTRY = Path(__file__).parent.parent.parent / "docs/architecture/ai-council/registry.yaml"


def gh_api(path):
    """GET a GitHub API path. Uses gh CLI auth if available."""
    # Try gh CLI first (handles auth)
    try:
        out = subprocess.run(
            ["gh", "api", path], capture_output=True, text=True, timeout=30
        )
        if out.returncode == 0:
            return json.loads(out.stdout)
    except (FileNotFoundError, subprocess.TimeoutExpired):
        pass
    # Fallback: unauthenticated (rate-limited but works for public repos)
    req = urllib.request.Request(
        f"https://api.github.com{path}",
        headers={"Accept": "application/vnd.github.v3+json",
                 "User-Agent": "onx-council-liveness"},
    )
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def get_recent_prs(repo, n=5):
    """Return the N most recently updated PRs (open or closed)."""
    data = gh_api(f"/repos/{repo}/pulls?state=all&sort=updated&direction=desc&per_page={n}")
    return [(pr["number"], pr["title"][:60]) for pr in data]


def get_pr_participants(repo, pr_number):
    """Return set of bot/user logins that posted reviews or comments on a PR."""
    participants = set()

    # Reviews
    try:
        reviews = gh_api(f"/repos/{repo}/pulls/{pr_number}/reviews")
        for r in reviews:
            user = (r.get("user") or {}).get("login", "")
            if user:
                participants.add(user)
    except Exception:
        pass

    # Issue comments (relay bot posts turn summaries here)
    try:
        comments = gh_api(f"/repos/{repo}/issues/{pr_number}/comments?per_page=100")
        for c in comments:
            user = (c.get("user") or {}).get("login", "")
            # Only count bot accounts and known relay posters to reduce noise
            if user and ("[bot]" in user or user in ("gokoo",)):
                participants.add(user)
    except Exception:
        pass

    return participants


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--prs", type=int, default=5, help="Number of recent PRs to check")
    ap.add_argument("--json", action="store_true", help="JSON output")
    ap.add_argument("--repo", default="gokooteam/The-Open-Network-X")
    args = ap.parse_args()

    prs = get_recent_prs(args.repo, args.prs)

    # Aggregate: for each PR, who showed up
    pr_activity = {}
    all_observed = set()
    for number, title in prs:
        observed = get_pr_participants(args.repo, number)
        # Map to registry IDs where known
        mapped = {BOT_MAP.get(u, u) for u in observed}
        pr_activity[number] = {
            "title": title,
            "observed_logins": sorted(observed),
            "mapped_participants": sorted(mapped),
        }
        all_observed.update(mapped)

    # Who was seen on the most recent PR vs. who has gone quiet
    latest_pr = prs[0][0] if prs else None
    latest_active = set(pr_activity[latest_pr]["mapped_participants"]) if latest_pr else set()

    # Quiet = seen in older PRs but not the latest
    older_active = set()
    for number in list(pr_activity.keys())[1:]:
        older_active.update(pr_activity[number]["mapped_participants"])
    gone_quiet = sorted(older_active - latest_active)

    result = {
        "prs_checked": args.prs,
        "latest_pr": latest_pr,
        "latest_pr_active": sorted(latest_active),
        "gone_quiet_since_last_pr": gone_quiet,
        "all_observed_across_window": sorted(all_observed),
        "per_pr": pr_activity,
        "note": (
            "Observed activity only. Compare against registry.yaml expected "
            "participants to find gaps. A participant 'gone quiet' may be "
            "UNAVAILABLE, or the PR may not have needed their review class."
        ),
    }

    if args.json:
        print(json.dumps(result, indent=2))
    else:
        print(f"Council liveness — last {args.prs} PRs (repo: {args.repo})")
        print(f"Latest PR: #{latest_pr}")
        print()
        print("Active on latest PR:")
        for p in sorted(latest_active):
            print(f"  ✓ {p}")
        if gone_quiet:
            print()
            print("Gone quiet since last PR (seen before, not on latest):")
            for p in gone_quiet:
                print(f"  ? {p}")
        print()
        print("Per-PR breakdown:")
        for number, info in pr_activity.items():
            print(f"  #{number} ({info['title']}):")
            for p in info["mapped_participants"]:
                print(f"    - {p}")


if __name__ == "__main__":
    main()
