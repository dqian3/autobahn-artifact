#!/usr/bin/env python3
"""Autobahn benchmark orchestrator.

Subcommands:
    install     Install dependencies and clone repo on all VMs
    remote      Run a full benchmark on remote VMs
    kill        Kill all autobahn processes on VMs
    vm-start    Start VMs
    vm-stop     Stop VMs
    vm-status   Show VM status

Binary distribution lives in the parent project's run_experiment.py
(`_autobahn_upload_via_vm`): it builds on a cluster VM (matching glibc)
and scp-fans the resulting `node` + `benchmark_client` to every VM.
The local-build `upload` subcommand was removed because its locally
built binary doesn't match the VMs' older glibc on our test cluster.

Usage:
    python scripts/bench.py remote --config scripts/configs/gcloud-autobahn.yaml
    python scripts/bench.py remote --config scripts/configs/gcloud-autobahn.yaml --rate 100000 --duration 30
"""

import argparse
import json
import os
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from math import ceil
from pathlib import Path

# Add parent directory so we can import benchmark.logs for log parsing.
REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "benchmark"))

from autobahn_config import (
    load_config,
    validate_config,
    generate_committee,
    generate_parameters,
    committee_workers_addresses,
    committee_primary_addresses,
    get_all_vms,
    get_num_authorities,
    get_client_assignments,
    get_replica_vms,
    build_host_lists,
    ip_from_address,
)
from remote import load_remote


# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

