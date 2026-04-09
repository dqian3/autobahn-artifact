#!/usr/bin/env python3
"""Sweep over send rates (and optionally payload sizes) to generate
throughput/latency curves for Autobahn benchmarks.

Runs the benchmark at each (rate, tx_size) combination, downloads logs,
parses results, and prints a summary table.

Usage:
    python scripts/sweep.py --config scripts/configs/gcloud-autobahn.yaml --rates 10000 50000 100000
    python scripts/sweep.py --config scripts/configs/gcloud-autobahn.yaml --rates 50000 --tx-sizes 128 512 1024
    python scripts/sweep.py --config scripts/configs/gcloud-autobahn.yaml --rates 50000 100000 --duration 30
    python scripts/sweep.py --analyze logs/sweep_20260407_120000
"""

import argparse
import copy
import json
import os
import shutil
import sys
import time
from datetime import datetime
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parent
sys.path.insert(0, str(REPO_ROOT / "benchmark"))
sys.path.insert(0, str(SCRIPT_DIR))

from autobahn_config import load_config
from remote import load_remote
from bench import run_benchmark, cmd_upload, build_binaries


def parse_logs(log_dir, faults=0):
    """Parse logs and return a result dict, or None on failure."""
    try:
        from benchmark.logs import LogParser
        logger = LogParser.process(log_dir, faults=faults)
        return logger
    except Exception as e:
        print(f"  Log parsing failed: {e}")
        return None


def result_to_entry(logger, rate, tx_size):
    """Convert a LogParser result to a summary dict."""
    if logger is None:
        return {
            "rate": rate,
            "tx_size": tx_size,
            "error": True,
        }

    consensus_tps, consensus_bps, duration = logger._consensus_throughput()
    e2e_tps, e2e_bps, _ = logger._end_to_end_throughput()
    consensus_lat = logger._consensus_latency() * 1000
    e2e_lat = logger._end_to_end_latency() * 1000

    return {
        "rate": rate,
        "tx_size": tx_size,
        "input_rate": sum(logger.rate),
        "consensus_tps": round(consensus_tps),
        "consensus_bps": round(consensus_bps),
        "consensus_latency_ms": round(consensus_lat),
        "e2e_tps": round(e2e_tps),
        "e2e_bps": round(e2e_bps),
        "e2e_latency_ms": round(e2e_lat),
        "duration_s": round(duration) if duration else 0,
        "committee_size": logger.committee_size,
        "workers": logger.workers,
        "misses": logger.misses,
    }


def print_summary(all_results, sweep_dir):
    """Print the sweep summary table."""
    print(f"\n{'=' * 110}")
    print(f"  SWEEP SUMMARY")
    print(f"{'=' * 110}")

    header = (
        f"{'Rate':>10} {'TxSize':>8} {'InRate':>10} "
        f"{'ConsTPS':>10} {'ConsBPS':>12} {'ConsLat':>10} "
        f"{'E2E TPS':>10} {'E2E BPS':>12} {'E2E Lat':>10} "
        f"{'Dur':>5} {'Miss':>5}"
    )
    print(header)
    print("-" * len(header))

    for r in all_results:
        if r.get("error"):
            print(f"{r['rate']:>10} {r['tx_size']:>8}   ** FAILED **")
            continue
        print(
            f"{r['rate']:>10} "
            f"{r['tx_size']:>8} "
            f"{r['input_rate']:>10,} "
            f"{r['consensus_tps']:>10,} "
            f"{r['consensus_bps']:>12,} "
            f"{r['consensus_latency_ms']:>8} ms "
            f"{r['e2e_tps']:>10,} "
            f"{r['e2e_bps']:>12,} "
            f"{r['e2e_latency_ms']:>8} ms "
            f"{r['duration_s']:>5} "
            f"{r['misses']:>5}"
        )

    print(f"{'=' * 110}")
    print(f"\nFull results: {sweep_dir}/sweep_results.json")


def analyze(sweep_dir):
    """Re-parse logs from an existing sweep directory and print summary."""
    results_file = os.path.join(sweep_dir, "sweep_results.json")
    if os.path.exists(results_file):
        with open(results_file) as f:
            all_results = json.load(f)
        print_summary(all_results, sweep_dir)
        return

    # Try to re-parse from logs
    import glob as globmod
    run_dirs = sorted(globmod.glob(os.path.join(sweep_dir, "run_*")))
    if not run_dirs:
        print(f"No run_* directories or sweep_results.json found in {sweep_dir}")
        sys.exit(1)

    all_results = []
    for run_dir in run_dirs:
        # Parse rate and tx_size from dir name: run_<rate>_<tx_size>
        basename = os.path.basename(run_dir)
        parts = basename.replace("run_", "").split("_")
        rate = int(parts[0])
        tx_size = int(parts[1]) if len(parts) > 1 else 512

        logger = parse_logs(run_dir, faults=0)
        all_results.append(result_to_entry(logger, rate, tx_size))

    print_summary(all_results, sweep_dir)


