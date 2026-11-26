"""Actual M31 history survives M32 without rewriting retry or execution evidence."""
from contextlib import closing
import hashlib
from pathlib import Path
import tempfile
import unittest
import psycopg2

from work_pg_fixture import DisposablePostgresCluster,PG16_AVAILABLE,SQL_DIR
import test_delivery_retry_events as delivery_fixtures
from test_work_policy_upgrade import historical_registry,PreUpgradeCluster


# Migration 0051 (pg_proxy_cutover) revokes direct table writes
# (INSERT/UPDATE/DELETE/TRUNCATE, ACL letters a/w/d/D) for the four
# write-capable roles. Apply that revocation to a pg_class relacl value
# so an upgrade test can pin exactly which privileges may disappear
# and nothing else. psycopg2 returns aclitem[] as the array-literal
# string '{role=privs/grantor,...}' (not a list); the value is returned
# in the same shape it arrived in (a list stays a list). A privilege
# letter's '*' (WITH GRANT OPTION) goes with the letter; an entry left
# with no privileges is dropped, as REVOKE drops it.
_CUTOVER_ROLES={'forge_agent','forge_coding_worker','forge_coding_reconciler','muse_events'}
_CUTOVER_REVOKED=set('awdD')


def _split_acl_array(relacl):
    text=relacl.strip()
    if text.startswith('{') and text.endswith('}'):
        text=text[1:-1]
    parts,current,quoted=[],[],False
    for char in text:
        if char=='"':
            quoted=not quoted
            current.append(char)
        elif char==',' and not quoted:
            parts.append(''.join(current))
            current=[]
        else:
            current.append(char)
    if current:
        parts.append(''.join(current))
    return parts


def _normalize_null_acl(relacl, owner):
    # A NULL relacl means "the owner holds every table privilege".
    # REVOKE (as the M51 cutover does on all tables in the schemas)
    # materializes that as an explicit owner entry,
    # '{owner=arwdDxt/owner}', without changing effective privileges.
    # Normalize the two spellings so the upgrade comparison is not
    # tripped by the mechanical materialization.
    if relacl is not None:
        return relacl
    return '{%s=arwdDxt/%s}' % (owner, owner)


# Migration 0050 (pg_proxy_preparation) grants each caller role SELECT
# on its allowlisted read tables, derived from the proxy registry (the
# same source M50 was generated from, so the expected post-upgrade ACL
# is exact). Coordinator/operator share forge_agent; a "*" read set
# means every READ_TABLES entry. The grants run as forge_agent_owner.
from forge_pg_proxy import CALLER_ALLOWLISTS as _PROXY_ALLOWLISTS, CALLER_ROLES as _PROXY_ROLES, READ_TABLES as _PROXY_READ_TABLES
_PROXY_READERS={}
for _caller,_allow in _PROXY_ALLOWLISTS.items():
    _read=_allow['read']
    _tables=sorted(_PROXY_READ_TABLES) if _read=='*' else sorted(_read)
    for _table in _tables:
        _PROXY_READERS.setdefault(_table,set()).add(_PROXY_ROLES[_caller])


def _with_proxy_reads(relacl,readers):
    # Append a `role=r/forge_agent_owner` entry for every proxy reader
    # role that had no entry before, in the generator's grant order
    # (roles sorted). A role that already held an entry is untouched:
    # GRANT SELECT on an existing entry is a no-op, and the M51
    # revocation below only rewrites forge_agent's entry in place.
    if not readers:
        return relacl
    entries=_split_acl_array(relacl)
    have={entry.split('=',1)[0] for entry in entries}
    for role in sorted(readers):
        if role not in have:
            entries.append(role+'=r/forge_agent_owner')
    return '{'+','.join(entries)+'}'


