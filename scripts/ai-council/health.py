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

    # Architecture health: FULL only if every council member is AVAILABLE
    # and the lead is AVAILABLE. Any other status degrades it.
    lead_unavailable = [p for p in lead if p["status"] != "AVAILABLE"]
    non_available = [p for p in council if p["status"] != "AVAILABLE"]

    if lead_unavailable:
        arch = "STOPPED (lead unavailable — no work can proceed)"
    elif not non_available:
        arch = "FULL"
    elif not required_missing:
        arch = "DEGRADED (advisory members not AVAILABLE)"
    else:
        arch = "DEGRADED (required members missing — consensus PRs blocked)"

    # This timestamp is when the REPORT was generated, not when statuses were
    # verified. Statuses are hand-set in registry.yaml; see last_successful_check
    # per participant for when each was actually confirmed.
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
            "report_generated_at": now,
            "note": "Statuses are hand-set in registry.yaml; report_generated_at is when this ran, not when statuses were verified.",
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
    if not_configured:
        print(f"  Not configured: {', '.join(p['name'] for p in not_configured)}")
    print()
    print("Architecture:")
    print(f"  {arch}")
    print()
    print("Report generated:")
    print(f"  {now}")
    print("  (Statuses are hand-set in registry.yaml; this timestamp is")
    print("   when the report ran, not when statuses were verified.)")
    print()
    print("Registry: docs/architecture/ai-council/registry.yaml")
    print("ADR: docs/adr/0049-ai-council-first-class.md")

if __name__ == "__main__":
    main()
