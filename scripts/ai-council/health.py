#!/usr/bin/env python3
"""AI Council health report.

Reads docs/architecture/ai-council/registry.yaml and prints a human-readable
status report answering: who is supposed to be here, and who is actually
available right now?

Usage:
    python3 scripts/ai-council/health.py [--json]

The --json flag emits machine-readable output for CI.
"""

import sys
import yaml
from pathlib import Path
from datetime import datetime, timezone

REGISTRY = Path(__file__).parent.parent.parent / "docs" / "architecture" / "ai-council" / "registry.yaml"

def load_registry():
    with open(REGISTRY) as f:
        return yaml.safe_load(f)

def main():
    as_json = "--json" in sys.argv
    reg = load_registry()
    participants = reg["participants"]

    # Separate lead from council
    lead = [p for p in participants if p["role"] == "lead_implementation"]
    council = [p for p in participants if p["role"] != "lead_implementation"]

    # Count operational (AVAILABLE only; DEGRADED counts as not fully operational)
    available = [p for p in council if p["status"] == "AVAILABLE"]
    degraded = [p for p in council if p["status"] == "DEGRADED"]
    unavailable = [p for p in council if p["status"] == "UNAVAILABLE"]
    disabled = [p for p in council if p["status"] == "DISABLED"]
    not_configured = [p for p in council if p["status"] == "NOT_CONFIGURED"]

    required = [p for p in council if p.get("required", False)]
    required_available = [p for p in required if p["status"] == "AVAILABLE"]
    required_missing = [p for p in required if p["status"] != "AVAILABLE"]

    # Architecture health: FULL if all required are AVAILABLE, DEGRADED otherwise
    if not required_missing and not degraded and not unavailable:
        arch = "FULL"
    elif not required_missing:
        arch = "DEGRADED (advisory members missing)"
    else:
        arch = "DEGRADED (required members missing — consensus PRs blocked)"

    now = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")

    if as_json:
        import json
        print(json.dumps({
            "lead": [{"id": p["id"], "status": p["status"]} for p in lead],
            "council": [{"id": p["id"], "role": p["role"], "status": p["status"],
                         "required": p.get("required", False)} for p in council],
            "summary": {
                "total": len(council),
                "available": len(available),
                "degraded": len(degraded),
                "unavailable": len(unavailable),
                "required_available": len(required_available),
                "required_total": len(required),
            },
            "architecture": arch,
            "checked_at": now,
        }, indent=2))
        return

    print("OPEN NETWORK X — AI COUNCIL STATUS")
    print()
    print("Lead:")
    for p in lead:
        print(f"  {p['name']:<18} {p['status']}")
    print()
    print("Council:")
    for p in council:
        req = " (required)" if p.get("required") else ""
        print(f"  {p['name']:<18} {p['status']}{req}")
    print()
    print("Council Health:")
    print(f"  {len(available)} / {len(council)} reviewers operational")
    if degraded:
        print(f"  Degraded: {', '.join(p['name'] for p in degraded)}")
    if unavailable:
        print(f"  Unavailable: {', '.join(p['name'] for p in unavailable)}")
    if disabled:
        print(f"  Disabled: {', '.join(p['name'] for p in disabled)}")
    print()
    print("Architecture:")
    print(f"  {arch}")
    print()
    print("Last checked:")
    print(f"  {now}")
    print()
    print("Registry: docs/architecture/ai-council/registry.yaml")
    print("ADR: docs/adr/0049-ai-council-first-class.md")

if __name__ == "__main__":
    main()
