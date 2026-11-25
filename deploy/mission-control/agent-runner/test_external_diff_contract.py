"""External source review hashes are independent of clone object population."""
import hashlib
from pathlib import Path
import re
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from uuid import uuid4

import test_work_external_delivery as fixtures
from work_external_review_context import ExternalReviewContextProvider,canonical


class ExternalDiffContractTest(unittest.TestCase):
    def test_prepared_diff_is_verified_in_a_larger_independent_object_database(self):
        helper=fixtures.LocalGitSanitizerTest()
        with TemporaryDirectory(prefix='forge-independent-diff-') as temp:
            config,author,remote,canonical_repo,base,_tree,port=helper._setup_repo_fixture(temp)
            (author/'app.py').write_text('candidate\n')
            helper.git(author,'add','app.py');helper.git(author,'commit','--quiet','-m','Candidate')
            raw=helper.git(author,'rev-parse','HEAD').decode().strip()
            tree=helper.git(author,'rev-parse','HEAD^{tree}').decode().strip()
            helper.git(author,'push','--quiet',str(remote),'HEAD:refs/heads/feature')
            candidate=helper._prepare(port,base,raw,tree,source_branch='feature')
            reviewer=Path(temp)/'reviewer'
            helper.git(author,'clone','--quiet','--no-hardlinks',str(canonical_repo),str(reviewer))
            helper.git(reviewer,'fetch','--quiet',str(canonical_repo),candidate['candidate_commit'])
            helper.git(reviewer,'checkout','--quiet','--detach',candidate['candidate_commit'])
            # Independent worker caches contain objects unrelated to this task.
            # Git's automatic blob abbreviation grows with object population.
            stream=bytearray()
            for i in range(70000):
                blob=('unrelated-review-cache-object-'+str(i)+'\n').encode()
                stream.extend(b'blob\ndata '+str(len(blob)).encode()+b'\n'+blob+b'\n')
            helper.git(reviewer,'fast-import','--quiet',data=stream)
            legacy=('--binary','--no-ext-diff','--no-textconv','--no-color','--diff-algorithm=myers','--no-renames')
            producer=next((config.state_root/'actions').iterdir())/'quarantine'
            old_source=helper.git(producer,'diff',*legacy,base,candidate['candidate_commit'],'--')
            old_review=helper.git(reviewer,'diff',*legacy,base,candidate['candidate_commit'],'--')
            self.assertNotEqual(old_source,old_review)
            self.assertEqual(re.sub(rb'(?m)^index .+$',b'index IDENTITIES',old_source),
                             re.sub(rb'(?m)^index .+$',b'index IDENTITIES',old_review))
            content=canonical({'version':1,'candidate_commit':raw,'candidate_tree':tree,'base_revision':base,'summary':'Source artifact'}).decode()
            plan='# Approved source-bound plan'
            reviewer_id=str(uuid4())
            target={'version':1,'kind':'external_database_review','workflow_id':str(uuid4()),
                'review_id':str(uuid4()),'reviewer_task_id':reviewer_id,'reviewer_profile':'codex-v2',
                'target_kind':'code','source_task_id':str(uuid4()),'source_assignment_id':str(uuid4()),
                'source_sha256':hashlib.sha256(content.encode()).hexdigest(),'source_revision':3,
                'source_base_revision':base,'review_base_revision':candidate['candidate_commit'],
                'request_prompt':'Review candidate','acceptance_criteria':'Verify exact source',
                'source_content':content,'approved_plan':{'assignment_id':str(uuid4()),
                    'sha256':hashlib.sha256(plan.encode()).hexdigest(),'content':plan},
                'candidate':candidate,'required_attestations':[]}
            def envelope():
                raw_target=canonical(target).decode()
                return {'target_json':raw_target,'target_sha256':hashlib.sha256(raw_target.encode()).hexdigest()}
            store=SimpleNamespace(read_external_review_context=lambda lease:envelope())
            provider=ExternalReviewContextProvider(store,helper.git)
            lease={'mode':'review','task_id':reviewer_id,'agent_profile':'codex-v2'}
            workspace=SimpleNamespace(path=reviewer,base_revision=candidate['candidate_commit'])
            self.assertEqual(provider.acquire(lease,workspace).reviewer_task_id,reviewer_id)
            target['candidate']={**candidate,'diff_sha256':'0'*64}
            with self.assertRaises(ValueError):provider.acquire(lease,workspace)


if __name__=='__main__':unittest.main()