def main():
    ap = argparse.ArgumentParser(description="Sweep send rates/payload sizes for Autobahn benchmark.")
    ap.add_argument("--analyze", default=None, metavar="SWEEP_DIR",
                    help="Re-analyze an existing sweep directory instead of running experiments")
    ap.add_argument("--config", default=None, help="YAML config file")
    ap.add_argument("--rates", type=int, nargs="+", default=None,
                    help="List of total tx rates to sweep")
    ap.add_argument("--tx-sizes", type=int, nargs="+", default=None, dest="tx_sizes",
                    help="List of payload sizes (bytes) to sweep (default: from config)")
    ap.add_argument("--duration", type=int, default=None,
                    help="Override benchmark duration (seconds)")
    ap.add_argument("--faults", type=int, default=None,
                    help="Override fault count")
    ap.add_argument("--upload", action="store_true",
                    help="Build and upload binaries before sweeping")
    ap.add_argument("--output-dir", default=None, dest="output_dir",
                    help="Base directory for sweep output (default: scripts/logs/sweep_<timestamp>)")
    ap.add_argument("--debug", action="store_true",
                    help="Use verbose logging (-vvv)")
    ap.add_argument("--pause", type=int, default=10,
                    help="Seconds to pause between runs (default: 10)")
    args = ap.parse_args()

    if args.analyze:
        analyze(args.analyze)
        return

    if not args.config:
        ap.error("--config is required when not using --analyze")
    if not args.rates:
        ap.error("--rates is required when not using --analyze")

    config = load_config(args.config)
    remote = load_remote(config)
    vms = config["vms"]

    # Check VMs and pre-discover zones
    print("=== Checking VM status ===")
    remote.check_vms_running(vms)

    # Upload if requested
    if args.upload:
        print("\n=== Building and uploading binaries ===")
        upload_args = argparse.Namespace(config=args.config)
        cmd_upload(upload_args)

    # Build sweep matrix
    tx_sizes = args.tx_sizes or [config["bench"]["tx_size"]]

    sweep_points = []
    for tx_size in tx_sizes:
        for rate in args.rates:
            sweep_points.append((rate, tx_size))

    # Setup output directory
    timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
    sweep_dir = args.output_dir or str(SCRIPT_DIR / "logs" / f"sweep_{timestamp}")
    os.makedirs(sweep_dir, exist_ok=True)

    # Copy config for reproducibility
    shutil.copy2(args.config, os.path.join(sweep_dir, os.path.basename(args.config)))

    duration = args.duration or config["bench"]["duration"]
    faults = args.faults if args.faults is not None else config["bench"]["faults"]

    print(f"\n=== Sweep: {len(sweep_points)} run(s), duration={duration}s ===")
    for rate, tx_size in sweep_points:
        print(f"  rate={rate:,} tx/s, tx_size={tx_size} B")
    print(f"Output: {sweep_dir}\n")

    all_results = []

    for idx, (rate, tx_size) in enumerate(sweep_points):
        print(f"\n{'#' * 60}")
        print(f"  Run {idx + 1}/{len(sweep_points)}: rate={rate:,} tx/s, tx_size={tx_size} B")
        print(f"{'#' * 60}")

        # Deep copy config and apply overrides for this run
        run_config = copy.deepcopy(config)
        run_config["bench"]["rate"] = rate
        run_config["bench"]["tx_size"] = tx_size
        run_config["bench"]["faults"] = faults
        if args.duration is not None:
            run_config["bench"]["duration"] = args.duration

        run_dir = os.path.join(sweep_dir, f"run_{rate}_{tx_size}")

        try:
            log_dir = run_benchmark(run_config, remote, log_dir=run_dir, debug=args.debug)
            logger = parse_logs(log_dir, faults=faults)
            entry = result_to_entry(logger, rate, tx_size)

            if logger:
                print(logger.result())
        except Exception as e:
            print(f"  Run failed: {e}")
            entry = {"rate": rate, "tx_size": tx_size, "error": True}

        all_results.append(entry)

        # Save per-run summary
        with open(os.path.join(run_dir, "summary.json"), "w") as f:
            json.dump(entry, f, indent=2)

        # Pause between runs
        if idx < len(sweep_points) - 1 and args.pause > 0:
            print(f"\nWaiting {args.pause}s before next run...")
            time.sleep(args.pause)

    # Save all results
    with open(os.path.join(sweep_dir, "sweep_results.json"), "w") as f:
        json.dump(all_results, f, indent=2)

    print_summary(all_results, sweep_dir)


if __name__ == "__main__":
    main()
