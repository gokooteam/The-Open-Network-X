#!/usr/bin/env python3
"""Keep the project websites accurate: generate, check, and compare with live.

The project site (on-x.live, `site/index.html`) states facts about this
repository: how many specifications and ADRs exist, the workspace version,
the toolchain, the block format, devnet-1's chain ID, the current milestone,
test results, and real `onx replay` output. Hand-maintained, those facts went
stale within days. This script makes the repository the single source.

Two kinds of facts:

* **Derived** facts are recomputed from the working tree on every run
  (spec and ADR lists, version, toolchain, protocol constants, the current
  milestone row of MILESTONES.md, devnet-1's chain ID from the explorer
  config, the golden roots in phase5_replay.rs).
* **Measured** facts come from running something and are recorded with the
  commit they were measured at, in `site/state.json`: the test count
  (`--tests-log`), the replay output (`--replay-log`), and the commit whose
  statuses a person last reviewed (`--reviewed`). The page always names that
  commit, and its own script tells visitors how far `main` has moved since.

Generated regions in the HTML are delimited by markers:

    <!--gen:KEY-->…<!--/gen:KEY-->        (HTML)
    /*gen:KEY*/…/*/gen:KEY*/              (inside the page's script)

README.md has one generated region too, `readme_status`: the development
phase and the milestone map, built from MILESTONES.md ('At a glance' plus
the exit-criteria checkboxes under each milestone's heading).

Commands:

    build   Rewrite every generated region from the repository (and update
            measured facts when their logs/flags are given), in the site
            and in README.md.
    check   Fail if any generated region (site or README) is stale, if a link into this
            repository points at a path that doesn't exist, if retired terms
            reappear, if the probe console names a test that doesn't exist,
            or if the explorer's protocol constants disagree with the code.
    live    Fetch what on-x.live and on-x-scan.com actually serve and compare
            it with the files in this checkout; also reports TLS failures.

Usage:

    python3 scripts/site.py build [--tests-log FILE] [--replay-log FILE] [--reviewed]
    python3 scripts/site.py check
    python3 scripts/site.py live [--attempts N] [--json FILE]
    python3 scripts/site.py milestones     (JSON for the Milestones workflow)

Only the Python standard library is used.
"""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import html
import json
import re
import ssl
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SITE = ROOT / "site" / "index.html"
README = ROOT / "README.md"
STATE = ROOT / "site" / "state.json"
EXPLORER = ROOT / "explorer" / "index.html"
REPO = "https://github.com/gokooteam/The-Open-Network-X"
BLOB = f"{REPO}/blob/main/"
TREE = f"{REPO}/tree/main/"

# Deployed copies, compared by `live`. Each is served from a single file in
# this repository; the first URL is the canonical one.
DEPLOYMENTS = {
    "on-x.live": {"file": SITE, "urls": ["https://on-x.live/", "https://www.on-x.live/"]},
    "on-x-scan.com": {"file": EXPLORER, "urls": ["https://on-x-scan.com/", "https://www.on-x-scan.com/"]},
}

# Words and paths that were retired. If one reappears on a page, the page is
# describing something that no longer exists. Each entry: regex, reason.
RETIRED = [
    (r"\bOnyx\b", "currency is Onyxi since ADR-0033 (write 'Onyxi')"),
    (r"docs/decisions/ADR-", "ADRs moved to docs/adr/ (see docs/decisions/README.md)"),
    (r"\bONXBLK0[1-4]\b", "block files are ONXBLK05 since ADR-0032"),
]
# Retired terms are allowed inside <!--history-->…<!--/history--> regions:
# dated posts that describe what was true at the time.
RETIRED_ALLOWED_IN = ("history",)


# --------------------------------------------------------------------------
# Small helpers
# --------------------------------------------------------------------------

def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=ROOT, capture_output=True, text=True, check=True
    ).stdout.strip()


def esc(text: str) -> str:
    return html.escape(text, quote=True)


def first_heading(path: Path) -> str:
    for line in read(path).splitlines():
        if line.startswith("# "):
            return line[2:].strip()
    raise SystemExit(f"{path.relative_to(ROOT)}: no '# ' heading")


def fmt_int(n: int) -> str:
    return f"{n:,}"


# --------------------------------------------------------------------------
# Derived facts
# --------------------------------------------------------------------------

def specs() -> list[dict]:
    out = []
    order = ["architecture", "protocol-primitives", "data-structures", "state-model",
             "transactions", "blocks", "execution", "tvm-instruction-set", "consensus",
             "networking-adnl", "networking-dht", "networking-overlay", "sharding",
             "economics", "payment-channels"]
    files = sorted((ROOT / "docs" / "specification").glob("*.md"),
                   key=lambda p: (order.index(p.stem) if p.stem in order else len(order), p.stem))
    for p in files:
        title = re.sub(r"^ONX (Specification — )?", "", first_heading(p))
        out.append({"file": p.name, "title": title})
    return out


