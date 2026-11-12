# Forge command benchmark

Mission Control sends a fresh chat to Forge for every fixture and model. The benchmark
uses the existing loopback adapter on port 18082 and restores its original model or
stopped state when the run finishes. Model switches briefly interrupt Forge IDE chat.

From the Homelab checkout on Mission Control, run:

```bash
sudo deploy/mission-control/agent-driver/setup-user.sh
sudo python3 -B deploy/mission-control/agent-driver/benchmark.py --check
sudo python3 -B deploy/mission-control/agent-driver/benchmark.py
```

Python 3 and PyYAML must be installed. The benchmark reads the existing adapter
credential as root. Each fixture command runs under the dedicated non-login
`forge-agent-driver` account without supplementary groups. The benchmark translates
the supported command grammar into fixed executable arguments, runs in a read-only
synthetic fixture directory, and passes no credentials to the subprocess. The grammar
allows a few text utilities and a single pipe to `uniq`. This harness scores fixture
commands. Arbitrary agent execution requires a separate sandbox.

The default comparison uses GPT-OSS 20B and Qwen2.5 Coder 7B. Repeat `--model` to
choose other models from the live adapter's allowlist. Each case passes when the
command exits successfully and stdout matches the expected bytes. A malformed reply,
policy rejection, command failure, output mismatch, or adapter error counts as a
failed case and is recorded separately. A run returns exit code 1 if any case failed.
Runs write per-case `results.jsonl` and model totals in `summary.json` under
`/mnt/data/forge-homelab/agent-driver/runs`.
