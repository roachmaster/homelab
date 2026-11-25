"""Service-scoped package restart policy; never call APT or restart a service."""
import contextlib
import hashlib
import io
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

from test_ci_supervision import SETUP


class CiRestartDeferralTest(unittest.TestCase):
    def test_perl_fragment_defers_only_exact_agent_and_preserves_other_selections(self):
        fragment=SETUP.render_agent_restart_deferral()
        program=r'''our %nrconf=(override_rc=>{qr(^fixture-other\.service$)=>1});
            eval do{local $/;<STDIN>};die $@ if $@;
            for my $name ('forge-jenkins-ci-agent.service','forge-jenkins-ci-agent.service-extra','jenkins.service','fixture-other.service') {
                my @keys=grep {$name =~ /$_/} keys %{$nrconf{override_rc}};
                print "$name=".(@keys ? $nrconf{override_rc}->{$keys[0]} : 'default')."\n";
            }'''
        result=subprocess.run(['/usr/bin/perl','-e',program],input=fragment,capture_output=True,timeout=5,check=True)
        self.assertEqual(result.stdout.decode().splitlines(),[
            'forge-jenkins-ci-agent.service=0','forge-jenkins-ci-agent.service-extra=default',
            'jenkins.service=default','fixture-other.service=1'])
        self.assertNotIn(b'blacklist_rc',fragment)
        self.assertNotIn(b'$nrconf{restart}',fragment)

    def test_root_only_install_has_one_fixed_target_and_no_service_or_controller_effect(self):
        with patch.object(SETUP.os,'geteuid',return_value=1000),patch.object(SETUP,'directory')as directory,patch.object(SETUP,'install')as install:
            with self.assertRaises(ValueError):SETUP.install_agent_restart_deferral()
            directory.assert_not_called();install.assert_not_called()
        with patch.object(SETUP.os,'geteuid',return_value=0),patch.object(SETUP,'directory')as directory,patch.object(SETUP,'install')as install,patch.object(SETUP.subprocess,'run')as run,patch.object(SETUP,'request')as request:
            result=SETUP.install_agent_restart_deferral()
        path=Path('/etc/needrestart/conf.d/99-forge-ci-agent.conf')
        directory.assert_called_once_with(path.parent)
        install.assert_called_once_with(path,SETUP.render_agent_restart_deferral(),owner=0,group=0,mode=0o644)
        run.assert_not_called();request.assert_not_called()
        self.assertFalse(result['service_restart_performed'])
        self.assertFalse(result['package_update_policy_changed'])
        self.assertEqual(result['sha256'],hashlib.sha256(SETUP.render_agent_restart_deferral()).hexdigest())

    def test_dedicated_cli_mode_requires_execute_but_no_controller_credentials(self):
        with patch.object(SETUP,'install_agent_restart_deferral')as install,contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit)as error:SETUP.main(['--agent-restart-deferral-only'])
            self.assertEqual(error.exception.code,2);install.assert_not_called()
        with patch.object(SETUP,'install_agent_restart_deferral',return_value={'service_restart_performed':False})as install,patch.object(SETUP,'execute')as full,patch.object(SETUP,'execute_native')as native,contextlib.redirect_stdout(io.StringIO()):
            SETUP.main(['--agent-restart-deferral-only','--execute'])
        install.assert_called_once_with();full.assert_not_called();native.assert_not_called()


if __name__=='__main__':unittest.main()