def adrs() -> list[dict]:
    out = []
    for p in sorted((ROOT / "docs" / "adr").glob("[0-9][0-9][0-9][0-9]-*.md")):
        head = first_heading(p)
        m = re.match(r"ADR-(\d{4})\s*[—:-]+\s*(.*)", head)
        if not m:
            raise SystemExit(f"{p.relative_to(ROOT)}: heading should read 'ADR-NNNN — Title'")
        if m.group(1) != p.name[:4]:
            raise SystemExit(f"{p.relative_to(ROOT)}: heading number ADR-{m.group(1)} != file number")
        status = "Unknown"
        sm = re.search(r"^\*\*Status:?\*\*:?\s*([A-Za-z]+(?: in part)?)", read(p), re.M)
        if sm:
            status = sm.group(1).capitalize()
        out.append({"num": m.group(1), "file": p.name, "title": m.group(2).strip(), "status": status})
    return out


def toml_value(path: Path, key: str) -> str:
    m = re.search(rf'^\s*{key}\s*=\s*"([^"]+)"', read(path), re.M)
    if not m:
        raise SystemExit(f"{path.relative_to(ROOT)}: no {key}")
    return m.group(1)


def rust_const(rel: str, name: str) -> str:
    m = re.search(rf"^\s*(?:pub(?:\([^)]*\))?\s+)?const {name}\s*:[^=]+=\s*(.+?);", read(ROOT / rel), re.M)
    if not m:
        raise SystemExit(f"{rel}: no const {name}")
    return m.group(1).strip()


def workspace_members() -> list[str]:
    body = read(ROOT / "Cargo.toml")
    block = re.search(r"members\s*=\s*\[(.*?)\]", body, re.S)
    return re.findall(r'"([^"]+)"', block.group(1)) if block else []


def current_milestone() -> dict:
    """The row of MILESTONES.md 'At a glance' marked ⏭ (next)."""
    for line in read(ROOT / "MILESTONES.md").splitlines():
        if line.startswith("|") and "⏭" in line:
            cells = [c.strip().strip("*").strip() for c in line.strip("|").split("|")]
            return {"id": cells[0], "name": cells[1], "version": cells[2].strip("`")}
    raise SystemExit("MILESTONES.md: no milestone row marked ⏭")


def done_milestones() -> list[str]:
    done = []
    for line in read(ROOT / "MILESTONES.md").splitlines():
        if line.startswith("| M") and "✅" in line:
            done.append(line.strip("|").split("|")[0].strip())
    return done


def format_done_milestones(done: list[str]) -> str:
    """'M0–M3, M5 done': done milestones rendered as contiguous runs, so a
    finished later milestone (M5) is not folded into an unfinished earlier
    one (M4)."""
    if not done:
        return "none done yet"
    nums = sorted(int(m[1:]) for m in done)
    runs = []
    start = prev = nums[0]
    for n in nums[1:]:
        if n == prev + 1:
            prev = n
        else:
            runs.append((start, prev))
            start = prev = n
    runs.append((start, prev))
    parts = [f"M{a}–M{b}" if a != b else f"M{a}" for a, b in runs]
    return ", ".join(parts) + " done"


def milestone_criteria(text: str, mid: str) -> tuple[int, int]:
    """(checked, total) exit-criteria boxes under the heading that names
    milestone `mid` ('### M4 — …', or '## Part 3 — … (M8, …)'), up to the
    next heading."""
    lines = text.splitlines()
    head = re.compile(rf"^#{{2,3}} .*\b{mid}\b")
    for i, line in enumerate(lines):
        if head.match(line):
            body = []
            for nxt in lines[i + 1:]:
                if nxt.startswith(("## ", "### ")):
                    break
                body.append(nxt)
            done = sum(1 for b in body if b.startswith("- [x]"))
            return done, done + sum(1 for b in body if b.startswith("- [ ]"))
    return 0, 0


def milestone_criteria_list(text: str, mid: str) -> list[dict]:
    """Every exit-criteria box under the milestone heading, in order.

    Each item: {"index": n, "text": "...", "checked": bool}. Multi-line
    boxes (continuation lines indented under the "- [ ]") are joined with
    single spaces. Same scope as milestone_criteria, so the two can't drift.
    """
    lines = text.splitlines()
    head = re.compile(rf"^#{{2,3}} .*\b{mid}\b")
    out: list[dict] = []
    for i, line in enumerate(lines):
        if not head.match(line):
            continue
        for nxt in lines[i + 1:]:
            if nxt.startswith(("## ", "### ")):
                break
            m = re.match(r"^- \[( |x)\] (.*)$", nxt)
            if m:
                out.append({"index": len(out),
                            "text": m.group(2).strip(),
                            "checked": m.group(1) == "x"})
            elif out and nxt.strip() and nxt[0] in " \t":
                out[-1]["text"] += " " + nxt.strip()
        break
    return out


