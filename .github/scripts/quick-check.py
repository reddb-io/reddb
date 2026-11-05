#!/usr/bin/env python3
"""Fast repository sanity and the on-demand Actions cost policy."""
import json
from pathlib import Path
import tomllib
import yaml

root = Path.cwd()
metadata = ['red-issues-needs-triage.yml']
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
    events = workflow.get("on", workflow.get(True, {}))
    if isinstance(events, str):
        events = {events: None}
    elif isinstance(events, list):
        events = dict.fromkeys(events)
    if not isinstance(events, dict):
        raise SystemExit(f"{path}: expected workflow triggers")
    if path.name == "quick-checks.yml":
        continue
    if "schedule" in events:
        raise SystemExit(f"{path}: schedules are opt-in; use workflow_dispatch")
    if path.name in metadata:
        continue
    if any(event in events for event in ["pull_request", "pull_request_target", "merge_group"]):
        raise SystemExit(f"{path}: quality checks must be manually dispatched")
    if "push" in events:
        push = events["push"] or {}
        if not isinstance(push, dict) or not push.get("tags") or push.get("branches"):
            raise SystemExit(f"{path}: only intentional release-tag pushes are automatic")
print("Workflow syntax, manifests and on-demand CI policy passed.")
