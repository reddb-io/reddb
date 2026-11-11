#!/usr/bin/env python3
"""Fast repository sanity and the Actions runner policy."""
import json
from pathlib import Path
import re
import tomllib
import yaml

root = Path.cwd()
metadata = ['red-issues-needs-triage.yml']
automatic_validation = {'ci.yml', 'ci-docs.yml', 'quick-checks.yml'}
for path in [root / "package.json", *root.glob("packages/*/package.json"), *root.glob("apps/*/package.json")]:
    if path.is_file():
        json.loads(path.read_text(encoding="utf-8-sig"))
for name in ["Cargo.toml", "pyproject.toml"]:
    path = root / name
    if path.is_file():
        tomllib.loads(path.read_text())
for path in sorted((root / ".github/workflows").glob("*.y*ml")):
    workflow = yaml.safe_load(path.read_text())
    if not isinstance(workflow, dict) or not isinstance(workflow.get("jobs"), dict):
        raise SystemExit(f"{path}: expected a workflow with jobs")
    if path.name != 'blacksmith-testbox.yml' and re.search(r'blacksmith-\d+vcpu-', json.dumps(workflow)):
        raise SystemExit(f"{path}: use standard GitHub-hosted runners for this public repository")
    events = workflow.get("on", workflow.get(True, {}))
    if isinstance(events, str):
        events = {events: None}
    elif isinstance(events, list):
        events = dict.fromkeys(events)
    if not isinstance(events, dict):
        raise SystemExit(f"{path}: expected workflow triggers")
    if path.name == 'blacksmith-testbox.yml':
        manual = events.get('workflow_dispatch') or {}
        if set(events) != {'workflow_dispatch'} or manual.get('inputs', {}).get('testbox_id', {}).get('required') is not True:
            raise SystemExit(f"{path}: paid Testbox comparisons require an explicit manual session")
    if "schedule" in events:
        raise SystemExit(f"{path}: schedules are opt-in; use workflow_dispatch")
    if path.name in metadata:
        continue
    automatic_events = {"pull_request", "pull_request_target", "merge_group"} & set(events)
    if automatic_events and path.name not in automatic_validation:
        raise SystemExit(f"{path}: automatic validation is restricted to the core CI allowlist")
    if "push" in events and path.name not in automatic_validation:
        push = events["push"] or {}
        if not isinstance(push, dict) or not push.get("tags") or push.get("branches"):
            raise SystemExit(f"{path}: only intentional release-tag pushes are automatic")
print("Workflow syntax, manifests and runner policy passed.")