def next_task() -> dict | None:
    """First unchecked exit criterion across Part 2 then Part 3.

    Returns {"milestone", "index", "text", "task_id"} or None when every
    box is checked. task_id is stable: milestone + 12 hex chars of the
    sha256 of the criterion text, so retries recognize the same task.
    """
    text = read(ROOT / "MILESTONES.md")
    for mid in ("M4", "M5", "M6", "M7", "M8"):
        for box in milestone_criteria_list(text, mid):
            if not box["checked"]:
                tid = hashlib.sha256(box["text"].encode()).hexdigest()[:12]
                return {"milestone": mid, "index": box["index"],
                        "text": box["text"], "task_id": f"{mid}-{tid}"}
    return None


def milestone_map() -> list[dict]:
    """Every M-row of MILESTONES.md 'At a glance', with its progress."""
    text = read(ROOT / "MILESTONES.md")
    rows = []
    for line in text.splitlines():
        cells = [c.strip() for c in line.strip().strip("|").split("|")]
        if not line.startswith("|") or not re.fullmatch(r"\**M\d+\**", cells[0]):
            continue
        mid = cells[0].strip("*")
        status = cells[3]
        date = re.search(r"\d{4}-\d{2}-\d{2}", status)
        met, total = milestone_criteria(text, mid)
        rows.append({
            "id": mid,
            "name": cells[1].replace("**", ""),
            "version": cells[2].replace("**", ""),
            "done": "✅" in status,
            "current": "⏭" in status,
            "date": date.group(0) if date else "",
            "met": met,
            "total": total,
        })
    if not rows:
        raise SystemExit("MILESTONES.md: no milestone rows in 'At a glance'")
    return rows


def milestone_problems(rows: list[dict]) -> list[str]:
    """A milestone with exit criteria is done exactly when all are checked,
    and the last row (the maintenance gate) must have criteria. Otherwise the
    README's phase and map could contradict MILESTONES.md."""
    problems = []
    if not rows[-1]["total"]:
        problems.append(f"MILESTONES.md: {rows[-1]['id']} (the maintenance gate) has no exit-criteria checkboxes")
    for r in rows:
        if not r["total"]:
            continue
        complete = r["met"] == r["total"]
        if r["done"] and not complete:
            problems.append(f"MILESTONES.md: {r['id']} is marked ✅ but {r['total'] - r['met']} of its exit criteria are unchecked")
        if complete and not r["done"]:
            problems.append(f"MILESTONES.md: every {r['id']} exit criterion is checked; mark its 'At a glance' row ✅ Done")
    return problems


def readme_marker_problems(text: str) -> list[str]:
    opens = text.count("<!--gen:readme_status-->")
    closes = text.count("<!--/gen:readme_status-->")
    if opens == 1 and closes == 1 and text.index("<!--gen:readme_status-->") < text.index("<!--/gen:readme_status-->"):
        return []
    return [f"README.md: needs exactly one <!--gen:readme_status-->…<!--/gen:readme_status--> region "
            f"(found {opens} opening, {closes} closing)"]


def explorer_network(name: str) -> dict:
    m = re.search(rf"{name}:\s*\{{[^}}]*chainId:\s*'([0-9a-f]{{64}})'[^}}]*files:\s*'([^']+)'", read(EXPLORER))
    if not m:
        raise SystemExit(f"explorer/index.html: no {name} network with chainId and files source")
    return {"chain_id": m.group(1), "source": m.group(2)}


def golden_roots() -> dict:
    src = read(ROOT / "crates/tooling/onx/tests/phase5_replay.rs")
    out = {}
    for key in ("GOLDEN_GENESIS_ROOT", "GOLDEN_ROOT_AFTER_3", "GOLDEN_ROOT_AFTER_5"):
        m = re.search(rf'const {key}: &str =\s*"([0-9a-f]{{64}})"', src)
        if not m:
            raise SystemExit(f"phase5_replay.rs: no {key}")
        out[key] = m.group(1)
    return out


def derived() -> dict:
    s, a = specs(), adrs()
    license_adr = next(x for x in a if "licen" in x["file"])
    return {
        "specs": s,
        "adrs": a,
        "version": toml_value(ROOT / "Cargo.toml", "version"),
        "rust": toml_value(ROOT / "rust-toolchain.toml", "channel"),
        "crates": len(workspace_members()),
        "license_adr": license_adr,
        "block_magic": rust_const("crates/tooling/onx/src/blockfile.rs", "BLOCK_FILE_MAGIC").removeprefix('b"').removeprefix('&b"').strip('"'),
        "header_len": int(rust_const("crates/protocol/onx-stf/src/block.rs", "BLOCK_HEADER_BYTE_LEN")),
        "protocol_version": int(rust_const("crates/protocol/onx-stf/src/block.rs", "PROTOCOL_VERSION")),
        "schema_version": int(rust_const("crates/protocol/onx-storage/src/store.rs", "SCHEMA_VERSION")),
        "milestone": current_milestone(),
        "done": done_milestones(),
        "map": milestone_map(),
        "devnet": explorer_network("testnet"),
        "golden": golden_roots(),
    }


