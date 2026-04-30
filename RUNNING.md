# Running Distributed Autobahn Benchmarks

This guide walks through running an Autobahn benchmark on a distributed GCP
cluster: from provisioning VMs to collecting results.

All commands are run from the repo root:
`/home/<user>/autobahn-artifact`.

---

## 1. Prerequisites

On your **local workstation**:

- `gcloud` CLI installed and authenticated (`gcloud auth login`, project set
  via `gcloud config set project <PROJECT>`).
- SSH access to GCP VMs (`gcloud compute ssh` works end-to-end).
- Rust toolchain (`cargo`, stable) — binaries are compiled locally and scp'd to
  the VMs.
- Python 3 with `pyyaml`:

  ```bash
  pip install -r scripts/requirements.txt
  ```

You do **not** need to install Rust on the VMs yourself — `bench.py install`
does that for you.

---

## 2. Create the cluster

Provision VMs and auto-generate a config YAML. Defaults are 4 × `t2d-standard-16`
nodes spread across four US zones:

```bash
python scripts/create_cluster.py
```

Customize as needed:

```bash
python scripts/create_cluster.py \
  --nodes 4 \
  --zones us-west1-c us-east1-c \
  --machine-type t2d-standard-16 \
  --prefix autobahn-node
```

This writes `scripts/configs/gcloud-autobahn.yaml` and waits for SSH to come up
on all VMs.

Tip: `--dry-run` prints the `gcloud` commands without creating anything.

---

## 3. (Optional) Edit the config

Open `scripts/configs/gcloud-autobahn.yaml` and adjust the `bench` and `params`
sections to match your experiment:

```yaml
bench:
  faults: 0          # byzantine/crashed replicas
  rate: 50000        # total tx/s across all clients
  tx_size: 512       # payload size (bytes)
  duration: 20       # benchmark duration (seconds)
  runs: 1            # number of repetitions
workers: 1           # worker processes per authority
```

---

## 4. Install dependencies on the VMs

One-time setup — installs build tools, Rust, and clones the repo on every VM:

```bash
python scripts/bench.py install --config scripts/configs/gcloud-autobahn.yaml
```

---

## 5. Build binaries locally and upload

Compiles the `node` crate with `--release --features benchmark` (producing both
`node` and `benchmark_client` binaries) and scp's them to every VM. Uses an
mtime cache, so re-runs skip VMs that already have the current build.

```bash
python scripts/bench.py upload --config scripts/configs/gcloud-autobahn.yaml
```

Rerun this step any time you change Rust code.

---

## 6. Run the benchmark

This is the main command. It:

1. Builds the binaries (no-op if fresh),
2. Generates fresh node keys and committee/parameters files,
3. Uploads configs to each VM,
4. Starts clients, primaries, and workers,
5. Waits `duration` seconds,
6. Kills all processes,
7. Downloads logs to `scripts/logs/`,
8. Parses the logs and prints throughput + latency.

```bash
python scripts/bench.py remote --config scripts/configs/gcloud-autobahn.yaml
```

CLI overrides (take precedence over the YAML):

```bash
python scripts/bench.py remote \
  --config scripts/configs/gcloud-autobahn.yaml \
  --rate 100000 \
  --duration 30 \
  --tx-size 512 \
  --workers 1 \
  --faults 0 \
  --debug          # -vvv logging on replicas
```

Raw logs land in `scripts/logs/`:

- `primary-<i>.log`
- `worker-<i>-<wid>.log`
- `client-<ci>.log`

---

## 7. (Optional) Throughput/latency sweep

To generate a curve across multiple rates and payload sizes, use `sweep.py`
instead of running `remote` repeatedly:

```bash
python scripts/sweep.py \
  --config scripts/configs/gcloud-autobahn.yaml \
  --rates 10000 50000 100000 150000 \
  --tx-sizes 512 \
  --duration 30
```

Each run is saved under `scripts/logs/sweep_<timestamp>/run_<rate>_<tx_size>/`,
and a combined `sweep_results.json` + summary table is written at the top.

Re-analyze an existing sweep without rerunning experiments:

```bash
python scripts/sweep.py --analyze scripts/logs/sweep_20260418_120000
```

---

## 8. Cleanup

Kill any stray processes on the VMs:

```bash
python scripts/bench.py kill --config scripts/configs/gcloud-autobahn.yaml
```

Stop VMs to pause billing (state preserved, restart with `vm-start`):

```bash
python scripts/bench.py vm-stop   --config scripts/configs/gcloud-autobahn.yaml
python scripts/bench.py vm-start  --config scripts/configs/gcloud-autobahn.yaml
python scripts/bench.py vm-status --config scripts/configs/gcloud-autobahn.yaml
```

To fully tear down the cluster, delete the VMs via `gcloud compute instances
delete <name> --zone=<zone>` for each VM in the config.

---

## Typical end-to-end session

```bash
# One-time
python scripts/create_cluster.py --nodes 4
python scripts/bench.py install --config scripts/configs/gcloud-autobahn.yaml

# Each experiment iteration
python scripts/bench.py upload --config scripts/configs/gcloud-autobahn.yaml
python scripts/bench.py remote --config scripts/configs/gcloud-autobahn.yaml \
  --rate 100000 --duration 30

# When done for the day
python scripts/bench.py vm-stop --config scripts/configs/gcloud-autobahn.yaml
```

---

## Troubleshooting

- **Build fails locally**: run `cargo build --release --features benchmark`
  from the `node/` crate directly to see the full error.
- **SSH times out on a VM**: rerun `python scripts/bench.py vm-status` to
  confirm the VM is `RUNNING`; rerun `install` if the VM was recreated.
- **Clients exit with connection errors**: replicas haven't come up in time —
  increase startup slack by adding `duration`, or check `primary-*.log` for
  bind/committee errors.
- **Log parsing fails but raw logs exist**: run `sweep.py --analyze <dir>` on
  the log directory to re-parse, or inspect the raw logs in `scripts/logs/`.
