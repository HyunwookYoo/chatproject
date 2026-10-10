#!/usr/bin/env python3
"""Static checks for .github/workflows/*.yml and *.yaml (public repo rules, M1b plan Global Constraints).

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
    if path.stem == "testflight":  # GitHub runs testflight.yaml too
        problems += check_testflight(name, doc)
    return problems


def check_testflight(name, doc):
    """The release job holds the App Store signing secrets (Review Focus 5)."""
    problems = []
    trig = triggers(doc)
    if set(trig) - {"workflow_dispatch", "push"}:
        problems.append(f"{name}: triggers must be workflow_dispatch and push (tags) only")
    # A bare `push:` (or `on: push`) has no filter and starts the job for every branch and tag.
    if "push" in trig and trig["push"] != {"tags": ["testflight-*"]}:
        problems.append(f"{name}: push may only be for tags testflight-*")
    for job_id, job in (doc.get("jobs") or {}).items():
        if job.get("environment") != "testflight":
            problems.append(f"{name}: job {job_id} must use environment: testflight")
        step_list = job.get("steps") or []
        names = [step.get("name", "") for step in step_list]
        if not names or names[0] != "Refuse re-runs":
            problems.append(f"{name}: job {job_id} must start with the 'Refuse re-runs' step")
        guard = next((s for s in step_list if s.get("name") == "Refuse re-runs"), None)
        if guard is not None and not all(k in guard.get("run", "") for k in ("GITHUB_RUN_ATTEMPT", "exit 1")):
            problems.append(f"{name}: job {job_id}: the 'Refuse re-runs' step must test GITHUB_RUN_ATTEMPT and exit 1")
        profiles = next((s for s in step_list if s.get("name") == "Install provisioning profiles"), None)
        if profiles is None or not all(k in profiles.get("run", "") for k in
                                        ("ProvisionedDevices", "application-groups", "aps-environment")):
            problems.append(f"{name}: job {job_id} must validate the profiles before signing")
        for step in step_list:
            if "upload-artifact" in step.get("uses", ""):
                problems.append(f"{name}: job {job_id} must not upload-artifact (IPA and profiles stay private)")
    return problems


def main():
    # GitHub runs both .yml and .yaml files from this directory.
    paths = sorted(
        path
        for path in (ROOT / ".github" / "workflows").glob("*")
        if path.suffix in (".yml", ".yaml")
    )
    problems = []
    if not paths:
        problems.append("no workflow files found in .github/workflows")
    for path in paths:
        problems += check(path, yaml.safe_load(path.read_text(encoding="utf-8")))
    if problems:
        print("\n".join(problems))
        return 1
    print("workflows ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
