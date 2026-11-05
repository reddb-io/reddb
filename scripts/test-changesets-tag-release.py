#!/usr/bin/env python3
"""Exercise release recovery with isolated git/gh shims; no network or real tags."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("changesets-tag-release.sh")
SHIM = r'''#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
path=Path(os.environ['RELEASE_TEST_STATE'])
state=json.loads(path.read_text());args=sys.argv[1:];tool=Path(sys.argv[0]).name
state['calls'].append({'tool':tool,'args':args})
def finish(code=0,output=''):
 path.write_text(json.dumps(state))
 if output:print(output)
 raise SystemExit(code)
if tool=='git':
 if args[:2]==['ls-remote','--tags']:
  if state.get('fail_remote'):finish(128)
  if not state['remote']:finish()
  finish(output='c'*40+'\trefs/tags/v9.8.7\n'+state['remote_commit']+'\trefs/tags/v9.8.7^{}')
 if args[:3]==['rev-parse','-q','--verify']:
  finish(0 if state['local'] else 1, 'c'*40 if state['local'] else '')
 if args[0]=='rev-parse':
  finish(output=state['local_commit'] if 'v9.8.7' in args[-1] else 'b'*40)
 if args[:2]==['tag','-a']:
  state.update(local=True,local_commit='b'*40);finish()
 if args[0]=='push':
  if state.get('fail_push'):finish(1)
  state.update(remote=True,remote_commit=state['local_commit']);finish()
if tool=='gh':
 if args[:2]==['run','list']:
  if state.get('fail_list'):finish(1)
  assert state['remote_commit'] in args[args.index('--jq')+1], args
  finish(output='127' if state['publisher'] else '')
 if args[:3]==['workflow','run','release.yml']:
  assert args[args.index('--ref')+1]=='v9.8.7',args
  assert 'channel=stable' in args and 'version=9.8.7' in args and 'publish_cargo=true' in args,args
  state['publisher']=True;finish(output='https://github.com/owner/repo/actions/runs/127')
finish(2,'Unexpected mock command: '+repr([tool,*args]))
'''


class TagRelease(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="changesets-release-test-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "package.json").write_text('{"version":"9.8.7"}\n')
        bindir = self.root / "bin"
        bindir.mkdir()
        for name in ("git", "gh"):
            command = bindir / name
            command.write_text(SHIM)
            command.chmod(0o755)
        self.state_path = self.root / "state.json"
        self.state_path.write_text(json.dumps(dict(remote=False,local=False,publisher=False,
            remote_commit="a"*40,local_commit="b"*40,calls=[])))
        self.env = {**os.environ, "PATH": str(bindir) + os.pathsep + os.environ["PATH"],
                    "GITHUB_REPOSITORY": "owner/repo", "GITHUB_TOKEN": "synthetic-not-a-token",
                    "RELEASE_TEST_STATE": str(self.state_path)}

    def change(self, **values):
        state = self.state()
        state.update(values)
        self.state_path.write_text(json.dumps(state))

    def state(self):
        return json.loads(self.state_path.read_text())

    def run_script(self):
        return subprocess.run(["bash", str(SCRIPT)],cwd=self.root,env=self.env,
                              capture_output=True,text=True,timeout=10)

    def dispatches(self):
        return [c for c in self.state()["calls"] if c["tool"]=="gh"
                and c["args"][:3]==["workflow","run","release.yml"]]

    def test_new_tag_dispatches_once_and_retry_is_idempotent(self):
        first = self.run_script()
        self.assertEqual(first.returncode,0,first.stdout+first.stderr)
        self.assertEqual(len(self.dispatches()),1)
        second = self.run_script()
        self.assertEqual(second.returncode,0,second.stdout+second.stderr)
        self.assertEqual(len(self.dispatches()),1)
        calls = self.state()["calls"]
        self.assertEqual(sum(c["tool"]=="git" and c["args"][0]=="push" for c in calls),1)
        self.assertFalse(any("--force" in c["args"] or "-f" in c["args"] for c in calls if c["tool"]=="git"))

    def test_existing_tag_recovers_missing_publisher(self):
        self.change(remote=True)
        result = self.run_script()
        self.assertEqual(result.returncode,0,result.stdout+result.stderr)
        self.assertEqual(len(self.dispatches()),1)
        self.assertFalse(any(c["tool"]=="git" and c["args"][0] in ("tag","push") for c in self.state()["calls"]))

    def test_existing_publisher_is_never_submitted_again(self):
        self.change(remote=True,publisher=True)
        result = self.run_script()
        self.assertEqual(result.returncode,0,result.stdout+result.stderr)
        self.assertEqual(len(self.dispatches()),0)

    def test_failed_tag_push_cannot_dispatch(self):
        self.change(fail_push=True)
        result = self.run_script()
        self.assertNotEqual(result.returncode,0)
        self.assertEqual(len(self.dispatches()),0)

    def test_remote_read_failure_cannot_create_or_push_a_tag(self):
        self.change(fail_remote=True)
        result = self.run_script()
        self.assertNotEqual(result.returncode,0)
        self.assertFalse(any(c["tool"]=="git" and c["args"][0] in ("tag","push") for c in self.state()["calls"]))

    def test_unknown_publisher_state_does_not_dispatch_blindly(self):
        self.change(remote=True,fail_list=True)
        result = self.run_script()
        self.assertNotEqual(result.returncode,0)
        self.assertEqual(len(self.dispatches()),0)


if __name__ == "__main__":
    unittest.main()