# --------------------------------------------------------------------------
# Measured facts (site/state.json)
# --------------------------------------------------------------------------

def load_state() -> dict:
    return json.loads(read(STATE)) if STATE.exists() else {}


def head_commit() -> dict:
    sha = git("rev-parse", "HEAD")
    date = git("show", "-s", "--format=%cs", "HEAD")
    return {"commit": sha, "date": date}


def parse_tests_log(text: str) -> dict:
    """Sum the `test result:` lines of `cargo test` output."""
    total = {"passed": 0, "failed": 0, "ignored": 0, "binaries": 0}
    for m in re.finditer(r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored", text):
        total["passed"] += int(m.group(1))
        total["failed"] += int(m.group(2))
        total["ignored"] += int(m.group(3))
        total["binaries"] += 1
    if not total["binaries"]:
        raise SystemExit("tests log: no 'test result:' lines (pass the output of cargo test)")
    return total


def parse_replay_log(text: str) -> list[str]:
    """The `onx replay` stdout printed by replay_prints_vectors_for_freezing."""
    lines = [l.strip() for l in text.splitlines()
             if re.match(r"^\s*(chain_id|genesis_root|seqno|final_seqno|final_block_hash|final_state_root)=", l)]
    if not any(l.startswith("final_state_root=") for l in lines):
        raise SystemExit("replay log: no final_state_root line. Produce it with:\n"
                         "  cargo test -p onx --test phase5_replay replay_prints_vectors_for_freezing -- --nocapture")
    return lines


# --------------------------------------------------------------------------
# Rendering
# --------------------------------------------------------------------------

def link(path: str, text: str, tree: bool = False) -> str:
    return f'<a href="{(TREE if tree else BLOB) + path}">{esc(text)}</a>'


def render(d: dict, st: dict) -> dict[str, str]:
    a = d["adrs"]
    accepted = [x for x in a if x["status"] == "Accepted"]
    other = [x for x in a if x["status"] != "Accepted"]
    other_note = ", ".join(f"ADR-{x['num']} {x['status'].lower()}" for x in other)
    tests = st.get("tests")
    replay = st.get("replay")
    reviewed = st.get("reviewed", {})
    ms = d["milestone"]
    r: dict[str, str] = {}

    r["spec_count"] = str(len(d["specs"]))
    r["adr_count"] = str(len(a))
    r["adr_summary"] = (f"{len(a)} decision records, {len(accepted)} accepted"
                        + (f" ({other_note})" if other else "") + ".")
    r["version"] = d["version"]
    r["rust"] = d["rust"]
    r["crates"] = str(d["crates"])
    r["license_adr"] = link(f"docs/adr/{d['license_adr']['file']}", f"ADR-{d['license_adr']['num']}")
    r["block_magic"] = d["block_magic"]
    r["block_format"] = (f"<code>{d['block_magic']}</code> blocks, {d['header_len']}-byte header, "
                         f"protocol version {d['protocol_version']}, storage schema {d['schema_version']}")
    r["milestone"] = f"{ms['id']}: {esc(ms['name'])}"
    r["milestone_short"] = esc(ms["name"])
    r["milestones_done"] = format_done_milestones(d["done"])
    r["devnet_chain_id"] = d["devnet"]["chain_id"]
    r["devnet_source"] = esc(d["devnet"]["source"])

    r["spec_list"] = "".join(
        f'<li><a href="{BLOB}docs/specification/{s["file"]}"><span>{esc(s["title"])}</span>'
        f'<span class="k">{s["file"]}</span></a></li>' for s in d["specs"])
    r["adr_list"] = "".join(
        f'<li><a href="{BLOB}docs/adr/{x["file"]}"><span>{esc(x["title"])}'
        + ("" if x["status"] == "Accepted" else f' <em>({x["status"].lower()})</em>')
        + f'</span><span class="k">ADR-{x["num"]}</span></a></li>' for x in a)

    if tests:
        short = tests["commit"][:7]
        r["tests_count"] = fmt_int(tests["passed"])
        r["tests_value"] = f"{fmt_int(tests['passed'])} passed, {tests['failed']} failed"
        r["tests_note"] = (f"Local run of <code>cargo test --workspace --all-targets</code> at "
                           f"<a href=\"{REPO}/commit/{tests['commit']}\">{short}</a> ({tests['date']}, "
                           f"Rust {esc(tests.get('toolchain', d['rust']))}): {tests['binaries']} test binaries, "
                           f"{tests['ignored']} ignored.")
        r["tested_commit"] = f"<a href=\"{REPO}/commit/{tests['commit']}\">{short}</a>"
    if reviewed:
        r["reviewed_sha"] = reviewed["commit"][:7]
        r["reviewed_date"] = reviewed["date"]
        r["reviewed_foot"] = (f"Statuses on this page were reviewed against commit "
                              f"{reviewed['commit'][:7]} ({reviewed['date']}).")
        r["snapshot"] = f"'{reviewed['commit']}'"
    if replay:
        r.update(render_replay(replay, d["golden"]))
    return r


def render_replay(replay: dict, golden: dict) -> dict[str, str]:
    kv, blocks = {}, []
    for line in replay["lines"]:
        if line.startswith("seqno="):
            parts = dict(p.split("=", 1) for p in line.split())
            blocks.append(parts)
        else:
            k, v = line.split("=", 1)
            kv[k] = v
    if kv.get("genesis_root") != golden["GOLDEN_GENESIS_ROOT"] or kv.get("final_state_root") != golden["GOLDEN_ROOT_AFTER_5"]:
        raise SystemExit("replay log disagrees with the golden roots in phase5_replay.rs; regenerate it")
    def lane(name: str, who: str, what: str) -> str:
        cells = "".join(
            f'<div class="cell is-done{" is-final" if i == len(blocks) - 1 else ""}" data-seq="{b["seqno"]}">'
            f'<span class="s">#{b["seqno"]}</span><span class="r">{b["root"][:8]}</span></div>'
            for i, b in enumerate(blocks))
        return f'<div class="lane" data-lane="{name}"><div class="lane__who"><strong>{who}</strong>{what}</div>{cells}</div>'
    out = {
        "replay_chain_id": kv["chain_id"],
        "replay_genesis_root": kv["genesis_root"],
        "replay_lanes": "\n          ".join([
            lane("a", "Process A", "first run, fresh data directory"),
            lane("b", "Process B", "second run, separate process"),
            lane("c", "Process C", "kill -9 during a commit, then resume")]),
        "replay_final_seqno": kv["final_seqno"],
        "replay_final_block": kv["final_block_hash"],
        "replay_final_root": kv["final_state_root"],
        "replay_final_short": kv["final_state_root"][:8],
        "replay_commit": f"<a href=\"{REPO}/commit/{replay['commit']}\">{replay['commit'][:7]}</a>",
        "replay_terminal": "\n".join(esc(l) for l in replay["lines"]),
        # The page's script animates the same replay; keep it on the same data.
        "replay_roots_js": "[" + ", ".join(f"'{b['root'][:8]}'" for b in blocks) + "]",
        "replay_final_js": f"'{kv['final_state_root']}'",
    }
    return out


def bar(met: int, total: int, width: int = 10) -> str:
    filled = round(width * met / total) if total else 0
    return "█" * filled + "░" * (width - filled)


def render_readme(d: dict) -> dict[str, str]:
    """The README's status block: phase, current milestone, milestone map.
    No target dates: a milestone is done when its exit criteria are."""
    rows = d["map"]
    gate = rows[-1]
    current = next((r for r in rows if r["current"]), next((r for r in rows if not r["done"]), gate))
    if gate["done"] and gate["total"] and gate["met"] == gate["total"]:
        phase = ("**Phase: maintenance.** The maintenance gate (M8) is passed: the scope "
                 "is finished and changes are fixes, security, and compatible improvements.")
    else:
        phase = ("**Phase: development.** ONX is being built, not maintained. It switches "
                 "to maintenance when every box in the "
                 "[maintenance gate](MILESTONES.md#part-3--the-maintenance-gate-m8-100) "
                 f"({gate['id']}, `{gate['version'].strip('`')}`) is checked.")
    now = f"**Now:** {current['id']}, *{current['name']}* (`{current['version'].strip('`')}`)"
    if current["total"]:
        now += f": {current['met']} of {current['total']} exit criteria met."
    else:
        now += "."

    def mark(r: dict) -> str:
        if r["done"]:
            return "✅"
        if r is current:
            return "▶"
        return "🏁" if r is gate else "○"

    line = " ── ".join(f"{r['id']} {mark(r)}" for r in rows)
    table = ["| | Milestone | Version | Progress |", "| :-: | --- | --- | --- |"]
    for r in rows:
        name = f"{r['id']} · {r['name']}"
        if r is current:
            name = f"**{name}**"
        if r["done"]:
            progress = f"done {r['date']}" if r["date"] else "done"
        elif r["total"]:
            progress = f"`{bar(r['met'], r['total'])}` {r['met']}/{r['total']}"
        else:
            progress = "criteria not written yet"
        table.append(f"| {mark(r)} | {name} | {r['version']} | {progress} |")
    block = "\n".join([
        "",
        phase,
        "",
        now,
        "",
        "```text",
        line,
        "```",
        "",
        *table,
        "",
        "Progress counts the exit-criteria checkboxes in [`MILESTONES.md`](MILESTONES.md); "
        "a box is ticked in the PR that earns it. There are no target dates. This block is "
        "generated by `scripts/site.py build`, and CI fails when it falls behind.",
        "",
    ])
    return {"readme_status": block}


GEN = re.compile(
    r"(?P<open><!--gen:(?P<k1>[a-z0-9_]+)-->|/\*gen:(?P<k2>[a-z0-9_]+)\*/)"
    r"(?P<body>.*?)"
    r"(?P<close><!--/gen:(?P=k1)-->|/\*/gen:(?P=k2)\*/)",
    re.S,
)


def apply(page: str, values: dict[str, str]) -> tuple[str, list[str]]:
    missing = []

    def sub(m: re.Match) -> str:
        key = m.group("k1") or m.group("k2")
        if key not in values:
            missing.append(key)
            return m.group(0)
        return m.group("open") + values[key] + m.group("close")

    out = GEN.sub(sub, page)
    # Feed header: number of posts.
    posts = len(re.findall(r'<article class="post"', out))
    out = re.sub(r"(<!--gen:post_count-->)\d+(<!--/gen:post_count-->)", rf"\g<1>{posts}\g<2>", out)
    return out, sorted(set(missing) - {"post_count"})


# --------------------------------------------------------------------------
# Static checks
# --------------------------------------------------------------------------

def repo_links(page: str) -> list[str]:
    problems = []
    for url in sorted(set(re.findall(r'href="(https://github\.com/gokooteam/The-Open-Network-X/(?:blob|tree)/main/[^"#?]*)', page))):
        path = re.sub(r"^.*/(?:blob|tree)/main/", "", url)
        if not (ROOT / path).exists():
            problems.append(f"link to a path that doesn't exist: {path}")
    return problems


def retired_terms(page: str, label: str) -> list[str]:
    problems = []
    # Generated regions quote repository files verbatim (ADR-0033's own title
    # says "Onyx → Onyxi"), so only hand-written text is scanned.
    text = GEN.sub("", page)
    for allowed in RETIRED_ALLOWED_IN:
        text = re.sub(rf"<!--{allowed}-->.*?<!--/{allowed}-->", "", text, flags=re.S)
    for pattern, reason in RETIRED:
        for m in re.finditer(pattern, text):
            line = text.count("\n", 0, m.start()) + 1
            problems.append(f"{label}: retired term {m.group(0)!r} near line {line}: {reason}")
    return problems


def probe_console_tests(page: str) -> list[str]:
    problems = []
    tmap = re.search(r"const T = \{(.*?)\};", page, re.S)
    if not tmap:
        return ["site: probe console file map (const T) not found"]
    files = dict(re.findall(r"(\w+): '([^']+)'", tmap.group(1)))
    for name, key in re.findall(r"test: \['(\w+)', T\.(\w+)\]", page):
        rel = files.get(key)
        if not rel or not (ROOT / rel).exists():
            problems.append(f"site: probe console test {name}: file for T.{key} missing")
        elif not re.search(rf"\bfn {name}\b", read(ROOT / rel)):
            problems.append(f"site: probe console names test {name}, which is not in {rel}")
    return problems


def explorer_constants(d: dict) -> list[str]:
    page = read(EXPLORER)
    problems = []
    fmt = re.search(r"const FORMAT = Object\.freeze\(\{(.*?)\}\);", page, re.S)
    tag = re.search(r"const TAG = Object\.freeze\(\{(.*?)\}\);", page, re.S)
    if not fmt or not tag:
        return ["explorer: FORMAT or TAG table not found"]
    f = dict(re.findall(r"(\w+):\s*'?([\w.]+?)'?n?,", fmt.group(1)))
    expect = {
        "BLOCK_MAGIC": d["block_magic"],
        "HEADER_LEN": str(d["header_len"]),
        "CELL_MAX_DATA": rust_const("crates/protocol/onx-state-model/src/cell.rs", "MAX_CELL_DATA_BYTES"),
        "CELL_MAX_REFS": rust_const("crates/protocol/onx-state-model/src/cell.rs", "MAX_CELL_REFS"),
        "EMBEDDED_CELL_MAX": rust_const("crates/protocol/onx-state-model/src/account.rs", "MAX_EMBEDDED_CELL_BYTES"),
        "MSG_MAX_PAYLOAD": rust_const("crates/protocol/onx-stf/src/message.rs", "MAX_MESSAGE_BYTES").replace("_", ""),
        "GAS_PER_NANO": rust_const("crates/protocol/onx-stf/src/stf.rs", "GAS_PER_NANO").replace("_", ""),
    }
    for k, v in expect.items():
        if f.get(k) != v:
            problems.append(f"explorer: FORMAT.{k} is {f.get(k)!r}, the code says {v!r}")
    rust_tags = set()
    for p in (ROOT / "crates").rglob("*.rs"):
        if "/tests/" in str(p):
            continue
        rust_tags.update(re.findall(r'"(ONX_[A-Z0-9_]+)"', read(p)))
    for name, value in re.findall(r"(\w+): '(ONX_[A-Z0-9_]+)'", tag.group(1)):
        if value not in rust_tags:
            problems.append(f"explorer: TAG.{name} = {value!r} is not a domain tag in the Rust code")
    # The explorer's in-browser self-test must use the same hand-derived
    # vectors as the Rust tests it cites, or a format change leaves every
    # visitor looking at "Self-test failed" (as happened after ONXBLK05).
    vec = re.search(r"const V = \{(.*?)\};", page, re.S)
    if not vec:
        return problems + ["explorer: self-test vector table (const V) not found"]
    v = dict(re.findall(r"(\w+): '([0-9a-f]+)'", vec.group(1)))
    sources = {
        "crates/protocol/onx-stf/tests/hand_derived_vectors.rs": {
            "msgWire": "HAND_MSG_WIRE_HEX", "msgHash": "HAND_MSG_HASH_HEX", "msgsRoot": "HAND_MSGS_ROOT_HEX",
            "header": "HAND_HEADER_HEX", "headerHash": "HAND_HEADER_HASH_HEX", "pubG": "HAND_G_PUBKEY_HEX",
            "sigG": "HAND_G_SIGNATURE_HEX", "msgHashG": "HAND_G_MSG_HASH_HEX", "addrG": "HAND_G_ADDRESS_HEX"},
        "crates/protocol/onx-state-model/tests/hand_derived_cell.rs": {"cellHash": "HAND_CELL_HASH_HEX"},
    }
    for rel, pairs in sources.items():
        src = read(ROOT / rel)
        for js, rs in pairs.items():
            m = re.search(rf'const {rs}: &str =\s*"([0-9a-f]+)"', src)
            if not m:
                problems.append(f"explorer: self-test vector {js}: {rs} not found in {rel}")
            elif v.get(js) != m.group(1):
                problems.append(f"explorer: self-test vector V.{js} differs from {rs} in {rel}")
    return problems


def state_commits(st: dict) -> list[str]:
    problems = []
    for key in ("tests", "replay", "reviewed"):
        c = st.get(key, {}).get("commit")
        if not c:
            problems.append(f"site/state.json: no {key}.commit")
            continue
        try:
            git("cat-file", "-e", f"{c}^{{commit}}")
        except subprocess.CalledProcessError:
            # Shallow CI checkouts may not have it; say so instead of failing.
            print(f"note: {key} commit {c[:7]} is not in this checkout's history (shallow clone?)")
    t = st.get("tests")
    if t and t.get("failed"):
        problems.append(f"site/state.json: the recorded test run has {t['failed']} failures; the page would advertise them")
    return problems


# --------------------------------------------------------------------------
# Live comparison
# --------------------------------------------------------------------------

def fetch(url: str) -> tuple[str, bytes | str]:
    req = urllib.request.Request(url, headers={"User-Agent": "onx-site-check/1", "Cache-Control": "no-cache"})
    ctx = ssl.create_default_context()
    try:
        with urllib.request.urlopen(req, timeout=20, context=ctx) as res:
            return "ok", res.read()
    except ssl.SSLError as e:
        return "tls", str(e)
    except urllib.error.URLError as e:
        reason = e.reason
        if isinstance(reason, ssl.SSLError) or "CERTIFICATE" in str(reason).upper():
            return "tls", str(reason)
        return "error", str(reason)
    except Exception as e:  # noqa: BLE001 - report anything else as an error
        return "error", str(e)


def normalise(body: bytes) -> bytes:
    """Compare bytes, ignoring only line endings and surrounding whitespace.

    The deployed file must be the repository file. A host that wraps it in
    its own <html>/<head> (as one of on-x-scan.com's servers did) is drift.
    """
    return body.replace(b"\r\n", b"\n").strip()


def live(attempts: int) -> tuple[dict, int]:
    report, failures = {}, 0
    for name, dep in DEPLOYMENTS.items():
        want = normalise(dep["file"].read_bytes())
        want_sha = hashlib.sha256(want).hexdigest()
        entry = {"file": str(dep["file"].relative_to(ROOT)), "sha256": want_sha, "urls": {}}
        for url in dep["urls"]:
            seen = []
            # Several attempts: a domain with more than one A record can serve
            # different content (or certificates) from each server.
            for _ in range(attempts):
                kind, body = fetch(url)
                if kind == "ok":
                    got = hashlib.sha256(normalise(body)).hexdigest()
                    seen.append("match" if got == want_sha else f"differs (sha256 {got[:12]})")
                else:
                    seen.append(f"{kind}: {body[:120]}")
            entry["urls"][url] = sorted(set(seen))
            if any(s != "match" for s in seen):
                failures += 1
        report[name] = entry
    return report, failures


# --------------------------------------------------------------------------
# Commands
# --------------------------------------------------------------------------

def cmd_build(args) -> int:
    st = load_state()
    if args.tests_log:
        t = parse_tests_log(read(Path(args.tests_log)))
        t.update(head_commit())
        t["toolchain"] = toml_value(ROOT / "rust-toolchain.toml", "channel")
        st["tests"] = t
    if args.replay_log:
        st["replay"] = {"lines": parse_replay_log(read(Path(args.replay_log))), **head_commit()}
    if args.reviewed:
        st["reviewed"] = head_commit()
    STATE.write_text(json.dumps(st, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    page, missing = apply(read(SITE), render(derived(), st))
    if missing:
        print("No value for generated region(s): " + ", ".join(missing), file=sys.stderr)
        return 1
    SITE.write_text(page, encoding="utf-8")
    d = derived()
    bad = readme_marker_problems(read(README)) + milestone_problems(d["map"])
    if bad:
        print("\n".join(bad), file=sys.stderr)
        return 1
    readme, missing = apply(read(README), render_readme(d))
    if missing:
        print("README.md: no value for generated region(s): " + ", ".join(missing), file=sys.stderr)
        return 1
    README.write_text(readme, encoding="utf-8")
    print(f"site/index.html regenerated ({len(page):,} bytes); README.md status updated.")
    return 0


def cmd_check(_args) -> int:
    d, st = derived(), load_state()
    page = read(SITE)
    fresh, missing = apply(page, render(d, st))
    problems = []
    if missing:
        problems.append("site: no value for generated region(s): " + ", ".join(missing))
    if fresh != page:
        problems.append("site/index.html is out of date with the repository. Run: python3 scripts/site.py build")
    readme = read(README)
    problems += readme_marker_problems(readme)
    problems += milestone_problems(d["map"])
    fresh_readme, missing = apply(readme, render_readme(d))
    if missing:
        problems.append("README.md: no value for generated region(s): " + ", ".join(missing))
    if fresh_readme != readme:
        problems.append("README.md's status block is out of date with MILESTONES.md. Run: python3 scripts/site.py build")
    problems += repo_links(page)
    problems += [f"explorer: {p}" for p in repo_links(read(EXPLORER))]
    problems += retired_terms(page, "site")
    problems += retired_terms(read(EXPLORER), "explorer")
    problems += probe_console_tests(page)
    problems += explorer_constants(d)
    problems += state_commits(st)
    if problems:
        print("Site check failed:")
        print("\n".join(f"- {p}" for p in problems))
        return 1
    print("Site check passed: generated facts current, links resolve, explorer constants match the code.")
    return 0


def cmd_milestones(_args) -> int:
    """Print the milestone map as JSON; milestones.yml syncs GitHub
    milestones from it."""
    print(json.dumps({"milestones": milestone_map()}, indent=2, ensure_ascii=False))
    return 0


def cmd_next_task(_args) -> int:
    """Print the next milestone-task-chain task as JSON (null when done)."""
    print(json.dumps({"task": next_task()}, indent=2, ensure_ascii=False))
    return 0


def cmd_live(args) -> int:
    report, failures = live(args.attempts)
    if args.json:
        Path(args.json).write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    for name, entry in report.items():
        print(f"{name}  (repo {entry['file']}, sha256 {entry['sha256'][:12]})")
        for url, seen in entry["urls"].items():
            mark = "OK " if seen == ["match"] else "!! "
            print(f"  {mark}{url}: {'; '.join(seen)}")
    if failures:
        print(f"\n{failures} URL(s) serve something other than the file in this checkout, or failed TLS.")
        return 1
    print("\nEvery URL serves exactly the file in this checkout.")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("build", help="regenerate site/index.html and README.md's status from the repository")
    b.add_argument("--tests-log", help="output of `cargo test --workspace --all-targets` at HEAD")
    b.add_argument("--replay-log", help="output of replay_prints_vectors_for_freezing --nocapture at HEAD")
    b.add_argument("--reviewed", action="store_true",
                   help="record HEAD as the commit whose statuses you just reviewed by hand")
    sub.add_parser("check", help="fail if the site or explorer disagrees with the repository")
    lv = sub.add_parser("live", help="compare the deployed sites with this checkout")
    lv.add_argument("--attempts", type=int, default=4, help="fetches per URL (default 4)")
    lv.add_argument("--json", help="also write the report as JSON")
    sub.add_parser("milestones", help="print the milestone map from MILESTONES.md as JSON")
    sub.add_parser("next-task", help="print the first unchecked exit criterion (task chain) as JSON")
    args = ap.parse_args()
    return {"build": cmd_build, "check": cmd_check, "live": cmd_live,
            "milestones": cmd_milestones, "next-task": cmd_next_task}[args.cmd](args)


if __name__ == "__main__":
    sys.exit(main())
