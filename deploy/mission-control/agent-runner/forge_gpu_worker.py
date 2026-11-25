"""Claim PostgreSQL jobs and execute them against the Forge-local model adapter."""

from contextlib import closing, contextmanager
import logging
import os
from pathlib import Path
from queue import Empty, Queue
import re
import select
import threading
import time

import psycopg2
from psycopg2.extras import Json

from model_adapter import MAIL_MODEL, ModelAdapter, ModelError


MODEL_PORT = 18080
POSTGRES_PORT = 15432
SCAN_SECONDS = 5
RENEW_SECONDS = 10
MODEL_RETRY_SECONDS = 30
MODELS = {"gpt-oss-20b", "qwen2.5-coder-7b", MAIL_MODEL}
LOG = logging.getLogger("forge-gpu-worker")


class WorkerStore:
    def __init__(self, connect):
        self.connect = connect

    def _call(self, sql, args):
        with closing(self.connect()) as connection, connection, connection.cursor() as cursor:
            cursor.execute(sql, args)
            return cursor.fetchone()[0]

    def advertise(self, model, ready):
        self._call("SELECT forge_agent_worker_api.advertise(%s, %s)", (model, ready))

    def next_model(self, models):
        return self._call("SELECT forge_agent_worker_api.next_model(%s::text[])", (models,))

    def claim(self, model):
        return self._call("SELECT forge_agent_worker_api.claim(%s)", (model,))

    def renew(self, run_id, attempt_token):
        return self._call("SELECT forge_agent_worker_api.renew(%s::uuid, %s::uuid)", (run_id, attempt_token))

    def publish(self, run_id, attempt_token, outcome):
        return self._call("SELECT forge_agent_worker_api.publish(%s::uuid, %s::uuid, %s)",
                          (run_id, attempt_token, Json(outcome)))

    @contextmanager
    def listen(self):
        with closing(self.connect()) as listener:
            listener.autocommit = True
            with listener.cursor() as cursor:
                cursor.execute("LISTEN forge_agent_jobs")
            yield listener

    def wait(self, listener):
        if select.select([listener], [], [], SCAN_SECONDS)[0]:
            listener.poll()
            listener.notifies.clear()


class ForgeWorker:
    def __init__(self, store, adapter):
        self.store = store
        self.adapter = adapter
        self.model_retry_at = {}

    def model_ready(self):
        try:
            model = self.adapter.ready_model()
        except (ModelError, ValueError, AttributeError):
            return None
        return model if model in MODELS else None

    def run_forever(self):
        while True:
            try:
                with self.store.listen() as listener:
                    self.run_listener(listener)
            except (psycopg2.Error, OSError) as error:
                LOG.warning("Worker reconnecting (%s)", type(error).__name__)
                time.sleep(SCAN_SECONDS)

    def run_listener(self, listener):
        last_model = None
        while True:
            loaded = self.model_ready()
            if last_model and last_model != loaded:
                self.store.advertise(last_model, False)
            last_model = loaded
            now = time.monotonic()
            models = sorted(model for model in MODELS if self.model_retry_at.get(model, 0) <= now)
            model = self.store.next_model(models) if models else None
            if model is not None and model != loaded:
                if loaded:
                    self.store.advertise(loaded, False)
                    last_model = None
                try:
                    self.adapter.load_model(model)
                except (ModelError, ValueError) as error:
                    self.model_retry_at[model] = time.monotonic() + MODEL_RETRY_SECONDS
                    LOG.warning("Could not load %s (%s)", model, getattr(error, "code", "invalid_forge_runtime"))
                    continue
                loaded = last_model = model
            if loaded:
                self.store.advertise(loaded, True)
            if model:
                job = self.store.claim(model)
                if job is not None:
                    self.process(job)
                    continue
            self.store.wait(listener)

    def process(self, job):
        run_id, attempt = job["run_id"], job["attempt_token"]
        stop = threading.Event()
        results = Queue(maxsize=1)
        inference = threading.Thread(target=self.generate, args=(job, stop, results), daemon=True)
        inference.start()
        try:
            while True:
                try:
                    outcome = results.get(timeout=RENEW_SECONDS)
                    break
                except Empty:
                    if not self.store.renew(run_id, attempt):
                        return
                    self.store.advertise(job["model"], True)
            try:
                self.store.publish(run_id, attempt, outcome)
            except (psycopg2.OperationalError, OSError):
                # An ambiguous disconnect can leave a committed receipt.
                self.store.publish(run_id, attempt, outcome)
        finally:
            stop.set()
            if inference.is_alive():
                self.adapter.interrupt()
                inference.join()

    def generate(self, job, stop, results):
        try:
            request = {key: job[key] for key in ("model", "messages", "max_tokens", "temperature")}
            mail_profile = job.get("mail_profile")
            if job["model"] == MAIL_MODEL:
                if not isinstance(mail_profile, dict):
                    raise ModelError(422, "mail_profile_mismatch")
                request["mail_profile"] = mail_profile
            elif mail_profile is not None:
                raise ModelError(422, "mail_profile_mismatch")
            request["stream"] = False
            answer, usage, reason = self.adapter.generate(job["model"], request,
                                                          allow_partial=job["allow_partial"], stop=stop)
            tokens = {name: count for name, count in usage.items()
                      if name in {"prompt_tokens", "completion_tokens", "total_tokens"}
                      and type(count) is int and 0 <= count <= 2**31 - 1}
            result = {"answer": answer, "usage": tokens, "finish_reason": reason}
        except ModelError as error:
            result = {"error": error.code}
        except Exception as error:
            LOG.warning("Invalid local inference result (%s)", type(error).__name__)
            result = {"error": "invalid_forge_reply"}
        results.put(result)


def main():
    logging.basicConfig(level=logging.INFO)
    passfile = Path(os.environ["PGPASSFILE"])
    token_file = Path(os.environ["FORGE_WORKER_MODEL_TOKEN_FILE"])
    if not passfile.is_absolute() or not passfile.is_file() or not token_file.is_absolute():
        raise ValueError("Worker credentials need absolute file paths")
    port = int(os.environ.get("FORGE_WORKER_POSTGRES_PORT", str(POSTGRES_PORT)))
    model_port = int(os.environ.get("FORGE_WORKER_MODEL_PORT", str(MODEL_PORT)))
    if not (1 <= port <= 65535 and 1 <= model_port <= 65535):
        raise ValueError("Invalid worker PostgreSQL or model port")
    token = token_file.read_text().strip()
    if not re.fullmatch(r"[a-fA-F0-9]{32,128}", token):
        raise ValueError("Expected a hexadecimal model bearer credential")

    def connect():
        from proxy_client import connect_from_config, proxy_configured
        if proxy_configured():
            # The unit file sets FORGE_PG_PROXY_URL + TOKEN_FILE; the
            # caller is this worker's own identity, not the env default.
            return connect_from_config(caller="gpu")
        return psycopg2.connect(host="127.0.0.1", hostaddr="127.0.0.1", port=port,
                                dbname="forge_agent", user="forge_gpu_worker", passfile=str(passfile),
                                sslmode="disable", connect_timeout=4, application_name="forge-gpu-worker",
                                options="-c statement_timeout=5000 -c lock_timeout=4000")

    LOG.info("Waiting for PostgreSQL jobs on Forge")
    ForgeWorker(WorkerStore(connect), ModelAdapter(token, model_port)).run_forever()


if __name__ == "__main__":
    main()
