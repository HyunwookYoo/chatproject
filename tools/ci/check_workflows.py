#!/usr/bin/env python3
"""Static checks for .github/workflows/*.yml (public repo rules, M1b plan Global Constraints).

Prints one line per problem and exits 1 if there is any.
"""
import pathlib
import re
import sys

import yaml

ROOT = pathlib.Path(__file__).resolve().parents[2]
PINNED = re.compile(r"^[\w.-]+/[\w.-]+(/[\w./-]+)?@[0-9a-f]{40}$")


def triggers(doc):
    # PyYAML (YAML 1.1) reads the bare key `on` as True.
    on = doc.get("on", doc.get(True))
    if isinstance(on, str):
        return {on: None}
    if isinstance(on, list):
        return {name: None for name in on}
    return on or {}


def steps(doc):
    for job_id, job in (doc.get("jobs") or {}).items():
        for index, step in enumerate(job.get("steps") or []):
            yield job_id, index, job, step


def check(path, doc):
    name = path.name
    problems = []
    if "pull_request_target" in triggers(doc) or "workflow_run" in triggers(doc):
        problems.append(f"{name}: pull_request_target/workflow_run is not allowed in a public repo")
    if doc.get("permissions") != {"contents": "read"}:
        problems.append(f"{name}: top-level permissions must be exactly 'contents: read'")
    env = doc.get("env") or {}
    if "CARGOKIT_TOOLCHAIN" in env and str(env["CARGOKIT_TOOLCHAIN"]) != str(env.get("RUST_TOOLCHAIN")):
        problems.append(f"{name}: CARGOKIT_TOOLCHAIN must equal RUST_TOOLCHAIN")
    for job_id, index, job, step in steps(doc):
        uses = step.get("uses")
        if not uses:
            continue
        if not PINNED.match(uses):
            problems.append(f"{name}: {job_id} step {index}: '{uses}' is not pinned to a full commit SHA")
        if uses.startswith("actions/checkout@") and (step.get("with") or {}).get("persist-credentials") is not False:
            problems.append(f"{name}: {job_id} step {index}: checkout must set persist-credentials: false")
    return problems


def main():
    problems = []
    for path in sorted((ROOT / ".github" / "workflows").glob("*.yml")):
        problems += check(path, yaml.safe_load(path.read_text(encoding="utf-8")))
    if problems:
        print("\n".join(problems))
        return 1
    print("workflows ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
