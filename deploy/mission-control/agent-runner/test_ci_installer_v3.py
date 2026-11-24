"""Explicit v3 provisioning preserves the live isolated executor boundary."""
from copy import deepcopy
import hashlib
import json
from pathlib import Path
import subprocess
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import test_ci_supervision as fixtures

SETUP = fixtures.SETUP
CI = fixtures.CI


class V3InstallerTest(unittest.TestCase):
    def pipeline(self, profile=CI.SCOPED_PROFILE):
        return {'version':1,'build_user':'forge_ci','projects':['homelab'],
            'verification_profile':profile,'verifier_sha256':'a'*64,
            'results_root':'/legacy/results','cargo_cache':{'enabled':True,
                'root':'/mnt/data/forge-homelab/ci-cache','max_total_bytes':64*1024**3,
                'seed_from_main':True,'seed_from_siblings':False}}

    def test_explicit_v3_and_inherited_v3_keep_root_results_export_and_cache(self):
        source=Path(CI.__file__).read_bytes()
        for prior,selection in ((CI.SCOPED_PROFILE,CI.EVENT_PROFILE),(CI.EVENT_PROFILE,None)):
            with self.subTest(prior=prior,selection=selection):
                original=self.pipeline(prior);before=deepcopy(original)
                selected=SETUP.native_installation_plan(original,source,verification_profile=selection)
                self.assertEqual(original,before)
                config=selected['pipeline']
                self.assertEqual(config['verification_profile'],CI.EVENT_PROFILE)
                self.assertIs(config['portal_artifact_export'],True)
                self.assertIs(config['require_main_ancestor'],True)
                self.assertEqual(config['results_root'],str(SETUP.NATIVE_RESULTS))
                self.assertEqual(config['cargo_cache'],before['cargo_cache'])
                self.assertEqual(config['verifier_sha256'],hashlib.sha256(selected['helper']).hexdigest())
                self.assertIn(b"CONFIG = Path('/etc/forge-ci/native-pipeline.json')",selected['helper'])

    def test_rendered_config_explicitly_enables_v3_without_changing_legacy_defaults(self):
        args=dict(operator='fixture',spool=Path('/tmp/spool'),state_root=Path('/tmp/state'),
            toolchain=Path('/opt/forge-ci/toolchain'),registry=Path('/opt/forge-ci/registry'),
            modules=Path('/opt/forge-ci/node_modules'),verifier_sha256='a'*64)
        selected=SETUP.render_pipeline_config(**args,verification_profile=CI.EVENT_PROFILE)
        self.assertEqual(selected['verification_profile'],CI.EVENT_PROFILE)
        self.assertIs(selected['portal_artifact_export'],True)
        self.assertEqual(SETUP.render_pipeline_config(**args)['verification_profile'],CI.SCOPED_PROFILE)
        self.assertIs(SETUP.render_pipeline_config(**args,verification_profile=CI.LEGACY_PROFILE)['portal_artifact_export'],False)

    def test_actual_native_installation_routes_v3_to_the_separate_job_only(self):
        user=SimpleNamespace(pw_uid=43210,pw_gid=43211)
        original=self.pipeline();installed={}
        info={'job':'forge-native-candidate','node':'forge-mission-control-ci',
            'controller_executors':0,'no_job_submitted':True}
        with patch.object(SETUP.os,'geteuid',return_value=0), \
             patch.object(SETUP.pwd,'getpwnam',return_value=user), \
             patch.object(SETUP,'read_owned',return_value=b'{}'), \
             patch.object(SETUP,'qualify_stage_token',return_value='systemd-credential'), \
             patch.object(SETUP.Path,'exists',return_value=True), \
             patch.object(SETUP,'read_pipeline',return_value=original), \
             patch.object(SETUP,'directory') as directory, \
             patch.object(SETUP,'install',side_effect=lambda path,raw,**kw:installed.update({path:raw})), \
             patch.object(SETUP,'request',return_value=json.dumps(info).encode()) as request, \
             patch.object(SETUP.subprocess,'run') as run,patch('builtins.print'):
            SETUP.execute_native(Path('/private/client.json'),'fixture',verification_profile=CI.EVENT_PROFILE)
        config=json.loads(installed[SETUP.CONFIG/'native-pipeline.json'])
        self.assertEqual(config['verification_profile'],CI.EVENT_PROFILE)
        self.assertTrue(config['portal_artifact_export'])
        directory.assert_any_call(SETUP.NATIVE_RESULTS,group=user.pw_gid,mode=0o750)
        self.assertEqual(config['verifier_sha256'],hashlib.sha256(installed[Path('/usr/local/libexec/forge-native-ci-check')]).hexdigest())
        self.assertEqual(run.call_count,1)
        self.assertEqual(run.call_args.args[0][0],'/usr/sbin/visudo')
        self.assertEqual(request.call_count,1)
        for forbidden in (SETUP.CONFIG/'pipeline.json',Path('/usr/local/libexec/forge-ci-check'),
                Path('/etc/systemd/system/forge-jenkins-ci-agent.service')):
            self.assertNotIn(forbidden,installed)

    def test_cli_accepts_v3_but_still_requires_explicit_execution(self):
        result=subprocess.run([sys.executable,'-B',str(fixtures.HERE.parent/'jenkins/setup-ci.py'),
            '--native-only','--verification-profile',CI.EVENT_PROFILE,
            '--client-config','/nonexistent/never-read'],capture_output=True,text=True,timeout=10)
        self.assertEqual(result.returncode,2)
        self.assertIn('Explicit --execute',result.stderr)
        self.assertNotIn('invalid choice',result.stderr)


if __name__=='__main__':unittest.main()
