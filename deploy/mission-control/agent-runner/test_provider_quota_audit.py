"""Historical recovery accepts exact private, timestamped CLI error evidence."""
from datetime import UTC,datetime
import hashlib
import json
import os
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch
from uuid import uuid4

from queue_store import Rejected
from work_external_delivery import read_private
from work_provider_model_route import ModelRoute,QuotaAuditProofFactory


class QuotaAuditTest(unittest.TestCase):
    def fixture(self,root):
        root=Path(root);config=root/'config.json';config.write_bytes(b'{"selected":"fixture"}');config.chmod(0o600)
        command=ModelRoute('homelab',uuid4(),uuid4(),3,uuid4(),'claude-opus-5-5-high','source-quota','Explicit audited quota source.')
        task={'owner_id':'mission-control'};workflow={'workflow_id':uuid4()}
        attempt=dict(recipe_digest='a'*64,started_at=datetime(2026,11,25,20,33,50,tzinfo=UTC),
                     finished_at=datetime(2026,11,25,20,34,15,tzinfo=UTC))
        log=root/'cli-20261125_203353.log'
        log.write_bytes(b'E1125 20:34:14.725287     205 errorreport.go:285] RESOURCE_EXHAUSTED (code 429): Individual quota reached. Resets in 20m56s.\n')
        log.chmod(0o600)
        manifest=dict(version=1,owner='mission-control',model=command.model,source_config_sha256=hashlib.sha256(config.read_bytes()).hexdigest(),
            observations=[dict(workflow_id=str(workflow['workflow_id']),task_id=str(command.task_id),attempt_id=str(command.attempt_id),
                source_revision=3,recipe_digest='a'*64,log_file=log.name,log_sha256=hashlib.sha256(log.read_bytes()).hexdigest())])
        audit=root/'audit.json'
        def save():audit.write_text(json.dumps(manifest));audit.chmod(0o600)
        save()
        port=QuotaAuditProofFactory(audit,config,root,worker_uid=os.geteuid()).create()
        return port,command,task,attempt,workflow,manifest,save,log
    def read_fixture(self,path,*,owner=None,maximum=65536):
        # Fixture runner cannot chown files to root. Production still enforces
        # owner=0 for audit/config and exact selected worker UID for raw logs.
        return read_private(path,owner=os.geteuid(),maximum=maximum)
    def test_exact_historical_source_error_is_accepted_without_invoking_agent(self):
        with TemporaryDirectory()as root:
            port,command,task,attempt,workflow,*_=self.fixture(root)
            with patch('work_external_delivery.read_private',self.read_fixture):
                proof=port.acquire(command,task,attempt,workflow)
            self.assertEqual(proof.attempt_id,command.attempt_id)
            self.assertEqual(proof.model,'claude-opus-5-5-high')
            self.assertEqual(proof.recipe_digest,attempt['recipe_digest'])
    def test_scope_hash_timestamp_errorchannel_and_strict_schema_reject(self):
        for case in('revision','version_bool','config_sha','log_sha','workflow','filename','normal_prose','outside_interval'):
            with self.subTest(case=case),TemporaryDirectory()as root:
                port,command,task,attempt,workflow,manifest,save,log=self.fixture(root)
                item=manifest['observations'][0]
                if case=='revision':item['source_revision']=4
                elif case=='version_bool':manifest['version']=True
                elif case=='config_sha':manifest['source_config_sha256']='b'*64
                elif case=='log_sha':item['log_sha256']='b'*64
                elif case=='workflow':item['workflow_id']=str(uuid4())
                elif case=='filename':item['log_file']='../'+log.name
                elif case=='normal_prose':
                    log.write_bytes(b'I1125 20:34:14.725287 205 model.go:285] Individual quota reached. Resets in 20m56s.\n')
                    item['log_sha256']=hashlib.sha256(log.read_bytes()).hexdigest()
                else:attempt['finished_at']=datetime(2026,11,25,20,34,13,tzinfo=UTC)
                save()
                with patch('work_external_delivery.read_private',self.read_fixture),self.assertRaises(Rejected):
                    port.acquire(command,task,attempt,workflow)


if __name__=='__main__':unittest.main()
