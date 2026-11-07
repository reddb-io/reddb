"""Exercise the workflow cost boundary and the real release plan/conditions."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]
RELEASE = yaml.safe_load((ROOT / '.github/workflows/release.yml').read_text())


class RunnerPolicy(unittest.TestCase):
    def check_workflow(self, workflow, name='fixture.yml'):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            workflows = root / '.github/workflows'
            workflows.mkdir(parents=True)
            (workflows / name).write_text(yaml.safe_dump(workflow))
            return subprocess.run([sys.executable, str(ROOT / '.github/scripts/quick-check.py')],
                                  cwd=root, text=True, capture_output=True)

    def test_standard_native_arm_runner_is_accepted(self):
        workflow = {'on': {'workflow_dispatch': {}}, 'jobs': {'build': {'runs-on': 'ubuntu-24.04-arm'}}}
        self.assertEqual(self.check_workflow(workflow).returncode, 0)

    def test_paid_labels_are_rejected_in_jobs_and_embedded_platform_matrices(self):
        for job in [{'runs-on': 'blacksmith-8vcpu-ubuntu-2404'},
                    {'runs-on': '${{ matrix.os }}', 'env': {'PLATFORM_MATRICES': json.dumps({
                        'build': [{'row': {'os': 'blacksmith-8vcpu-ubuntu-2404-arm'}}]})}}]:
            with self.subTest(job=job):
                result = self.check_workflow({'on': {'workflow_dispatch': {}}, 'jobs': {'build': job}})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('standard GitHub-hosted runners', result.stderr)

    def test_testbox_exception_requires_a_manual_session(self):
        workflow = yaml.safe_load((ROOT / '.github/workflows/blacksmith-testbox.yml').read_text())
        self.assertEqual(self.check_workflow(workflow, 'blacksmith-testbox.yml').returncode, 0)
        events = workflow.get('on', workflow.get(True))
        events['workflow_dispatch']['inputs']['testbox_id']['required'] = False
        result = self.check_workflow(workflow, 'blacksmith-testbox.yml')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('explicit manual session', result.stderr)

    def plan(self, build_only, channel='stable', version=''):
        step = next(s for s in RELEASE['jobs']['plan']['steps'] if s.get('id') == 'plan')
        script = step['run']
        for expression, value in {
            "${{ github.event_name }}": 'workflow_dispatch',
            "${{ github.ref }}": 'refs/heads/qualification',
            "${{ github.event.inputs.channel || 'stable' }}": channel,
            "${{ github.event.inputs.version || '' }}": version,
            "${{ github.event.inputs.publish_cargo || 'false' }}": 'true',
        }.items():
            script = script.replace(expression, value)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'package.json').write_text('{"version":"1.2.3"}')
            output = root / 'output'
            result = subprocess.run(['bash', '-c', script], cwd=root, capture_output=True, text=True,
                                    env={**os.environ, 'BUILD_ONLY': str(build_only).lower(),
                                         'GITHUB_OUTPUT': str(output), 'GITHUB_RUN_NUMBER': '1',
                                         'HEAD_COMMIT_MESSAGE': ''})
            values = dict(line.split('=', 1) for line in output.read_text().splitlines()) if output.exists() else {}
            return result, values

    def test_build_only_plan_uses_checked_out_version_and_disables_cargo_publication(self):
        result, output = self.plan(True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output['package_version'], '1.2.3')
        self.assertEqual(output['release_channel'], 'stable')
        self.assertEqual(output['should_skip'], 'false')
        self.assertEqual(output['publish_cargo'], 'false')

    def test_normal_manual_release_still_requires_a_version(self):
        self.assertNotEqual(self.plan(False)[0].returncode, 0)
        result, output = self.plan(False, version='1.2.4')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output['package_version'], '1.2.4')
        self.assertEqual(output['publish_cargo'], 'true')
        self.assertNotEqual(self.plan(True, channel='next')[0].returncode, 0)

    def test_build_only_blocks_every_publisher_even_when_all_dependencies_succeed(self):
        script = r"""
const fs = require('node:fs');
const vm = require('node:vm');
const jobs = JSON.parse(fs.readFileSync(0, 'utf8'));
const outputs = {should_skip:'false', release_channel:'stable', publish_cargo:'true', linux_x64:'true', docker_platforms:'linux/amd64,linux/arm64'};
const needs = Object.fromEntries(Object.keys(jobs).map(name => [name, {result:'success', outputs}]));
const results = {};
for (const build_only of [false, true]) {
  results[build_only] = {};
  for (const [name, job] of Object.entries(jobs)) {
    if (!name.startsWith('publish-') && name !== 'verify-release-assets') continue;
    const condition = job.if ? job.if.replace(/^\s*\$\{\{/, '').replace(/\}\}\s*$/, '') : 'true';
    results[build_only][name] = vm.runInNewContext(condition, {inputs:{build_only, preview_platforms:false}, needs, always:()=>true, cancelled:()=>false});
  }
}
process.stdout.write(JSON.stringify(results));
"""
        result = subprocess.run(['node', '-e', script], input=json.dumps(RELEASE['jobs']),
                                text=True, capture_output=True, check=True)
        modes = json.loads(result.stdout)
        self.assertGreater(len(modes['true']), 10)
        self.assertFalse(any(modes['true'].values()), modes['true'])
        self.assertTrue(modes['false']['publish-github'])
        self.assertTrue(modes['false']['publish-docker'])
        self.assertTrue(modes['false']['publish-cargo'])
        self.assertFalse(modes['false']['publish-python-wheels'])


if __name__ == '__main__':
    unittest.main()
