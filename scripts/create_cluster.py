#!/usr/bin/env python3
"""Create GCP VMs for an Autobahn benchmark cluster.

Creates node VMs spread across specified zones (co-locate mode:
primary + workers + clients all on the same VM per authority).

Usage:
    python create_cluster.py
    python create_cluster.py --nodes 4 --zones us-west1-c us-east1-c
    python create_cluster.py --dry-run
"""

import argparse
import os
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

import yaml


DEFAULT_NODES = 4
DEFAULT_ZONES = ["us-west1-c", "us-west4-c", "us-east1-c", "us-east4-c"]
DEFAULT_MACHINE_TYPE = "t2d-standard-16"
DEFAULT_IMAGE_FAMILY = "ubuntu-2204-lts"
DEFAULT_IMAGE_PROJECT = "ubuntu-os-cloud"


def create_vm(name, machine_type, zone, image_family, image_project, dry_run=False):
    """Create a single GCE VM."""
    cmd = [
        "gcloud", "compute", "instances", "create", name,
        f"--zone={zone}",
        f"--machine-type={machine_type}",
        f"--image-family={image_family}",
        f"--image-project={image_project}",
    ]

    if dry_run:
        print(f"  [dry-run] {' '.join(cmd)}")
        return name

    print(f"  Creating {name} ({machine_type} in {zone})...")
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or "(no output)"
        print(f"  ERROR creating {name}: {detail}", file=sys.stderr)
        raise RuntimeError(f"Failed to create {name}")

    print(f"  Created {name}")
    return name


