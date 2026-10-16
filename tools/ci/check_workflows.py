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
    if not isinstance(doc, dict):
        return [f"{name}: workflow must be a YAML mapping"]
    problems = []
    allowed = {"push", "pull_request", "workflow_dispatch"}
    if path.stem.lower() == "e2e":
        allowed = {"workflow_dispatch", "schedule"}
    if set(triggers(doc)) - allowed:
        problems.append(f"{name}: unsupported workflow trigger")
    if doc.get("permissions") != {"contents": "read"}:
        problems.append(f"{name}: top-level permissions must be exactly 'contents: read'")
    env = doc.get("env") or {}
    if "CARGOKIT_TOOLCHAIN" in env and str(env["CARGOKIT_TOOLCHAIN"]) != str(env.get("RUST_TOOLCHAIN")):
        problems.append(f"{name}: CARGOKIT_TOOLCHAIN must equal RUST_TOOLCHAIN")
    for job_id, job in (doc.get("jobs") or {}).items():
        # Direct runner references are unavailable here; resolve them in a step.
        for value in (job.get("env") or {}).values():
            if isinstance(value, str) and re.search(r"\$\{\{\s*runner\s*[.\[]", value):
                problems.append(f"{name}: job {job_id}: runner context is unavailable in job env")
        if "permissions" in job:
            permissions = job["permissions"]
            if (not isinstance(permissions, dict) or set(permissions) - {"contents"}
                    or any(value not in ("read", "none") for value in permissions.values())):
                problems.append(f"{name}: job {job_id} permissions may only narrow contents: read")
        uses = job.get("uses")
        if uses and not uses.startswith("./.github/workflows/") and not PINNED.fullmatch(uses):
            problems.append(f"{name}: job {job_id}: '{uses}' is not pinned to a full commit SHA")
    for job_id, index, job, step in steps(doc):
        uses = step.get("uses")
        if not uses:
            continue
        if not uses.startswith("./") and not PINNED.fullmatch(uses):
            problems.append(f"{name}: {job_id} step {index}: '{uses}' is not pinned to a full commit SHA")
        if uses.startswith("actions/checkout@") and (step.get("with") or {}).get("persist-credentials") is not False:
            problems.append(f"{name}: {job_id} step {index}: checkout must set persist-credentials: false")
    if path.stem.lower() == "ci":
        problems += check_ci(name, doc)
    if path.stem.lower() == "testflight":  # GitHub runs testflight.yaml too
        problems += check_testflight(name, doc)
    return problems


def check_ci(name, doc):
    """Nothing but testflight.yml (which runs for a tag only) compiles the Release configuration for a device."""
    job = (doc.get("jobs") or {}).get("ios-release")
    if job is None:
        return [f"{name}: job ios-release is missing (CI would never build the Release configuration for a device)"]
    runs = "\n".join(step.get("run", "") for step in job.get("steps") or [])
    problems = []
    if "flutter build ios --release" not in runs or "--no-codesign" not in runs:
        problems.append(f"{name}: job ios-release must run 'flutter build ios --release --no-codesign'")
    if "tools/ci/check_ios_app.sh" not in runs:
        problems.append(f"{name}: job ios-release must run tools/ci/check_ios_app.sh on the built app")
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
        # The IPA check is the last guard before the upload: it must exist, check the signed entitlements
        # too, and come first.
        ipa = next((i for i, s in enumerate(step_list) if s.get("name") == "Locate IPA and check its contents"), None)
        upload = next((i for i, s in enumerate(step_list) if "upload-testflight-build" in s.get("uses", "")), None)
        if ipa is None or "tools/ci/check_ios_app.sh --signed" not in step_list[ipa].get("run", ""):
            problems.append(f"{name}: job {job_id} must run 'tools/ci/check_ios_app.sh --signed' on the exported IPA")
        elif upload is not None and upload < ipa:
            problems.append(f"{name}: job {job_id} must check the IPA before it uploads it")
        for step in step_list:
            if "upload-artifact" in step.get("uses", ""):
                problems.append(f"{name}: job {job_id} must not upload-artifact (IPA and profiles stay private)")
    return problems


def main(root=ROOT):
    # GitHub runs both .yml and .yaml files from this directory.
    paths = sorted(
        path
        for path in (root / ".github" / "workflows").glob("*")
        if path.suffix.lower() in (".yml", ".yaml")
    )
    problems = []
    if not paths:
        problems.append("no workflow files found in .github/workflows")
    for path in paths:
        try:
            doc = yaml.safe_load(path.read_text(encoding="utf-8"))
        except yaml.YAMLError:
            problems.append(f"{path.name}: invalid YAML")
            continue
        problems += check(path, doc)
    if problems:
        print("\n".join(problems))
        return 1
    print("workflows ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
