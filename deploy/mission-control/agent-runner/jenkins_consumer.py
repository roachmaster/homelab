"""Provider-wired leased consumer loop and built-in observation handler for Jenkins events."""

import logging
import threading
import time
from contextlib import ExitStack
from types import MappingProxyType
from typing import Mapping, Protocol, runtime_checkable

from jenkins_config import JenkinsConfig
from jenkins_events import (
    ConsumptionReceipt,
    JenkinsDeliveryRecord,
    JenkinsEventRecord,
    sanitize_error_message,
    sha256_canonical,
)

LOG = logging.getLogger("forge-jenkins-consumer")
# JenkinsStore bounds every renewal statement at five seconds. Allow that
# transaction to settle, then surface any remaining owned thread explicitly.
HEARTBEAT_STOP_SECONDS = 6.0


class LeaseLostError(RuntimeError):
    """A handler cannot finish a delivery after its supporting lease was lost."""


class HeartbeatCleanupError(RuntimeError):
    """Supporting I/O is still active; the host must retain owned resources."""


class RetryableHandlerError(Exception):
    """Raised by a JenkinsEventHandler to schedule a bounded backoff retry."""


class PermanentHandlerError(Exception):
    """Raised by a JenkinsEventHandler to dead-letter a delivery without retrying."""


@runtime_checkable
class JenkinsEventHandler(Protocol):
    def handle(self, event: JenkinsEventRecord, delivery: JenkinsDeliveryRecord) -> ConsumptionReceipt:
        ...


class ObservationJenkinsHandler:
    """Default built-in sink projecting Jenkins events into monotonic observations."""

    def __init__(self, store):
        if store is None:
            raise TypeError("ObservationJenkinsHandler requires an injected store")
        self._store = store

    def handle(self, event: JenkinsEventRecord, delivery: JenkinsDeliveryRecord) -> ConsumptionReceipt:
        self._store.upsert_observation(event)
        obs_key = event.observation_key()
        digest = sha256_canonical({
            "consumer_name": delivery.consumer_name,
            "event_id": event.event_id,
            "event_sequence": event.sequence,
            "payload_sha256": event.payload_sha256,
            "observation_key": obs_key,
        })
        return ConsumptionReceipt(
            consumer_name=delivery.consumer_name,
            event_id=event.event_id,
            receipt_digest=digest,
            observation_key=obs_key,
        )


DEFAULT_HANDLER_FACTORIES = MappingProxyType({
    "observation": ObservationJenkinsHandler,
})