def _cutover_acl(relacl):
    if relacl is None:
        return None
    as_list=isinstance(relacl,list)
    entries=list(relacl) if as_list else _split_acl_array(relacl)
    kept=[]
    for entry in entries:
        role,rest=entry.split('=',1)
        privs,_,grantor=rest.partition('/')
        out=[]
        index=0
        while index<len(privs):
            letter=privs[index]
            grant_option=index+1<len(privs) and privs[index+1]=='*'
            if role in _CUTOVER_ROLES and letter in _CUTOVER_REVOKED:
                index+=2 if grant_option else 1
                continue
            out.append(letter)
            if grant_option:
                out.append('*')
            index+=2 if grant_option else 1
        if out:
            kept.append(role+'='+''.join(out)+'/'+grantor)
    return kept if as_list else '{'+','.join(kept)+'}'


@unittest.skipUnless(PG16_AVAILABLE,'Requires owned PostgreSQL16')
class DeliveryLineageSchemaTest(unittest.TestCase):
    @staticmethod
    def rows(cluster,sql,args=()):
        with closing(cluster.connect('postgres')) as c,c.cursor() as q:
            q.execute(sql,args)
            return q.fetchall()

    def test_schema31_history_rows_receipts_counters_roles_defaults_and_hba_survive_upgrade(self):
        with tempfile.TemporaryDirectory(prefix='forge-lineage32-upgrade-') as scratch:
            registry=Path(scratch)/'sql';historical_registry(registry,31)
            cluster=PreUpgradeCluster(registry).start()
            try:
                # Restore the real current runtime grants; the historical helper
                # intentionally trims unrelated grants for its older fixtures.
                cluster._run('psql','-h',cluster.root,'-U','postgres','-X','-v','ON_ERROR_STOP=1',
                    '-d','forge_agent','-c','SET ROLE forge_agent_owner','-f',SQL_DIR/'management/grant-runner.sql')
                cluster.admin_exec('CREATE ROLE muse_events LOGIN NOINHERIT',owner=False)
                cluster._run('psql','-h',cluster.root,'-U','postgres','-X','-v','ON_ERROR_STOP=1',
                    '-d','forge_agent','-c','SET ROLE forge_agent_owner','-f',SQL_DIR/'management/grant-muse-read-access.sql')
                fixture=delivery_fixtures.DeliveryRetryEventTest();fixture.pg=cluster;fixture.setUp()
                fixture.initial_failure('all-features-build')
                self.assertTrue(fixture.service().workflow_worker._delivery_repair.run_once())
                names=self.rows(cluster,"SELECT n.nspname,c.relname FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN('forge_agent','forge_external_workflow','forge_external_agent') AND c.relkind='r' AND c.relname<>'schema_migrations' ORDER BY 1,2")
                owners=dict(self.rows(cluster,'SELECT oid, rolname FROM pg_authid'))
                def snapshot():
                    return {
                        'rows':{schema+'.'+name:self.rows(cluster,"SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) FROM "+schema+'.'+name+' t') for schema,name in names},
                        'migration_history':self.rows(cluster,'SELECT version,name,applied_at FROM forge_agent.schema_migrations WHERE version<=31 ORDER BY version'),
                        'credentials':self.rows(cluster,'SELECT md5(jsonb_agg(to_jsonb(r) ORDER BY rolname)::text) FROM pg_authid r'),
                        'defaults':self.rows(cluster,'SELECT to_jsonb(d) FROM pg_default_acl d ORDER BY oid'),
                        'old_relation_acl':[(schema,name,owner,_normalize_null_acl(relacl,owners.get(owner))) for schema,name,owner,relacl in self.rows(cluster,"SELECT n.nspname,c.relname,c.relowner,c.relacl FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname IN('forge_agent','forge_external_workflow','forge_external_agent') AND c.relkind='r' AND c.relname NOT IN('delivery_lineage_handoffs','agent_branch_state','jenkins_stage_results','jenkins_stage_log_chunks','coding_identity_catalog','coding_task_identities','coding_attempt_deadline_extensions') AND NOT(n.nspname='forge_external_workflow' AND c.relname='plan_approve_overrides') ORDER BY 1,2")],
                        'hba':hashlib.sha256((cluster.data/'pg_hba.conf').read_bytes()).hexdigest(),
                    }
                before=snapshot()
                DisposablePostgresCluster.apply_and_verify(cluster)
                DisposablePostgresCluster.apply_and_verify(cluster)
                # Preparation stage: apply.sql stops at migration
                # 0050 (pg_proxy_preparation), which grants each
                # caller role SELECT on its allowlisted read tables;
                # the expected ACL adds exactly those entries (in
                # grant order) to the M31 ACL, and nothing else about
                # the M31 history changes. (Round-6 finding 7: this
                # test previously asserted the CUTOVER state here
                # although the fixture only applies preparation.)
                preparation=dict(before)
                preparation['old_relation_acl']=[
                    (schema,name,owner,_with_proxy_reads(relacl,_PROXY_READERS.get(schema+'.'+name,set())))
                    for schema,name,owner,relacl in before['old_relation_acl']
                ]
                self.assertEqual(snapshot(),preparation)
                self.assertEqual(self.rows(cluster,'SELECT max(version) FROM forge_agent.schema_migrations'),[(50,)])
                # Cutover stage: migration 0051 (pg_proxy_cutover),
                # applied only via apply-cutover.sql, intentionally
                # revokes direct table writes
                # (INSERT/UPDATE/DELETE/TRUNCATE) for forge_agent,
                # forge_coding_worker, forge_coding_reconciler and
                # muse_events on top of the preparation ACL.
                DisposablePostgresCluster.apply_cutover(cluster)
                expected=dict(before)
                expected['old_relation_acl']=[
                    (schema,name,owner,_cutover_acl(_with_proxy_reads(relacl,_PROXY_READERS.get(schema+'.'+name,set()))))
                    for schema,name,owner,relacl in before['old_relation_acl']
                ]
                self.assertEqual(snapshot(),expected)
                self.assertEqual(self.rows(cluster,'SELECT max(version) FROM forge_agent.schema_migrations'),[(51,)])
                self.assertEqual(self.rows(cluster,'SELECT count(*) FROM forge_agent.delivery_lineage_handoffs'),[(0,)])
                self.assertEqual(self.rows(cluster,"SELECT has_table_privilege('muse_events','forge_agent.delivery_lineage_handoffs','SELECT')"),[(False,)])
                # verify.sql itself starts BEGIN READ ONLY and pins the exact
                # M32 definitions before any predecessor source normalization.
                cluster._run('psql','-h',cluster.root,'-U','postgres','-X','-v','ON_ERROR_STOP=1',
                    '-d','forge_agent','-c','SET ROLE forge_agent_owner','-f',SQL_DIR/'management/verify.sql')
            finally:cluster.stop()

    def test_new_authority_table_trigger_acl_and_function_drift_fail_read_only_verification(self):
        cluster=DisposablePostgresCluster().start()
        try:
            changes=(
                'ALTER TABLE forge_agent.delivery_lineage_handoffs ADD COLUMN unauthorized text',
                'ALTER TABLE forge_agent.delivery_lineage_handoffs DISABLE TRIGGER delivery_lineage_handoff_immutable',
                'GRANT SELECT ON forge_agent.delivery_lineage_handoffs TO forge_agent',
                "CREATE OR REPLACE FUNCTION forge_agent._delivery_lineage_blocked(p_owner text,p_project text,p_lineage uuid) RETURNS boolean LANGUAGE sql STABLE AS 'SELECT false'",
                'GRANT EXECUTE ON FUNCTION forge_agent._delivery_adopt_repair_lineage(uuid,uuid) TO forge_coding_worker',
            )
            for statement in changes:
                with self.subTest(statement=statement),closing(cluster.connect('postgres')) as c:
                    with c.cursor() as q:
                        q.execute(statement)
                        with self.assertRaises(psycopg2.Error):q.execute((SQL_DIR/'management/verify-delivery-lineage.sql').read_text())
                    c.rollback()
            cluster.apply_and_verify()
        finally:cluster.stop()
