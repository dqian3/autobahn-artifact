#!/usr/bin/env python3
"""Autobahn benchmark orchestrator.

Subcommands:
    install     Install dependencies and clone repo on all VMs
    upload      Build locally and upload binaries to VMs
    remote      Run a full benchmark on remote VMs
    kill        Kill all autobahn processes on VMs
    vm-start    Start VMs
    vm-stop     Stop VMs
    vm-status   Show VM status

Usage:
    python scripts/bench.py upload --config scripts/configs/gcloud-autobahn.yaml
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
    generate_committee,
    generate_parameters,
    committee_workers_addresses,
    committee_primary_addresses,
    ip_from_address,
)
from remote import load_remote


# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------

NODE_CRATE = REPO_ROOT / "node"
BINARY_DIR = REPO_ROOT / "target" / "release"
MTIME_FILE = Path(__file__).resolve().parent / ".upload_mtimes"
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


def load_mtimes():
    if MTIME_FILE.exists():
        with open(MTIME_FILE) as f:
            return json.load(f)
    return {}


def save_mtimes(mtimes):
    with open(MTIME_FILE, "w") as f:
        json.dump(mtimes, f)


# ---------------------------------------------------------------------------
# Subcommands
# ---------------------------------------------------------------------------

def cmd_install(args):
    """Install dependencies and clone repo on all VMs."""
    config = load_config(args.config)
    remote = load_remote(config)
    vms = config["vms"]
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

    results = remote.run_on_all(vms, install_cmd)
    failed = [vm for vm, r in results.items() if isinstance(r, Exception)]
    if failed:
        print(f"ERROR: install failed on: {failed}", file=sys.stderr)
        sys.exit(1)
    print(f"Installed on {len(vms)} VMs successfully.")


def cmd_upload(args):
    """Build locally and upload binaries to all VMs."""
    config = load_config(args.config)
    remote = load_remote(config)
    vms = config["vms"]

    remote.check_vms_running(vms)

    node_bin, client_bin = build_binaries()
    binaries = {"node": node_bin, "benchmark_client": client_bin}

    mtimes = load_mtimes()
    uploads = []
    for name, path in binaries.items():
        mtime = os.path.getmtime(path)
        for vm in vms:
            cache_key = f"{vm}:{name}"
            if mtimes.get(cache_key) == mtime:
                continue
            uploads.append((vm, name, path, mtime, cache_key))

    if not uploads:
        print("All binaries up to date on all VMs.")
        return

    print(f"Uploading {len(uploads)} binary/VM pairs...")

    def _upload_one(vm, name, path, mtime, cache_key):
        remote.scp_upload(path, vm, f"~/{name}")
        remote.ssh(vm, f"chmod +x ~/{name}")
        return cache_key, mtime

    with ThreadPoolExecutor(max_workers=8) as pool:
        futures = {
            pool.submit(_upload_one, *u): u[0] for u in uploads
        }
        for f in as_completed(futures):
            vm = futures[f]
            try:
                cache_key, mtime = f.result()
                mtimes[cache_key] = mtime
                print(f"  {cache_key}: uploaded")
            except Exception as e:
                print(f"  [{vm}] upload failed: {e}")

    save_mtimes(mtimes)
    print("Upload complete.")


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

    remote = load_remote(config)
    vms = config["vms"]
    bench = config["bench"]
    faults = bench["faults"]
    workers = config["workers"]
    rate = bench["rate"]
    tx_size = bench["tx_size"]
    duration = bench["duration"]
    runs = bench.get("runs", 1)

    num_nodes = len(vms)
    good_nodes = num_nodes - faults

    print(f"=== Autobahn Benchmark ===")
    print(f"Nodes: {num_nodes} ({faults} faulty), Workers/node: {workers}")
    print(f"Rate: {rate:,} tx/s, Tx size: {tx_size} B, Duration: {duration}s")
    print()

    remote.check_vms_running(vms)

    # Get internal IPs
    print("Resolving VM IPs...")
    ips = remote.get_all_ips(vms)
    # Maintain order matching vms list
    ip_list = [ips[vm] for vm in vms]

    # Build binaries and generate keys locally
    node_bin, _ = build_binaries()
    print(f"Generating keys for {num_nodes} nodes...")
    keys = generate_keys(node_bin, num_nodes)

    # Build committee and parameters
    committee = generate_committee(keys, ip_list, config["base_port"], workers)
    parameters = generate_parameters(config)

    # Write config files locally
    committee_file = ".committee.json"
    parameters_file = ".parameters.json"
    with open(committee_file, "w") as f:
        json.dump(committee, f, indent=4)
    with open(parameters_file, "w") as f:
        json.dump(parameters, f, indent=4)

    # Upload config files to each VM
    print("Uploading config files...")
    key_names = list(committee["authorities"].keys())

    def _upload_configs(i, vm):
        key_file = f".node-{i}.json"
        remote.scp_upload(key_file, vm, f"~/{key_file}")
        remote.scp_upload(committee_file, vm, f"~/{committee_file}")
        remote.scp_upload(parameters_file, vm, f"~/{parameters_file}")

    with ThreadPoolExecutor(max_workers=len(vms)) as pool:
        futures = {pool.submit(_upload_configs, i, vm): vm for i, vm in enumerate(vms)}
        for f in as_completed(futures):
            f.result()

    # Run benchmarks
    workers_addresses = committee_workers_addresses(committee, faults)
    primary_addresses = committee_primary_addresses(committee, faults)

    # Flatten all worker transaction addresses (for client --nodes flag)
    all_tx_addrs = [addr for auth_workers in workers_addresses for _, addr in auth_workers]

    for run_idx in range(runs):
        if runs > 1:
            print(f"\n--- Run {run_idx + 1}/{runs} ---")

        # Kill any existing processes
        print("Killing existing processes...")
        remote.run_on_all(vms, "pkill -9 -f './node' || true; pkill -9 -f './benchmark_client' || true", quiet=True)
        time.sleep(2)

        # Clean up old state on VMs
        remote.run_on_all(vms, "rm -rf .db-* logs/ ; mkdir -p logs/", quiet=True)

        # Start clients first (they wait for nodes to come online)
        print("Starting clients...")
        total_workers = good_nodes * workers
        rate_share = ceil(rate / total_workers)
        nodes_flag = " ".join(all_tx_addrs)

        for i, auth_workers in enumerate(workers_addresses):
            vm = vms[i]
            key_file = f".node-{i}.json"
            for wid, tx_addr in auth_workers:
                cmd = (
                    f"nohup ./benchmark_client {tx_addr} "
                    f"--size {tx_size} --rate {rate_share} "
                    f"--keys {key_file} --nodes {nodes_flag} "
                    f">logs/client-{i}-{wid}.log 2>&1 &"
                )
                remote.ssh(vm, cmd, bg=True)

        # Start primaries
        print("Starting primaries...")
        for i, addr in enumerate(primary_addresses):
            vm = vms[i]
            key_file = f".node-{i}.json"
            debug_flag = "-vvv" if args.debug else "-vv"
            cmd = (
                f"nohup ./node {debug_flag} run "
                f"--keys {key_file} --committee {committee_file} "
                f"--store .db-{i} --parameters {parameters_file} primary "
                f">logs/primary-{i}.log 2>&1 &"
            )
            remote.ssh(vm, cmd, bg=True)

        # Start workers
        print("Starting workers...")
        for i, auth_workers in enumerate(workers_addresses):
            vm = vms[i]
            key_file = f".node-{i}.json"
            debug_flag = "-vvv" if args.debug else "-vv"
            for wid, _ in auth_workers:
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
        remote.run_on_all(vms, "pkill -9 -f './node' || true; pkill -9 -f './benchmark_client' || true", quiet=True)
        time.sleep(2)

        # Download logs
        print("Downloading logs...")
        LOGS_DIR.mkdir(parents=True, exist_ok=True)
        # Clear local logs
        for f in LOGS_DIR.glob("*.log"):
            f.unlink()

        def _download_log(vm, remote_path, local_path):
            try:
                remote.scp_download(vm, remote_path, str(local_path))
            except Exception as e:
                print(f"  Warning: could not download {remote_path} from {vm}: {e}")

        download_tasks = []
        for i in range(good_nodes):
            vm = vms[i]
            download_tasks.append((vm, f"logs/primary-{i}.log", LOGS_DIR / f"primary-{i}.log"))
            for wid in range(workers):
                download_tasks.append((vm, f"logs/worker-{i}-{wid}.log", LOGS_DIR / f"worker-{i}-{wid}.log"))
                download_tasks.append((vm, f"logs/client-{i}-{wid}.log", LOGS_DIR / f"client-{i}-{wid}.log"))

        with ThreadPoolExecutor(max_workers=8) as pool:
            futures = [pool.submit(_download_log, *t) for t in download_tasks]
            for f in as_completed(futures):
                f.result()

        # Parse and print results
        print("\nParsing logs...")
        try:
            from benchmark.logs import LogParser
            logger = LogParser.process(str(LOGS_DIR), faults=faults)
            print(logger.result())
        except Exception as e:
            print(f"Log parsing failed: {e}")
            print("Raw logs are available in scripts/logs/")


def cmd_kill(args):
    """Kill all autobahn processes on VMs."""
    config = load_config(args.config)
    remote = load_remote(config)
    vms = config["vms"]
    print(f"Killing processes on {len(vms)} VMs...")
    remote.run_on_all(vms, "pkill -9 -f './node' || true; pkill -9 -f './benchmark_client' || true", quiet=True)
    print("Done.")


def cmd_vm_start(args):
    config = load_config(args.config)
    remote = load_remote(config)
    vms = config["vms"]
    print(f"Starting {len(vms)} VMs...")
    remote.vm_start(vms)
    print("Done.")


def cmd_vm_stop(args):
    config = load_config(args.config)
    remote = load_remote(config)
    vms = config["vms"]
    print(f"Stopping {len(vms)} VMs...")
    remote.vm_stop(vms)
    print("Done.")


def cmd_vm_status(args):
    config = load_config(args.config)
    remote = load_remote(config)
    vms = config["vms"]
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

    # upload
    p = sub.add_parser("upload", help="Build and upload binaries to VMs")
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
        "upload": cmd_upload,
        "remote": cmd_remote,
        "kill": cmd_kill,
        "vm-start": cmd_vm_start,
        "vm-stop": cmd_vm_stop,
        "vm-status": cmd_vm_status,
    }
    commands[args.command](args)


if __name__ == "__main__":
    main()