def wait_for_ssh(vm_name, zone, timeout=120, interval=5):
    """Wait until SSH is available on a gcloud VM."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = subprocess.run(
            ["gcloud", "compute", "ssh", vm_name,
             f"--zone={zone}",
             "--command", "exit 0",
             "--ssh-flag=-o ConnectTimeout=5",
             "--ssh-flag=-o StrictHostKeyChecking=no"],
            capture_output=True, text=True,
        )
        if result.returncode == 0:
            return True
        time.sleep(interval)
    return False


def distribute_zones(count, zones):
    """Assign zones to VMs round-robin."""
    return [zones[i % len(zones)] for i in range(count)]


def main():
    ap = argparse.ArgumentParser(description="Create Autobahn benchmark cluster on GCP.")

    ap.add_argument("--nodes", type=int, default=DEFAULT_NODES,
                    help=f"Number of node VMs (default: {DEFAULT_NODES})")
    ap.add_argument("--zones", nargs="+", default=DEFAULT_ZONES,
                    help="Zones for nodes (round-robin if fewer than --nodes)")
    ap.add_argument("--machine-type", default=DEFAULT_MACHINE_TYPE,
                    help=f"Machine type (default: {DEFAULT_MACHINE_TYPE})")
    ap.add_argument("--prefix", default="autobahn-node",
                    help="VM name prefix (default: autobahn-node)")
    ap.add_argument("--image-family", default=DEFAULT_IMAGE_FAMILY,
                    help=f"VM image family (default: {DEFAULT_IMAGE_FAMILY})")
    ap.add_argument("--image-project", default=DEFAULT_IMAGE_PROJECT,
                    help=f"Image project (default: {DEFAULT_IMAGE_PROJECT})")
    ap.add_argument("--output", "-o", default=None,
                    help="Output YAML config path (default: configs/gcloud-autobahn.yaml)")
    ap.add_argument("--dry-run", action="store_true",
                    help="Print gcloud commands without executing")
    ap.add_argument("--no-wait-ssh", action="store_true",
                    help="Skip waiting for SSH availability")

    args = ap.parse_args()

    zone_list = distribute_zones(args.nodes, args.zones)

    all_vms = []
    vm_names = []

    for i in range(args.nodes):
        name = f"{args.prefix}{i}"
        zone = zone_list[i]
        all_vms.append((name, args.machine_type, zone))
        vm_names.append(name)

    # Print plan
    print("=== Cluster plan ===")
    print(f"{'Instance':<25} {'Zone':<20} {'Machine Type'}")
    print(f"{'--------':<25} {'----':<20} {'------------'}")
    for name, mt, zone in all_vms:
        print(f"{name:<25} {zone:<20} {mt}")
    print(f"\nTotal: {args.nodes} VMs")

    if args.dry_run:
        print("\n=== Dry run: gcloud commands ===")
        for name, mt, zone in all_vms:
            create_vm(name, mt, zone=zone,
                      image_family=args.image_family, image_project=args.image_project,
                      dry_run=True)
    else:
        # Check which VMs already exist
        print("\n=== Checking for existing VMs ===")
        existing = set()
        filter_expr = " OR ".join(f"name={n}" for n in vm_names)
        result = subprocess.run(
            ["gcloud", "compute", "instances", "list",
             f"--filter={filter_expr}",
             "--format=value(name)"],
            capture_output=True, text=True,
        )
        if result.returncode == 0:
            existing = set(result.stdout.strip().splitlines())

        to_create = [(n, mt, z) for n, mt, z in all_vms if n not in existing]
        if existing:
            print(f"  Already exist ({len(existing)}): {', '.join(sorted(existing))}")
        if not to_create:
            print("  All VMs already exist, nothing to create.")
        else:
            print(f"  Will create {len(to_create)} VMs")

            print("\n=== Creating VMs ===")
            failed = []
            with ThreadPoolExecutor(max_workers=10) as pool:
                futures = {}
                for name, mt, zone in to_create:
                    f = pool.submit(create_vm, name, mt, zone=zone,
                                    image_family=args.image_family,
                                    image_project=args.image_project)
                    futures[f] = name

                for f in as_completed(futures):
                    name = futures[f]
                    try:
                        f.result()
                    except Exception as e:
                        failed.append((name, str(e)))

            if failed:
                print(f"\nERROR: {len(failed)} VMs failed to create:")
                for name, err in failed:
                    print(f"  {name}: {err}")
                sys.exit(1)

        # Wait for SSH
        if not args.no_wait_ssh:
            print("\n=== Waiting for SSH on all VMs (up to 2 min each) ===")
            vm_zones = {name: zone for name, _, zone in all_vms}
            with ThreadPoolExecutor(max_workers=len(all_vms)) as pool:
                futures = {
                    pool.submit(wait_for_ssh, name, vm_zones[name]): name
                    for name in vm_names
                }
                for f in as_completed(futures):
                    name = futures[f]
                    if not f.result():
                        print(f"  WARNING: SSH not available on {name} after timeout")
                    else:
                        print(f"  {name}: SSH ready")

    # Generate config YAML
    config = {
        "platform": "gcloud",
        "vms": vm_names,
        "repo": {
            "url": "https://github.com/neilgiri/autobahn-artifact.git",
            "name": "autobahn-artifact",
            "branch": "autobahn-blips",
        },
        "base_port": 5000,
        "workers": 1,
        "bench": {
            "faults": 0,
            "rate": 50_000,
            "tx_size": 512,
            "duration": 20,
            "runs": 1,
        },
        "params": {
            "timeout_delay": 1000,
            "header_size": 1000,
            "max_header_delay": 100,
            "gc_depth": 50,
            "sync_retry_delay": 5000,
            "sync_retry_nodes": 3,
            "batch_size": 500_000,
            "max_batch_delay": 100,
            "k": 4,
            "use_fast_path": True,
            "fast_path_timeout": 500,
        },
    }

    output_path = args.output or os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "configs", "gcloud-autobahn.yaml"
    )
    os.makedirs(os.path.dirname(output_path), exist_ok=True)

    with open(output_path, "w") as f:
        yaml.dump(config, f, default_flow_style=False, sort_keys=False)

    print(f"\n=== Config written to {output_path} ===")
    print(f"Edit bench parameters in the config, then run:")
    print(f"  python scripts/bench.py install --config {output_path}")
    print(f"  python scripts/bench.py upload --config {output_path}")
    print(f"  python scripts/bench.py remote --config {output_path}")


if __name__ == "__main__":
    main()