class JenkinsEventConsumer:
    """Host-owned background worker claiming and executing leased Jenkins event deliveries."""

    def __init__(
        self,
        config: JenkinsConfig,
        store,
        handlers: Mapping[str, JenkinsEventHandler],
        *,
        worker_id: str = "jenkins-consumer",
        stop: threading.Event | None = None,
        owned_handlers=None,
    ):
        if not isinstance(config, JenkinsConfig):
            raise TypeError("JenkinsEventConsumer requires a frozen JenkinsConfig")
        if store is None:
            raise TypeError("JenkinsEventConsumer requires an injected store")
        if not isinstance(handlers, Mapping):
            raise TypeError("JenkinsEventConsumer requires a mapping of consumer_name -> handler")
        enabled_names = {c.name for c in config.consumers if c.enabled}
        for name, handler in handlers.items():
            if name not in enabled_names:
                raise ValueError(f"Handler configured for unknown or disabled consumer: {name}")
            if not callable(getattr(handler, "handle", None)):
                raise TypeError(f"Handler for consumer {name} must implement handle(event, delivery)")
        self._config = config
        self._store = store
        self._handlers = dict(handlers)
        self._consumer_names = tuple(sorted(self._handlers.keys()))
        self.worker_id = worker_id
        self.stop = stop if stop is not None else threading.Event()
        self._thread: threading.Thread | None = None
        self._owned_handlers = owned_handlers
        self._heartbeats = set()
        self._heartbeat_lock = threading.Lock()
        self._active_calls = 0

    @property
    def handlers(self) -> dict[str, JenkinsEventHandler]:
        return dict(self._handlers)

    def _run_with_heartbeat(self, event: JenkinsEventRecord, delivery: JenkinsDeliveryRecord, handler):
        done = threading.Event()
        lost = threading.Event()
        renew_interval = self._config.renew_seconds

        def _heartbeat():
            while not done.wait(renew_interval):
                if self.stop.is_set():
                    return
                try:
                    if not self._store.renew_lease(
                        delivery.delivery_row_id,
                        delivery.lease_token,
                        lease_seconds=self._config.lease_seconds,
                    ):
                        lost.set()
                        return
                except Exception:
                    lost.set()
                    LOG.warning("Jenkins lease renewal failed for %s", delivery.delivery_row_id)
                    return

        hb_thread = threading.Thread(
            target=_heartbeat,
            name=f"forge-jenkins-hb-{delivery.delivery_row_id[:8]}",
            daemon=True,
        )
        with self._heartbeat_lock:
            self._heartbeats.add(hb_thread)
        try:
            hb_thread.start()
        except BaseException:
            with self._heartbeat_lock:
                self._heartbeats.discard(hb_thread)
            raise
        try:
            receipt = handler.handle(event, delivery)
        finally:
            done.set()
            hb_thread.join(timeout=HEARTBEAT_STOP_SECONDS)
            if hb_thread.is_alive():
                self.stop.set()
                raise HeartbeatCleanupError('Jenkins lease heartbeat has not quiesced')
            with self._heartbeat_lock:
                self._heartbeats.discard(hb_thread)
        if lost.is_set():
            raise LeaseLostError('Jenkins delivery lease ended during the handler')
        return receipt

    def run_once(self) -> bool:
        with self._heartbeat_lock:
            if self.stop.is_set():
                return False
            self._active_calls += 1
        try:
            return self._run_once()
        finally:
            with self._heartbeat_lock:
                self._active_calls -= 1

    def _run_once(self) -> bool:
        if not self._consumer_names:
            return False
        claimed = self._store.claim_next(
            self._consumer_names,
            self.worker_id,
            lease_seconds=self._config.lease_seconds,
        )
        if claimed is None:
            return False
        event, delivery = claimed
        handler = self._handlers[delivery.consumer_name]
        try:
            receipt = self._run_with_heartbeat(event, delivery, handler)
            if (not isinstance(receipt, ConsumptionReceipt)
                    or receipt.event_id != event.event_id
                    or receipt.consumer_name != delivery.consumer_name):
                raise PermanentHandlerError("invalid_consumption_receipt")
        except HeartbeatCleanupError:
            raise  # Leave the uncertain delivery leased for fenced recovery.
        except LeaseLostError:
            return True  # No completion, failure or dead-letter on a lost lease.
        except RetryableHandlerError as error:
            self._store.fail_delivery(
                delivery.delivery_row_id,
                delivery.lease_token,
                sanitize_error_message(str(error), fallback="retryable_handler_error"),
                retryable=True,
                backoff_seconds=self._config.backoff_seconds,
                max_backoff_seconds=self._config.max_backoff_seconds,
            )
            return True
        except PermanentHandlerError as error:
            self._store.fail_delivery(
                delivery.delivery_row_id,
                delivery.lease_token,
                sanitize_error_message(str(error), fallback="permanent_handler_error"),
                retryable=False,
            )
            return True
        except Exception as error:
            LOG.warning(
                "Jenkins consumer handler error on %s (%s)",
                delivery.delivery_row_id,
                type(error).__name__,
            )
            self._store.fail_delivery(
                delivery.delivery_row_id,
                delivery.lease_token,
                sanitize_error_message(f"unhandled_error:{type(error).__name__}"),
                retryable=False,
            )
            return True

        completed = self._store.complete_delivery(
            delivery.delivery_row_id,
            delivery.lease_token,
            receipt.receipt_digest,
        )
        if not completed:
            LOG.warning("Jenkins delivery %s lost lease before completion", delivery.delivery_row_id)
        return True

    def run(self):
        while not self.stop.is_set():
            try:
                # Subscribe before scanning so a commit cannot fall between scan and wait.
                with self._store.listen() as listener:
                    while not self.stop.is_set():
                        while not self.stop.is_set() and self.run_once():
                            pass
                        self._store.wait_for_signal(
                            listener,
                            self._config.poll_interval_seconds,
                            self.stop,
                        )
            except Exception:
                LOG.warning("Jenkins event consumer reconnecting", exc_info=True)
                self.stop.wait(1)

    def start(self):
        if self._thread is not None or self.stop.is_set():
            raise RuntimeError("Jenkins event consumer already started")
        self._thread = threading.Thread(
            target=self.run,
            name=f"forge-{self.worker_id}",
            daemon=True,
        )
        try:
            self._thread.start()
        except BaseException:
            if self._thread.ident is None:
                self._thread = None
            raise

    def close(self, *, timeout: float = 15.0):
        self.stop.set()
        deadline = time.monotonic() + max(0.0, timeout)
        if self._thread is not None:
            self._thread.join(timeout=max(0.0, deadline-time.monotonic()))
            if self._thread.is_alive():
                raise RuntimeError("Jenkins event consumer did not quiesce")
            self._thread = None
        with self._heartbeat_lock:
            heartbeats = tuple(self._heartbeats)
        for heartbeat in heartbeats:
            heartbeat.join(timeout=max(0.0, deadline-time.monotonic()))
            if heartbeat.is_alive():
                raise HeartbeatCleanupError('Jenkins lease heartbeat remains active')
            with self._heartbeat_lock:
                self._heartbeats.discard(heartbeat)
        with self._heartbeat_lock:
            if self._active_calls:
                raise RuntimeError('Jenkins delivery handler remains active')
        if self._owned_handlers is not None:
            self._owned_handlers.close()
            self._owned_handlers = None


class JenkinsConsumerFactory:
    """Effect-free startup factory selecting handler implementations from an explicit mapping."""

    def __init__(
        self,
        config: JenkinsConfig,
        store,
        *,
        handler_factories: Mapping[str, object] | None = None,
        worker_id: str = "jenkins-consumer",
    ):
        if not isinstance(config, JenkinsConfig):
            raise TypeError("JenkinsConsumerFactory requires a frozen JenkinsConfig")
        if store is None:
            raise TypeError("JenkinsConsumerFactory requires an injected store")
        selected = dict(DEFAULT_HANDLER_FACTORIES) if handler_factories is None else dict(handler_factories)
        for key, factory in selected.items():
            if not isinstance(key, str) or not callable(factory):
                raise TypeError("handler_factories must map handler names to callables")
        for consumer in config.consumers:
            if consumer.enabled and consumer.handler not in selected:
                raise ValueError(f"Unregistered Jenkins consumer handler: {consumer.handler}")
        self._config = config
        self._store = store
        self._handler_factories = selected
        self._worker_id = worker_id

    def create(self) -> JenkinsEventConsumer:
        with ExitStack() as cleanup:
            handlers = {}
            for consumer in self._config.consumers:
                if consumer.enabled:
                    handler = self._handler_factories[consumer.handler](self._store)
                    close = getattr(handler, 'close', None)
                    if callable(close):
                        cleanup.callback(close)
                    handlers[consumer.name] = handler
            worker = JenkinsEventConsumer(self._config, self._store, handlers, worker_id=self._worker_id)
            worker._owned_handlers = cleanup.pop_all()
            return worker
