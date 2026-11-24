"""PostgreSQL DAO for durable Jenkins events, leased deliveries, and monotonic observations."""

from contextlib import contextmanager
from datetime import datetime, timezone
import select
import time
from uuid import UUID, uuid4
from urllib.parse import urlsplit

import psycopg2
from psycopg2.extras import RealDictCursor, register_uuid

from jenkins_config import JenkinsConfig
from jenkins_events import (
    JenkinsBuildResult,
    JenkinsDeliveryRecord,
    JenkinsEventEmit,
    JenkinsEventKind,
    JenkinsEventRecord,
    JenkinsStageStatus,
    MAX_SEQUENCE,
    VALID_DELIVERY_STATUSES,
    sanitize_error_message,
    validate_build_url,
    validate_commit_sha,
    validate_identifier,
    validate_int_range,
    validate_job_name,
    validate_sha256,
)
from queue_store import Rejected

register_uuid()

ADVISORY_LOCK_SALT = 2026112101
NOTIFY_CHANNEL = "forge_jenkins_events"
EVENTS_DEFAULT_LIMIT = 50
EVENTS_MAX_LIMIT = 100
DELIVERIES_DEFAULT_LIMIT = 50
DELIVERIES_MAX_LIMIT = 100


def _iso_utc(value) -> str | None:
    if value is None:
        return None
    if isinstance(value, datetime):
        utc_dt = value.astimezone(timezone.utc)
        if utc_dt.microsecond:
            return utc_dt.strftime("%Y-%m-%dT%H:%M:%S.%fZ")
        return utc_dt.strftime("%Y-%m-%dT%H:%M:%SZ")
    return str(value)


def _row_to_event(row) -> JenkinsEventRecord:
    return JenkinsEventRecord(
        event_id=str(row["event_id"]),
        owner_id=row["owner_id"],
        project_alias=row["project_alias"],
        sequence=int(row["sequence"]),
        jenkins_instance=row["jenkins_instance"],
        job_name=row["job_name"],
        delivery_id=row["delivery_id"],
        event_kind=JenkinsEventKind(row["event_kind"]),
        build_number=row["build_number"],
        queue_id=row["queue_id"],
        commit_sha=row["commit_sha"],
        branch=row["branch"],
        build_result=JenkinsBuildResult(row["build_result"]) if row["build_result"] is not None else None,
        stage_name=row["stage_name"],
        stage_status=JenkinsStageStatus(row["stage_status"]) if row["stage_status"] is not None else None,
        duration_ms=row["duration_ms"],
        build_url=row["build_url"],
        summary=row["summary"],
        correlation_id=row["correlation_id"],
        occurred_at=_iso_utc(row["occurred_at"]),
        received_at=_iso_utc(row["received_at"]),
        payload_sha256=row["payload_sha256"],
        actor=row["actor"],
        backfilled=bool(row["backfilled"]),
        reconciled=bool(row["reconciled"]),
        completion_time_estimated=bool(row["completion_time_estimated"]),
        started_at=_iso_utc(row["started_at"]),
    )


def _row_to_delivery(row) -> JenkinsDeliveryRecord:
    return JenkinsDeliveryRecord(
        delivery_row_id=str(row["delivery_row_id"]),
        event_id=str(row["event_id"]),
        owner_id=row["owner_id"],
        project_alias=row["project_alias"],
        event_sequence=int(row["event_sequence"]),
        consumer_name=row["consumer_name"],
        status=row["status"],
        attempt_count=int(row["attempt_count"]),
        max_attempts=int(row["max_attempts"]),
        worker_id=row["worker_id"],
        lease_token=str(row["lease_token"]) if row["lease_token"] is not None else None,
        lease_expires_at=_iso_utc(row["lease_expires_at"]),
        next_attempt_at=_iso_utc(row["next_attempt_at"]),
        last_error=row["last_error"],
        receipt_digest=row["receipt_digest"],
        consumed_at=_iso_utc(row["consumed_at"]),
        created_at=_iso_utc(row["created_at"]),
        updated_at=_iso_utc(row["updated_at"]),
    )


