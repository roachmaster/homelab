//! NativePostfixLedger (work_native_ci_postfix_proof.py):
//! the root-selected private evidence lock for the M38
//! post-fix exception. Binds all twenty admissions to one
//! exact workflow, latest source review, failure digest
//! and observation, held FOR SHARE on its own connection
//! for the whole recovery transaction.

use forge_work::native_ci_postfix::{PostfixEventLock, PostfixLedgerPort};
use forge_work::WorkError;
use uuid::Uuid;

use crate::client::PgSession;
use crate::config::PgConfig;

const LEDGER_SQL: &str = "SELECT e.source_digest FROM forge_agent.coding_retry_events e \
    JOIN forge_agent.coding_retry_review_events l ON l.event_id=e.event_id \
    JOIN forge_agent.coding_retry_event_observations o ON o.event_id=e.event_id \
    WHERE(e.owner_id,e.project_alias,e.scope_kind,e.scope_id,e.kind,e.phase_id,e.event_id)=($1,$2,'native_workflow',$3,'infrastructure','infrastructure',$4) \
    AND(e.used,e.attempt_limit,e.status)=(20,20,'exhausted') AND e.source_id=$5 \
    AND(l.review_id,l.reservation_key)=($6,$7) \
    AND(o.owner_id,o.project_alias,o.scope_kind,o.scope_id,o.kind,o.phase_id,o.source_id,o.source_digest,o.observation_kind)=(e.owner_id,e.project_alias,e.scope_kind,e.scope_id,e.kind,e.phase_id,e.source_id,e.source_digest,'failure') \
    AND(SELECT count(*) FROM forge_agent.coding_retry_event_admissions a WHERE a.event_id=e.event_id)=20 \
    FOR SHARE OF e,l,o";

/// The ledger port over a dedicated connection per acquire
/// (Python's injected connect factory).
pub struct PgPostfixLedger {
    config: PgConfig,
}

impl PgPostfixLedger {
    pub fn new(config: PgConfig) -> Self {
        Self { config }
    }
}

/// The held lock: the session's explicit transaction stays
/// open (with its FOR SHARE rows) until the guard drops,
/// which rolls it back.
pub struct PgPostfixEventLock {
    session: Option<PgSession>,
    event_id: Uuid,
    source_digest: String,
}

impl PostfixEventLock for PgPostfixEventLock {
    fn event_id(&self) -> Uuid {
        self.event_id
    }

    fn source_digest(&self) -> &str {
        &self.source_digest
    }
}

impl Drop for PgPostfixEventLock {
    fn drop(&mut self) {
        if let Some(mut session) = self.session.take() {
            let (client, rt) = session.raw();
            let _ = rt.block_on(async {
                client.batch_execute("ROLLBACK").await
            });
        }
    }
}

impl PostfixLedgerPort for PgPostfixLedger {
    fn acquire(
        &self,
        owner: &str,
        project: &str,
        workflow_id: Uuid,
        review_id: Uuid,
        event_id: Uuid,
    ) -> Result<Box<dyn PostfixEventLock>, WorkError> {
        let mut session = PgSession::connect(self.config.clone())
            .map_err(|error| WorkError::store(error.to_string()))?;
        let review_text = review_id.to_string();
        let reservation_key = format!("review-fix.{review_id}");
        let (client, rt) = session.raw();
        let digest = rt
            .block_on(async {
                client.batch_execute("BEGIN").await?;
                client
                    .batch_execute("SET LOCAL lock_timeout='4s'")
                    .await?;
                client
                    .batch_execute("SET LOCAL statement_timeout='10s'")
                    .await?;
                let params: &[&(dyn tokio_postgres::types::ToSql + Sync)] = &[
                    &owner,
                    &project,
                    &workflow_id,
                    &event_id,
                    &review_text,
                    &review_id,
                    &reservation_key,
                ];
                client.query_opt(LEDGER_SQL, params).await
            })
            .map_err(|error: tokio_postgres::Error| WorkError::store(error.to_string()))?;
        let Some(row) = digest else {
            let (client, rt) = session.raw();
            let _ = rt.block_on(async { client.batch_execute("ROLLBACK").await });
            return Err(WorkError::rejected(409, "retry_exhausted"));
        };
        let source_digest: String = row.get(0);
        Ok(Box::new(PgPostfixEventLock {
            session: Some(session),
            event_id,
            source_digest,
        }))
    }
}