NODE_CRATE = REPO_ROOT / "node"
BINARY_DIR = REPO_ROOT / "target" / "release"
LOGS_DIR = Path(__file__).resolve().parent / "logs"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def progress_bar(iterable, prefix="", length=30):
    total = len(iterable)
    for i, item in enumerate(iterable):
        pct = 100 * (i + 1) / total
        filled = int(length * (i + 1) // total)
        bar = "=" * filled + "-" * (length - filled)
        print(f"\r{prefix} |{bar}| {pct:.0f}%", end="", flush=True)
        yield item
    print()


def build_binaries():
    """Compile autobahn binaries locally."""
    print("Building binaries...")
    subprocess.run(
        ["cargo", "build", "--quiet", "--release", "--features", "benchmark"],
        check=True,
        cwd=str(NODE_CRATE),
    )
    node_bin = BINARY_DIR / "node"
    client_bin = BINARY_DIR / "benchmark_client"
    if not node_bin.exists() or not client_bin.exists():
        print("ERROR: binaries not found after build", file=sys.stderr)
        sys.exit(1)
    return str(node_bin), str(client_bin)


def generate_keys(node_bin, count):
    """Generate key files locally and return list of key dicts."""
    keys = []
    for i in range(count):
        filename = f".node-{i}.json"
        subprocess.run(
            [node_bin, "generate_keys", "--filename", filename],
            check=True,
        )
        with open(filename, "r") as f:
            keys.append(json.load(f))
    return keys


# ---------------------------------------------------------------------------
# Subcommands
# ---------------------------------------------------------------------------

def cmd_install(args):
    """Install dependencies and clone repo on all VMs."""
    config = load_config(args.config)
    remote = load_remote(config)
    vms = get_all_vms(config)
    repo = config["repo"]

    print(f"Installing on {len(vms)} VMs...")
    remote.check_vms_running(vms)

    install_cmd = " && ".join([
        "sudo apt-get update",
        "sudo apt-get -y upgrade",
        "sudo apt-get -y autoremove",
        "sudo apt-get -y install build-essential cmake clang",
        'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y',
        "source $HOME/.cargo/env",
        "rustup default stable",
        f'(git clone {repo["url"]} || (cd {repo["name"]} && git pull))',
    ])

    failed = []
    for idx, vm in enumerate(vms, start=1):
        print(f"\n===== [{idx}/{len(vms)}] Installing on {vm} =====", flush=True)
        start = time.monotonic()
        try:
            result = remote.ssh(vm, install_cmd)
            elapsed = time.monotonic() - start
            stdout = (result.stdout or "").strip()
            stderr = (result.stderr or "").strip()
            if stdout:
                print(f"--- [{vm}] stdout ---\n{stdout}")
            if stderr:
                print(f"--- [{vm}] stderr ---\n{stderr}")
            print(f"[{vm}] OK ({elapsed:.1f}s)", flush=True)
        except subprocess.CalledProcessError as e:
            elapsed = time.monotonic() - start
            stdout = (e.output or "").strip()
            stderr = (e.stderr or "").strip()
            if stdout:
                print(f"--- [{vm}] stdout ---\n{stdout}")
            if stderr:
                print(f"--- [{vm}] stderr ---\n{stderr}")
            print(f"[{vm}] FAILED exit={e.returncode} ({elapsed:.1f}s)", flush=True)
            failed.append(vm)
        except Exception as e:
            elapsed = time.monotonic() - start
            print(f"[{vm}] ERROR: {e} ({elapsed:.1f}s)", flush=True)
            failed.append(vm)

    if failed:
        print(f"\nERROR: install failed on: {failed}", file=sys.stderr)
        sys.exit(1)
    print(f"\nInstalled on {len(vms)} VMs successfully.")


def run_benchmark(config, remote, log_dir=None, debug=False):
    """Run a single benchmark. Returns the log directory used.

    This is the core logic shared by `cmd_remote` and `sweep.py`.
    Supports both colocate=true (all processes on one VM per authority)
    and colocate=false (primary and workers on separate VMs).

    Args:
        config: loaded config dict (with bench/params already set to desired values)
        remote: a Remote instance (already checked VMs running, IPs resolved)
        log_dir: directory to download logs into (default: scripts/logs/)
        debug: use -vvv logging
    """
    validate_config(config)

    all_vm_names = get_all_vms(config)
    replica_vms = get_replica_vms(config)
    client_assignments = get_client_assignments(config)
    num_nodes = get_num_authorities(config)

    bench = config["bench"]
    faults = bench["faults"]
    workers = config["workers"]
    rate = bench["rate"]
    tx_size = bench["tx_size"]
    duration = bench["duration"]
    good_nodes = num_nodes - faults

    # Build binaries and generate keys locally
    node_bin, _ = build_binaries()
    keys = generate_keys(node_bin, num_nodes)

    # Get internal IPs for all VMs
    ips = remote.get_all_ips(all_vm_names)

    # Build per-authority host lists for committee generation
    host_lists = build_host_lists(config, ips)

    # Build committee and parameters
    committee = generate_committee(keys, host_lists, config["base_port"], workers)
    parameters = generate_parameters(config)

    # Write config files locally
    committee_file = ".committee.json"
    parameters_file = ".parameters.json"
    with open(committee_file, "w") as f:
        json.dump(committee, f, indent=4)
    with open(parameters_file, "w") as f:
        json.dump(parameters, f, indent=4)

    # Upload config files to all VMs.
    # Replica VMs need their own key file; client VMs need the key file for
    # the authority they target.
    print("Uploading config files...")

    upload_targets = []  # (authority_index, vm_name)
    for i, vm in enumerate(replica_vms):
        upload_targets.append((i, vm))
    for c in client_assignments:
        upload_targets.append((c["authority"], c["vm"]))

    def _upload_configs(i, vm):
        key_file = f".node-{i}.json"
        remote.scp_upload(key_file, vm, f"~/{key_file}")
        remote.scp_upload(committee_file, vm, f"~/{committee_file}")
        remote.scp_upload(parameters_file, vm, f"~/{parameters_file}")

    seen = set()
    deduped = []
    for i, vm in upload_targets:
        if vm not in seen:
            seen.add(vm)
            deduped.append((i, vm))

    with ThreadPoolExecutor(max_workers=max(len(deduped), 1)) as pool:
        futures = {pool.submit(_upload_configs, i, vm): vm for i, vm in deduped}
        for f in as_completed(futures):
            f.result()

    # Compute addresses from committee
    workers_addresses = committee_workers_addresses(committee, faults)
    all_tx_addrs = [addr for auth_workers in workers_addresses for _, addr in auth_workers]
    nodes_flag = " ".join(all_tx_addrs)

    # Kill any existing processes
    print("Killing existing processes...")
    remote.run_on_all(all_vm_names, "pkill -9 -f './node' || true; pkill -9 -f './benchmark_client' || true", quiet=True)
    time.sleep(2)

    # Clean up old state on VMs
    remote.run_on_all(all_vm_names, "rm -rf .db-* logs/ ; mkdir -p logs/", quiet=True)

    # Start clients first (they wait for nodes to come online).
    # Rate is split evenly across all client processes.
    print(f"Starting {len(client_assignments)} client(s)...")
    num_clients = len(client_assignments)
    rate_per_client = ceil(rate / num_clients) if num_clients > 0 else 0
    # Replicas pick up `disable_crypto` from the parameters file. The
    # benchmark client doesn't read parameters, so pass it via CLI so
    # client-side tx signing is skipped too.
    client_disable_crypto = " --disable-crypto" if parameters.get("disable_crypto") else ""

    for ci, c in enumerate(client_assignments):
        auth_idx = c["authority"]
        wid = c["worker"]
        client_vm = c["vm"]
        key_file = f".node-{auth_idx}.json"

        # Look up the transaction address for this authority's worker
        tx_addr = workers_addresses[auth_idx][wid][1]

        cmd = (
            f"nohup ./benchmark_client {tx_addr} "
            f"--size {tx_size} --rate {rate_per_client} "
            f"--key {key_file} --nodes {nodes_flag}{client_disable_crypto} "
            f">logs/client-{ci}.log 2>&1 &"
        )
        remote.ssh(client_vm, cmd, bg=True)

    # Start primaries (on replica VMs)
    print("Starting primaries...")
    debug_flag = "-vvv" if debug else "-vv"
    for i in range(good_nodes):
        vm = replica_vms[i]
        key_file = f".node-{i}.json"
        cmd = (
            f"nohup ./node {debug_flag} run "
            f"--keys {key_file} --committee {committee_file} "
            f"--store .db-{i} --parameters {parameters_file} primary "
            f">logs/primary-{i}.log 2>&1 &"
        )
        remote.ssh(vm, cmd, bg=True)

    # Start workers (on replica VMs, co-located with primary)
    print("Starting workers...")
    for i in range(good_nodes):
        vm = replica_vms[i]
        key_file = f".node-{i}.json"
        for wid, _ in workers_addresses[i]:
            cmd = (
                f"nohup ./node {debug_flag} run "
                f"--keys {key_file} --committee {committee_file} "
                f"--store .db-{i}-{wid} --parameters {parameters_file} "
                f"worker --id {wid} "
                f">logs/worker-{i}-{wid}.log 2>&1 &"
            )
            remote.ssh(vm, cmd, bg=True)

    # Wait for benchmark duration
    print(f"Running benchmark ({duration}s)...")
    steps = 20
    step_duration = ceil(duration / steps)
    for step in progress_bar(range(steps), prefix="Progress:"):
        time.sleep(step_duration)

    # Kill all processes
    print("Stopping processes...")
    remote.run_on_all(all_vm_names, "pkill -9 -f './node' || true; pkill -9 -f './benchmark_client' || true", quiet=True)
    time.sleep(2)

    # Download logs
    dest = Path(log_dir) if log_dir else LOGS_DIR
    dest.mkdir(parents=True, exist_ok=True)
    for f in dest.glob("*.log"):
        f.unlink()

    print("Downloading logs...")

    def _download_log(vm, remote_path, local_path):
        try:
            remote.scp_download(vm, remote_path, str(local_path))
        except Exception as e:
            print(f"  Warning: could not download {remote_path} from {vm}: {e}")

    download_tasks = []
    # Primary + worker logs from replica VMs
    for i in range(good_nodes):
        vm = replica_vms[i]
        download_tasks.append((vm, f"logs/primary-{i}.log", dest / f"primary-{i}.log"))
        for wid, _ in workers_addresses[i]:
            download_tasks.append((vm, f"logs/worker-{i}-{wid}.log", dest / f"worker-{i}-{wid}.log"))
    # Client logs from client VMs
    for ci, c in enumerate(client_assignments):
        download_tasks.append((c["vm"], f"logs/client-{ci}.log", dest / f"client-{ci}.log"))

    with ThreadPoolExecutor(max_workers=8) as pool:
        futures = [pool.submit(_download_log, *t) for t in download_tasks]
        for f in as_completed(futures):
            f.result()

    return str(dest)


def cmd_remote(args):
    """Run a full benchmark on remote VMs."""
    config = load_config(args.config)

    # CLI overrides
    if args.rate is not None:
        config["bench"]["rate"] = args.rate
    if args.duration is not None:
        config["bench"]["duration"] = args.duration
    if args.tx_size is not None:
        config["bench"]["tx_size"] = args.tx_size
    if args.workers is not None:
        config["workers"] = args.workers
    if args.faults is not None:
        config["bench"]["faults"] = args.faults

    validate_config(config)
    remote = load_remote(config)
    all_vm_names = get_all_vms(config)
    num_nodes = get_num_authorities(config)
    bench = config["bench"]
    faults = bench["faults"]
    runs = bench.get("runs", 1)
    colocate = config.get("colocate", True)

    print(f"=== Autobahn Benchmark ===")
    print(f"Nodes: {num_nodes} ({faults} faulty), Workers/node: {config['workers']}, Colocate: {colocate}")
    print(f"Rate: {bench['rate']:,} tx/s, Tx size: {bench['tx_size']} B, Duration: {bench['duration']}s")
    print()

    remote.check_vms_running(all_vm_names)

    for run_idx in range(runs):
        if runs > 1:
            print(f"\n--- Run {run_idx + 1}/{runs} ---")

        log_dir = run_benchmark(config, remote, debug=getattr(args, "debug", False))

        # Parse and print results
        print("\nParsing logs...")
        try:
            from benchmark.logs import LogParser
            logger = LogParser.process(log_dir, faults=faults)
            print(logger.result())
        except Exception as e:
            print(f"Log parsing failed: {e}")
            print(f"Raw logs are available in {log_dir}/")


def cmd_kill(args):
    """Kill all autobahn processes on VMs."""
    config = load_config(args.config)
    remote = load_remote(config)
    vms = get_all_vms(config)
    print(f"Killing processes on {len(vms)} VMs...")
    remote.run_on_all(vms, "pkill -9 -f './node' || true; pkill -9 -f './benchmark_client' || true", quiet=True)
    print("Done.")


def cmd_vm_start(args):
    config = load_config(args.config)
    remote = load_remote(config)
    vms = get_all_vms(config)
    print(f"Starting {len(vms)} VMs...")
    remote.vm_start(vms)
    print("Done.")


def cmd_vm_stop(args):
    config = load_config(args.config)
    remote = load_remote(config)
    vms = get_all_vms(config)
    print(f"Stopping {len(vms)} VMs...")
    remote.vm_stop(vms)
    print("Done.")


def cmd_vm_status(args):
    config = load_config(args.config)
    remote = load_remote(config)
    vms = get_all_vms(config)
    statuses = remote.vm_status(vms)
    print(f"{'VM':<25} {'Status'}")
    print(f"{'--':<25} {'------'}")
    for vm in vms:
        status = statuses.get(vm, "NOT_FOUND")
        print(f"{vm:<25} {status}")


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="Autobahn benchmark orchestrator")
    sub = parser.add_subparsers(dest="command", required=True)

    # install
    p = sub.add_parser("install", help="Install deps and clone repo on VMs")
    p.add_argument("--config", required=True, help="YAML config file")

    # remote
    p = sub.add_parser("remote", help="Run benchmark on remote VMs")
    p.add_argument("--config", required=True, help="YAML config file")
    p.add_argument("--rate", type=int, default=None, help="Override total tx rate")
    p.add_argument("--duration", type=int, default=None, help="Override duration (seconds)")
    p.add_argument("--tx-size", type=int, default=None, help="Override transaction size")
    p.add_argument("--workers", type=int, default=None, help="Override workers per node")
    p.add_argument("--faults", type=int, default=None, help="Override fault count")
    p.add_argument("--debug", action="store_true", help="Use verbose logging (-vvv)")

    # kill
    p = sub.add_parser("kill", help="Kill all processes on VMs")
    p.add_argument("--config", required=True, help="YAML config file")

    # vm-start
    p = sub.add_parser("vm-start", help="Start VMs")
    p.add_argument("--config", required=True, help="YAML config file")

    # vm-stop
    p = sub.add_parser("vm-stop", help="Stop VMs")
    p.add_argument("--config", required=True, help="YAML config file")

    # vm-status
    p = sub.add_parser("vm-status", help="Show VM status")
    p.add_argument("--config", required=True, help="YAML config file")

    args = parser.parse_args()

    commands = {
        "install": cmd_install,
        "remote": cmd_remote,
        "kill": cmd_kill,
        "vm-start": cmd_vm_start,
        "vm-stop": cmd_vm_stop,
        "vm-status": cmd_vm_status,
    }
    commands[args.command](args)


if __name__ == "__main__":
    main()