class JenkinsStore:
    """Durable PostgreSQL store for Jenkins events, per-consumer deliveries, and observations."""

    def __init__(self, config: JenkinsConfig, connect_factory):
        if not isinstance(config, JenkinsConfig):
            raise TypeError("JenkinsStore requires a frozen JenkinsConfig")
        if not callable(connect_factory):
            raise TypeError("JenkinsStore requires a callable connect_factory")
        self._config = config
        self._connect_factory = connect_factory

    @property
    def owner(self) -> str:
        return self._config.owner

    @contextmanager
    def _transaction(self):
        connection = self._connect_factory()
        try:
            with connection, connection.cursor(cursor_factory=RealDictCursor) as cursor:
                cursor.execute("SET LOCAL lock_timeout = '4s'")
                cursor.execute("SET LOCAL statement_timeout = '5s'")
                yield cursor
        finally:
            connection.close()

    @contextmanager
    def listen(self):
        connection = self._connect_factory()
        try:
            connection.autocommit = True
            with connection.cursor() as cursor:
                cursor.execute(f"LISTEN {NOTIFY_CHANNEL}")
            yield connection
        finally:
            connection.close()

    @staticmethod
    def wait_for_signal(listener, timeout: float, stop=None) -> bool:
        deadline = time.monotonic() + max(0.0, float(timeout))
        while stop is None or not stop.is_set():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return False
            slice_timeout = min(0.25, remaining) if stop is not None else remaining
            if select.select([listener], [], [], slice_timeout)[0]:
                listener.poll()
                notified = bool(listener.notifies)
                listener.notifies.clear()
                if notified:
                    return True
        return False

    def _project_lock(self, cursor, project_alias: str):
        cursor.execute(
            "SELECT pg_advisory_xact_lock(hashtextextended(%s::text, %s::bigint))",
            (f"{self._config.owner}\n{project_alias}", ADVISORY_LOCK_SALT),
        )

    def _owner_capacity_lock(self, cursor):
        # Project aliases are nonempty, so the empty slot is a distinct owner scope.
        cursor.execute(
            "SELECT pg_advisory_xact_lock(hashtextextended(%s::text, %s::bigint))",
            (f"{self._config.owner}\n", ADVISORY_LOCK_SALT),
        )

    def _require_admitted(self, event: JenkinsEventEmit):
        if event.project_alias not in self._config.allowed_projects:
            raise Rejected(403, "denied")
        origin = self._config.instance_origin(event.jenkins_instance)
        if origin is None:
            raise Rejected(403, "denied")
        if not self._config.is_job_allowed(event.project_alias, event.job_name):
            raise Rejected(403, "denied")
        if event.build_url is not None:
            try:
                validate_build_url(event.build_url, expected_origin=origin)
            except ValueError:
                raise Rejected(400, "invalid_request") from None

    def _event_scope(self):
        """Parameterized current-grant predicate for an event aliased as e."""
        clauses = ['e.owner_id=%s', 'e.project_alias=ANY(%s)']
        params = [self._config.owner, sorted(self._config.allowed_projects)]
        instances = []
        for alias, instance in sorted(self._config.allowed_instances.items()):
            origins = [instance.origin]
            if urlsplit(instance.origin).port is None:
                origins.append(instance.origin + (':443' if instance.origin.startswith('https:') else ':80'))
            url_clauses = ['e.build_url IS NULL','lower(e.build_url)=ANY(%s)']
            params.extend((alias,[origin.lower() for origin in origins]))
            for origin in origins:
                for boundary in ('/','?','#'):
                    prefix = origin.lower() + boundary
                    url_clauses.append('left(lower(e.build_url),length(%s))=%s')
                    params.extend((prefix,prefix))
            instances.append('(e.jenkins_instance=%s AND ('+' OR '.join(url_clauses)+'))')
        clauses.append('('+' OR '.join(instances)+')')
        jobs = []
        for project in sorted(self._config.allowed_projects):
            jobs.append('(e.project_alias=%s AND e.job_name=ANY(%s))')
            params.extend((project,sorted(self._config.allowed_jobs[project])))
        clauses.append('('+' OR '.join(jobs)+')')
        return ' AND '.join(clauses),params

    def _delivery_scope(self, consumer_names=None):
        """Apply current enabled subscription grants before any lease effect."""
        enabled = {consumer.name:consumer for consumer in self._config.consumers if consumer.enabled}
        selected = sorted(enabled) if consumer_names is None else consumer_names
        if (not isinstance(selected,(list,tuple)) or len(selected)>len(enabled)
                or any(not isinstance(name,str) or name not in enabled for name in selected)
                or len(set(selected))!=len(selected)):
            raise ValueError('Consumer selection exceeds the current enabled catalog')
        if not selected:
            return 'false',[]
        scope,params = self._event_scope()
        subscriptions = []
        for name in selected:
            consumer = enabled[name]
            projects = sorted(project for project in self._config.allowed_projects
                if not consumer.projects or project in consumer.projects)
            kinds = sorted(kind.value for kind in JenkinsEventKind
                if not consumer.event_kinds or kind in consumer.event_kinds)
            subscriptions.append('(d.consumer_name=%s AND d.project_alias=ANY(%s) AND e.event_kind=ANY(%s))')
            params.extend((name,projects,kinds))
        return scope+' AND ('+' OR '.join(subscriptions)+')',params

    def emit(self, event: JenkinsEventEmit) -> dict:
        if not isinstance(event, JenkinsEventEmit):
            raise TypeError("JenkinsStore.emit requires a JenkinsEventEmit")
        self._require_admitted(event)
        digest = event.payload_sha256()
        subscribers = self._config.subscribed_consumers(event.project_alias, event.event_kind)

        with self._transaction() as cursor:
            # Capacity is owner-wide. Always take this before the project sequence lock.
            self._owner_capacity_lock(cursor)
            self._project_lock(cursor, event.project_alias)

            cursor.execute(
                """
                SELECT event_id, project_alias, sequence, payload_sha256
                FROM forge_agent.jenkins_events
                WHERE owner_id = %s
                  AND project_alias = %s
                  AND jenkins_instance = %s
                  AND delivery_id = %s
                """,
                (self._config.owner, event.project_alias, event.jenkins_instance, event.delivery_id),
            )
            existing = cursor.fetchone()
            if existing is not None:
                if existing["payload_sha256"] != digest:
                    raise Rejected(409, "idempotency_conflict")
                cursor.execute(
                    "SELECT count(*) AS count FROM forge_agent.jenkins_event_deliveries WHERE event_id = %s",
                    (existing["event_id"],),
                )
                delivery_count = int(cursor.fetchone()["count"])
                return {
                    "event_id": str(existing["event_id"]),
                    "project_alias": existing["project_alias"],
                    "sequence": int(existing["sequence"]),
                    "payload_sha256": existing["payload_sha256"],
                    "deduplicated": True,
                    "deliveries_queued": delivery_count,
                }

            cursor.execute(
                """
                SELECT count(*) AS pending_count
                FROM forge_agent.jenkins_event_deliveries
                WHERE owner_id = %s
                  AND status IN ('pending', 'leased', 'failed')
                """,
                (self._config.owner,),
            )
            pending_count = int(cursor.fetchone()["pending_count"])
            if pending_count + len(subscribers) > self._config.max_pending_deliveries:
                raise Rejected(503, "queue_full")

            cursor.execute(
                """
                SELECT coalesce(max(sequence), 0) + 1 AS next_seq
                FROM forge_agent.jenkins_events
                WHERE owner_id = %s AND project_alias = %s
                """,
                (self._config.owner, event.project_alias),
            )
            next_seq = int(cursor.fetchone()["next_seq"])

            cursor.execute(
                """
                INSERT INTO forge_agent.jenkins_events (
                    owner_id, project_alias, sequence, jenkins_instance, job_name, delivery_id,
                    event_kind, build_number, queue_id, commit_sha, branch, build_result,
                    stage_name, stage_status, duration_ms, build_url, summary,
                    correlation_id, occurred_at, payload_sha256,
                    actor, backfilled, reconciled, completion_time_estimated, started_at
                )
                VALUES (
                    %s, %s, %s, %s, %s, %s,
                    %s, %s, %s, %s, %s, %s,
                    %s, %s, %s, %s, %s,
                    %s, %s, %s,
                    %s, %s, %s, %s, %s
                )
                RETURNING event_id, project_alias, sequence, payload_sha256
                """,
                (
                    self._config.owner,
                    event.project_alias,
                    next_seq,
                    event.jenkins_instance,
                    event.job_name,
                    event.delivery_id,
                    event.event_kind.value,
                    event.build_number,
                    event.queue_id,
                    event.commit_sha,
                    event.branch,
                    event.build_result.value if event.build_result is not None else None,
                    event.stage_name,
                    event.stage_status.value if event.stage_status is not None else None,
                    event.duration_ms,
                    event.build_url,
                    event.summary,
                    event.correlation_id,
                    event.occurred_at,
                    digest,
                    event.actor,
                    event.backfilled,
                    event.reconciled,
                    event.completion_time_estimated,
                    event.started_at,
                ),
            )
            inserted = cursor.fetchone()

            for subscriber in subscribers:
                cursor.execute(
                    """
                    INSERT INTO forge_agent.jenkins_event_deliveries (
                        event_id, owner_id, project_alias, event_sequence,
                        consumer_name, status, max_attempts
                    )
                    VALUES (%s, %s, %s, %s, %s, 'pending', %s)
                    """,
                    (
                        inserted["event_id"],
                        self._config.owner,
                        event.project_alias,
                        next_seq,
                        subscriber.name,
                        subscriber.max_attempts,
                    ),
                )

            cursor.execute("SELECT pg_catalog.pg_notify(%s, '')", (NOTIFY_CHANNEL,))

            return {
                "event_id": str(inserted["event_id"]),
                "project_alias": inserted["project_alias"],
                "sequence": int(inserted["sequence"]),
                "payload_sha256": inserted["payload_sha256"],
                "deduplicated": False,
                "deliveries_queued": len(subscribers),
            }

    def list_events(
        self,
        *,
        project_alias: str,
        after_sequence: int = 0,
        limit: int = EVENTS_DEFAULT_LIMIT,
        job_name: str | None = None,
        event_kind: JenkinsEventKind | str | None = None,
        commit_sha: str | None = None,
    ) -> dict:
        validate_identifier(project_alias, "project_alias")
        if project_alias not in self._config.allowed_projects:
            raise Rejected(403, "denied")
        validate_int_range(after_sequence, 0, MAX_SEQUENCE, "after_sequence")
        validate_int_range(limit, 1, EVENTS_MAX_LIMIT, "limit")
        clauses = ["owner_id = %s", "project_alias = %s", "sequence > %s"]
        params: list = [self._config.owner, project_alias, after_sequence]
        if job_name is not None:
            validate_job_name(job_name)
            clauses.append("job_name = %s")
            params.append(job_name)
        if event_kind is not None:
            kind_val = event_kind.value if isinstance(event_kind, JenkinsEventKind) else JenkinsEventKind(event_kind).value
            clauses.append("event_kind = %s")
            params.append(kind_val)
        if commit_sha is not None:
            validate_commit_sha(commit_sha)
            clauses.append("commit_sha = %s")
            params.append(commit_sha)
        params.append(limit + 1)

        with self._transaction() as cursor:
            cursor.execute(
                f"""
                SELECT *
                FROM forge_agent.jenkins_events
                WHERE {' AND '.join(clauses)}
                ORDER BY sequence ASC
                LIMIT %s
                """,
                params,
            )
            rows = cursor.fetchall()

        has_more = len(rows) > limit
        page_rows = rows[:limit]
        events = [_row_to_event(row).to_dict() for row in page_rows]
        next_after = events[-1]["sequence"] if events else (after_sequence if after_sequence > 0 else None)
        return {
            "project_alias": project_alias,
            "events": events,
            "has_more": has_more,
            "next_after_sequence": next_after,
        }

    def get_event(self, *, project_alias: str, event_id: UUID | str) -> dict:
        validate_identifier(project_alias, "project_alias")
        if project_alias not in self._config.allowed_projects:
            raise Rejected(403, "denied")
        event_uuid = event_id if isinstance(event_id, UUID) else UUID(str(event_id))

        with self._transaction() as cursor:
            cursor.execute(
                """
                SELECT *
                FROM forge_agent.jenkins_events
                WHERE owner_id = %s
                  AND project_alias = %s
                  AND event_id = %s
                """,
                (self._config.owner, project_alias, event_uuid),
            )
            event_row = cursor.fetchone()
            if event_row is None:
                raise Rejected(404, "not_found")
            cursor.execute(
                """
                SELECT *
                FROM forge_agent.jenkins_event_deliveries
                WHERE owner_id = %s AND event_id = %s
                ORDER BY consumer_name ASC
                """,
                (self._config.owner, event_uuid),
            )
            delivery_rows = cursor.fetchall()

        return {
            "event": _row_to_event(event_row).to_dict(),
            "deliveries": [_row_to_delivery(row).to_public_dict() for row in delivery_rows],
        }

    def list_deliveries(
        self,
        *,
        project_alias: str,
        consumer_name: str | None = None,
        status: str | None = None,
        after_sequence: int = 0,
        after_consumer: str | None = None,
        limit: int = DELIVERIES_DEFAULT_LIMIT,
    ) -> dict:
        validate_identifier(project_alias, "project_alias")
        if project_alias not in self._config.allowed_projects:
            raise Rejected(403, "denied")
        validate_int_range(after_sequence, 0, MAX_SEQUENCE, "after_sequence")
        if after_consumer is not None:
            validate_identifier(after_consumer, "after_consumer")
            if after_sequence == 0:
                raise ValueError("after_consumer requires after_sequence >= 1")
        validate_int_range(limit, 1, DELIVERIES_MAX_LIMIT, "limit")
        clauses = ["owner_id = %s", "project_alias = %s"]
        params: list = [self._config.owner, project_alias]
        if after_consumer is not None:
            clauses.append("(event_sequence, consumer_name) > (%s, %s)")
            params.extend((after_sequence, after_consumer))
        else:
            clauses.append("event_sequence > %s")
            params.append(after_sequence)
        if consumer_name is not None:
            validate_identifier(consumer_name, "consumer_name")
            clauses.append("consumer_name = %s")
            params.append(consumer_name)
        if status is not None:
            if status not in VALID_DELIVERY_STATUSES:
                raise Rejected(400, "invalid_request")
            clauses.append("status = %s")
            params.append(status)
        params.append(limit + 1)

        with self._transaction() as cursor:
            cursor.execute(
                f"""
                SELECT *
                FROM forge_agent.jenkins_event_deliveries
                WHERE {' AND '.join(clauses)}
                ORDER BY event_sequence ASC, consumer_name ASC
                LIMIT %s
                """,
                params,
            )
            rows = cursor.fetchall()

        has_more = len(rows) > limit
        page_rows = rows[:limit]
        deliveries = [_row_to_delivery(row).to_public_dict() for row in page_rows]
        if deliveries:
            next_after_sequence = deliveries[-1]["event_sequence"]
            next_after_consumer = deliveries[-1]["consumer_name"]
        elif after_sequence > 0:
            next_after_sequence = after_sequence
            next_after_consumer = after_consumer
        else:
            next_after_sequence = None
            next_after_consumer = None
        return {
            "project_alias": project_alias,
            "deliveries": deliveries,
            "has_more": has_more,
            "next_after_sequence": next_after_sequence,
            "next_after_consumer": next_after_consumer,
        }

    def claim_next(
        self,
        consumer_names: tuple[str, ...],
        worker_id: str,
        *,
        lease_seconds: int | None = None,
    ) -> tuple[JenkinsEventRecord, JenkinsDeliveryRecord] | None:
        if not consumer_names:
            return None
        effective_lease = self._config.lease_seconds if lease_seconds is None else lease_seconds
        validate_int_range(effective_lease, 5, 600, 'lease_seconds')
        validate_identifier(worker_id, 'worker_id')
        scope,scope_params = self._delivery_scope(consumer_names)

        with self._transaction() as cursor:
            # Exhausted expired leases transition cleanly to dead_lettered instead of looping.
            cursor.execute(
                f"""
                WITH exhausted AS (
                    SELECT d.delivery_row_id
                    FROM forge_agent.jenkins_event_deliveries d
                    JOIN forge_agent.jenkins_events e ON(e.owner_id,e.project_alias,e.event_id,e.sequence)
                      =(d.owner_id,d.project_alias,d.event_id,d.event_sequence)
                    WHERE {scope}
                      AND d.status = 'leased'
                      AND d.lease_expires_at <= clock_timestamp()
                      AND d.attempt_count >= d.max_attempts
                    ORDER BY d.event_sequence ASC, d.delivery_row_id ASC
                    FOR UPDATE OF d SKIP LOCKED
                    LIMIT 32
                )
                UPDATE forge_agent.jenkins_event_deliveries d
                SET status = 'dead_lettered',
                    worker_id = NULL,
                    lease_token = NULL,
                    lease_expires_at = NULL,
                    last_error = COALESCE(d.last_error, 'lease_expired_max_attempts_exhausted'),
                    updated_at = clock_timestamp()
                FROM exhausted
                WHERE d.delivery_row_id = exhausted.delivery_row_id
                """,
                scope_params,
            )

            cursor.execute(
                f"""
                SELECT d.delivery_row_id
                FROM forge_agent.jenkins_event_deliveries d
                JOIN forge_agent.jenkins_events e ON(e.owner_id,e.project_alias,e.event_id,e.sequence)
                  =(d.owner_id,d.project_alias,d.event_id,d.event_sequence)
                WHERE {scope}
                  AND (
                      (d.status = 'pending' AND d.next_attempt_at <= clock_timestamp())
                      OR (d.status = 'failed'
                          AND d.attempt_count < d.max_attempts
                          AND d.next_attempt_at <= clock_timestamp())
                      OR (d.status = 'leased'
                          AND d.attempt_count < d.max_attempts
                          AND d.lease_expires_at <= clock_timestamp())
                  )
                ORDER BY d.next_attempt_at ASC, d.event_sequence ASC, d.delivery_row_id ASC
                LIMIT 1
                FOR UPDATE OF d SKIP LOCKED
                """,
                scope_params,
            )
            candidate = cursor.fetchone()
            if candidate is None:
                return None

            token = uuid4()
            cursor.execute(
                """
                UPDATE forge_agent.jenkins_event_deliveries
                SET status = 'leased',
                    attempt_count = attempt_count + 1,
                    worker_id = %s,
                    lease_token = %s,
                    lease_expires_at = clock_timestamp() + make_interval(secs => %s),
                    updated_at = clock_timestamp()
                WHERE delivery_row_id = %s
                RETURNING *
                """,
                (worker_id, token, effective_lease, candidate["delivery_row_id"]),
            )
            delivery_row = cursor.fetchone()

            cursor.execute(
                """
                SELECT *
                FROM forge_agent.jenkins_events
                WHERE owner_id = %s AND event_id = %s
                """,
                (self._config.owner, delivery_row["event_id"]),
            )
            event_row = cursor.fetchone()

        return _row_to_event(event_row), _row_to_delivery(delivery_row)

    def renew_lease(
        self,
        delivery_row_id: UUID | str,
        lease_token: UUID | str,
        *,
        lease_seconds: int | None = None,
    ) -> bool:
        effective_lease = self._config.lease_seconds if lease_seconds is None else lease_seconds
        validate_int_range(effective_lease, 5, 600, 'lease_seconds')
        scope,scope_params = self._delivery_scope()
        row_uuid = delivery_row_id if isinstance(delivery_row_id, UUID) else UUID(str(delivery_row_id))
        token_uuid = lease_token if isinstance(lease_token, UUID) else UUID(str(lease_token))
        with self._transaction() as cursor:
            cursor.execute(
                f"""
                UPDATE forge_agent.jenkins_event_deliveries d
                SET lease_expires_at = clock_timestamp() + make_interval(secs => %s),
                    updated_at = clock_timestamp()
                WHERE owner_id = %s
                  AND delivery_row_id = %s
                  AND status = 'leased'
                  AND lease_token = %s
                  AND lease_expires_at > clock_timestamp()
                  AND EXISTS(SELECT FROM forge_agent.jenkins_events e
                    WHERE(e.owner_id,e.project_alias,e.event_id,e.sequence)
                      =(d.owner_id,d.project_alias,d.event_id,d.event_sequence) AND {scope})
                RETURNING delivery_row_id
                """,
                (effective_lease, self._config.owner, row_uuid, token_uuid,*scope_params),
            )
            return cursor.fetchone() is not None

    def complete_delivery(
        self,
        delivery_row_id: UUID | str,
        lease_token: UUID | str,
        receipt_digest: str,
    ) -> bool:
        validate_sha256(receipt_digest, "receipt_digest")
        scope,scope_params = self._delivery_scope()
        row_uuid = delivery_row_id if isinstance(delivery_row_id, UUID) else UUID(str(delivery_row_id))
        token_uuid = lease_token if isinstance(lease_token, UUID) else UUID(str(lease_token))
        with self._transaction() as cursor:
            cursor.execute(
                f"""
                UPDATE forge_agent.jenkins_event_deliveries d
                SET status = 'consumed',
                    worker_id = NULL,
                    lease_token = NULL,
                    lease_expires_at = NULL,
                    receipt_digest = %s,
                    consumed_at = clock_timestamp(),
                    updated_at = clock_timestamp()
                WHERE owner_id = %s
                  AND delivery_row_id = %s
                  AND status = 'leased'
                  AND lease_token = %s
                  AND lease_expires_at > clock_timestamp()
                  AND EXISTS(SELECT FROM forge_agent.jenkins_events e
                    WHERE(e.owner_id,e.project_alias,e.event_id,e.sequence)
                      =(d.owner_id,d.project_alias,d.event_id,d.event_sequence) AND {scope})
                RETURNING delivery_row_id
                """,
                (receipt_digest, self._config.owner, row_uuid, token_uuid,*scope_params),
            )
            return cursor.fetchone() is not None

    def fail_delivery(
        self,
        delivery_row_id: UUID | str,
        lease_token: UUID | str,
        error_message: str,
        *,
        retryable: bool,
        backoff_seconds: int | None = None,
        max_backoff_seconds: int | None = None,
    ) -> str | None:
        sanitized = sanitize_error_message(error_message)
        base_backoff = self._config.backoff_seconds if backoff_seconds is None else backoff_seconds
        cap_backoff = self._config.max_backoff_seconds if max_backoff_seconds is None else max_backoff_seconds
        scope,scope_params = self._delivery_scope()
        row_uuid = delivery_row_id if isinstance(delivery_row_id, UUID) else UUID(str(delivery_row_id))
        token_uuid = lease_token if isinstance(lease_token, UUID) else UUID(str(lease_token))

        with self._transaction() as cursor:
            cursor.execute(
                f"""
                SELECT attempt_count, max_attempts
                FROM forge_agent.jenkins_event_deliveries d
                WHERE owner_id = %s
                  AND delivery_row_id = %s
                  AND status = 'leased'
                  AND lease_token = %s
                  AND lease_expires_at > clock_timestamp()
                  AND EXISTS(SELECT FROM forge_agent.jenkins_events e
                    WHERE(e.owner_id,e.project_alias,e.event_id,e.sequence)
                      =(d.owner_id,d.project_alias,d.event_id,d.event_sequence) AND {scope})
                FOR UPDATE OF d
                """,
                (self._config.owner, row_uuid, token_uuid,*scope_params),
            )
            row = cursor.fetchone()
            if row is None:
                return None

            attempt_count = int(row["attempt_count"])
            max_attempts = int(row["max_attempts"])
            if retryable and attempt_count < max_attempts:
                delay = min(cap_backoff, base_backoff * (2 ** max(0, attempt_count - 1)))
                cursor.execute(
                    """
                    UPDATE forge_agent.jenkins_event_deliveries
                    SET status = 'failed',
                        worker_id = NULL,
                        lease_token = NULL,
                        lease_expires_at = NULL,
                        next_attempt_at = clock_timestamp() + make_interval(secs => %s),
                        last_error = %s,
                        updated_at = clock_timestamp()
                    WHERE delivery_row_id = %s
                    """,
                    (delay, sanitized, row_uuid),
                )
                return "failed"

            cursor.execute(
                """
                UPDATE forge_agent.jenkins_event_deliveries
                SET status = 'dead_lettered',
                    worker_id = NULL,
                    lease_token = NULL,
                    lease_expires_at = NULL,
                    last_error = %s,
                    updated_at = clock_timestamp()
                WHERE delivery_row_id = %s
                """,
                (sanitized, row_uuid),
            )
            return "dead_lettered"

    def upsert_observation(self, event: JenkinsEventRecord) -> dict:
        if not isinstance(event, JenkinsEventRecord):
            raise TypeError("upsert_observation requires a JenkinsEventRecord")
        if event.owner_id != self._config.owner:
            raise Rejected(403,'denied')
        self._require_admitted(event)
        obs_key = event.observation_key()
        with self._transaction() as cursor:
            cursor.execute(
                """
                INSERT INTO forge_agent.jenkins_observations (
                    owner_id, project_alias, jenkins_instance, job_name, observation_key,
                    build_number, queue_id, last_event_id, last_event_sequence, last_event_kind,
                    commit_sha, branch, build_result, stage_name, stage_status,
                    duration_ms, build_url, summary, occurred_at, updated_at
                )
                VALUES (
                    %s, %s, %s, %s, %s,
                    %s, %s, %s, %s, %s,
                    %s, %s, %s, %s, %s,
                    %s, %s, %s, %s, clock_timestamp()
                )
                ON CONFLICT (owner_id, project_alias, jenkins_instance, job_name, observation_key)
                DO UPDATE SET
                    build_number = COALESCE(EXCLUDED.build_number, forge_agent.jenkins_observations.build_number),
                    queue_id = COALESCE(EXCLUDED.queue_id, forge_agent.jenkins_observations.queue_id),
                    last_event_id = EXCLUDED.last_event_id,
                    last_event_sequence = EXCLUDED.last_event_sequence,
                    last_event_kind = EXCLUDED.last_event_kind,
                    commit_sha = COALESCE(EXCLUDED.commit_sha, forge_agent.jenkins_observations.commit_sha),
                    branch = COALESCE(EXCLUDED.branch, forge_agent.jenkins_observations.branch),
                    build_result = COALESCE(EXCLUDED.build_result, forge_agent.jenkins_observations.build_result),
                    stage_name = EXCLUDED.stage_name,
                    stage_status = EXCLUDED.stage_status,
                    duration_ms = COALESCE(EXCLUDED.duration_ms, forge_agent.jenkins_observations.duration_ms),
                    build_url = COALESCE(EXCLUDED.build_url, forge_agent.jenkins_observations.build_url),
                    summary = EXCLUDED.summary,
                    occurred_at = EXCLUDED.occurred_at,
                    updated_at = clock_timestamp()
                WHERE EXCLUDED.last_event_sequence > forge_agent.jenkins_observations.last_event_sequence
                RETURNING last_event_id, last_event_sequence, observation_key
                """,
                (
                    self._config.owner,
                    event.project_alias,
                    event.jenkins_instance,
                    event.job_name,
                    obs_key,
                    event.build_number,
                    event.queue_id,
                    UUID(event.event_id),
                    event.sequence,
                    event.event_kind.value,
                    event.commit_sha,
                    event.branch,
                    event.build_result.value if event.build_result is not None else None,
                    event.stage_name,
                    event.stage_status.value if event.stage_status is not None else None,
                    event.duration_ms,
                    event.build_url,
                    event.summary,
                    event.occurred_at,
                ),
            )
            row = cursor.fetchone()
        return {
            "observation_key": obs_key,
            "applied": row is not None,
        }

    def get_observation(
        self,
        *,
        project_alias: str,
        jenkins_instance: str,
        job_name: str,
        observation_key: str,
    ) -> dict | None:
        if (project_alias not in self._config.allowed_projects
                or self._config.instance_origin(jenkins_instance) is None
                or not self._config.is_job_allowed(project_alias,job_name)):
            raise Rejected(403,'denied')
        with self._transaction() as cursor:
            cursor.execute(
                """
                SELECT *
                FROM forge_agent.jenkins_observations
                WHERE owner_id = %s
                  AND project_alias = %s
                  AND jenkins_instance = %s
                  AND job_name = %s
                  AND observation_key = %s
                """,
                (self._config.owner, project_alias, jenkins_instance, job_name, observation_key),
            )
            row = cursor.fetchone()
        if row is None:
            return None
        return {
            "project_alias": row["project_alias"],
            "jenkins_instance": row["jenkins_instance"],
            "job_name": row["job_name"],
            "observation_key": row["observation_key"],
            "build_number": row["build_number"],
            "queue_id": row["queue_id"],
            "last_event_id": str(row["last_event_id"]),
            "last_event_sequence": int(row["last_event_sequence"]),
            "last_event_kind": row["last_event_kind"],
            "commit_sha": row["commit_sha"],
            "branch": row["branch"],
            "build_result": row["build_result"],
            "stage_name": row["stage_name"],
            "stage_status": row["stage_status"],
            "duration_ms": row["duration_ms"],
            "build_url": row["build_url"],
            "summary": row["summary"],
            "occurred_at": _iso_utc(row["occurred_at"]),
            "updated_at": _iso_utc(row["updated_at"]),
        }
