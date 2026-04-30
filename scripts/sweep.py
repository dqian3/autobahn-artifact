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

from autobahn_config import load_config, get_all_vms
from remote import load_remote
from bench import run_benchmark, cmd_upload, build_binaries


_NOISE_RE = None

def _strip_noise(path):
    """Read a log file, dropping benign client-shutdown panic lines.

    The parser rejects any client log matching /Error/, but the benchmark
    client panics at shutdown when the worker socket closes
    (benchmark_client.rs:216). Those panics are post-benchmark and don't
    affect measurements, so we filter them before parsing.
    """
    import re
    global _NOISE_RE
    if _NOISE_RE is None:
        _NOISE_RE = re.compile(r'panicked at|SendError|Failed to send transaction')
    with open(path, 'r') as f:
        return ''.join(line for line in f if not _NOISE_RE.search(line))


def parse_logs(log_dir, faults=0):
    """Parse logs and return a result dict, or None on failure."""
    try:
        from glob import glob
        from benchmark.logs import LogParser
        clients = [_strip_noise(p) for p in sorted(glob(os.path.join(log_dir, 'client-*.log')))]
        primaries = [_strip_noise(p) for p in sorted(glob(os.path.join(log_dir, 'primary-*.log')))]
        workers = [_strip_noise(p) for p in sorted(glob(os.path.join(log_dir, 'worker-*.log')))]
        logger = LogParser(clients, primaries, workers, faults=faults)
        return logger
    except Exception as e:
        print(f"  Log parsing failed: {e}")
        return None


def _lat_stats_ms(values_seconds):
    """Return mean/median/p90 (in ms) for a list of latencies in seconds.

    p90 uses linear interpolation between the two surrounding order statistics
    (matches numpy.percentile default).
    """
    from statistics import mean, median
    if not values_seconds:
        return {"mean": 0, "median": 0, "p90": 0}
    s = sorted(values_seconds)
    n = len(s)
    idx = 0.9 * (n - 1)
    lo = int(idx)
    hi = min(lo + 1, n - 1)
    frac = idx - lo
    p90 = s[lo] + (s[hi] - s[lo]) * frac
    return {
        "mean": mean(values_seconds) * 1000,
        "median": median(values_seconds) * 1000,
        "p90": p90 * 1000,
    }


def _consensus_latencies(logger):
    """Per-committed-batch latency in seconds: commit_time - first_propose_time."""
    return [c - logger.proposals[d] for d, c in logger.commits.items() if d in logger.proposals]


def _e2e_latencies(logger):
    """Per-sample-transaction latency in seconds: commit_time - client_send_time."""
    out = []
    for sent, received in zip(logger.sent_samples, logger.received_samples):
        for tx_id, batch_id in received.items():
            if batch_id in logger.commits and tx_id in sent:
                out.append(logger.commits[batch_id] - sent[tx_id])
    return out


def result_to_entry(logger, rate, tx_size, round_idx=None):
    """Convert a LogParser result to a summary dict."""
    base = {"rate": rate, "tx_size": tx_size}
    if round_idx is not None:
        base["round"] = round_idx

    if logger is None:
        base["error"] = True
        return base

    consensus_tps, consensus_bps, duration = logger._consensus_throughput()
    e2e_tps, e2e_bps, _ = logger._end_to_end_throughput()

    cons_stats = _lat_stats_ms(_consensus_latencies(logger))
    e2e_stats = _lat_stats_ms(_e2e_latencies(logger))

    base.update({
        "input_rate": sum(logger.rate),
        "consensus_tps": round(consensus_tps),
        "consensus_bps": round(consensus_bps),
        "consensus_latency_ms": round(cons_stats["mean"]),
        "consensus_latency_ms_median": round(cons_stats["median"]),
        "consensus_latency_ms_p90": round(cons_stats["p90"]),
        "e2e_tps": round(e2e_tps),
        "e2e_bps": round(e2e_bps),
        "e2e_latency_ms": round(e2e_stats["mean"]),
        "e2e_latency_ms_median": round(e2e_stats["median"]),
        "e2e_latency_ms_p90": round(e2e_stats["p90"]),
        "duration_s": round(duration) if duration else 0,
        "committee_size": logger.committee_size,
        "workers": logger.workers,
        "misses": logger.misses,
    })
    return base


def aggregate_by_rate(all_results):
    """Group successful runs by (rate, tx_size) and take the median of each
    numeric stat across rounds. Returns a list of aggregated dicts sorted
    by (rate, tx_size) with an extra `n_runs` field."""
    from collections import defaultdict
    from statistics import median

    groups = defaultdict(list)
    for r in all_results:
        if r.get("error"):
            continue
        groups[(r["rate"], r["tx_size"])].append(r)

    numeric_keys = [
        "input_rate", "consensus_tps", "consensus_bps",
        "consensus_latency_ms", "consensus_latency_ms_median", "consensus_latency_ms_p90",
        "e2e_tps", "e2e_bps",
        "e2e_latency_ms", "e2e_latency_ms_median", "e2e_latency_ms_p90",
        "duration_s", "misses",
    ]

    aggregated = []
    for (rate, tx_size), entries in sorted(groups.items()):
        agg = {"rate": rate, "tx_size": tx_size, "n_runs": len(entries)}
        for key in numeric_keys:
            vals = [e[key] for e in entries if key in e]
            if vals:
                agg[key] = round(median(vals))
        aggregated.append(agg)
    return aggregated


def print_aggregate_summary(all_results, sweep_dir):
    """Print the per-rate median-across-rounds table and persist it to JSON.

    Skipped if there's only one round (nothing to aggregate)."""
    if not any("round" in r for r in all_results):
        return

    aggregated = aggregate_by_rate(all_results)
    if not aggregated:
        return

    width = 170
    print(f"\n{'=' * width}")
    print(f"  AGGREGATE SUMMARY  (median across all rounds, per rate; latency in ms)")
    print(f"{'=' * width}")

    header = (
        f"{'Runs':>5} "
        f"{'Rate':>10} {'TxSize':>8} {'InRate':>10} "
        f"{'ConsTPS':>10} {'ConsBPS':>12} "
        f"{'ConsLatMean':>12} {'ConsLatMed':>11} {'ConsLatP90':>11} "
        f"{'E2E TPS':>10} {'E2E BPS':>12} "
        f"{'E2ELatMean':>11} {'E2ELatMed':>10} {'E2ELatP90':>10} "
        f"{'Dur':>5} {'Miss':>5}"
    )
    print(header)
    print("-" * len(header))

    for r in aggregated:
        cons_med = r.get("consensus_latency_ms_median", r["consensus_latency_ms"])
        cons_p90 = r.get("consensus_latency_ms_p90", r["consensus_latency_ms"])
        e2e_med = r.get("e2e_latency_ms_median", r["e2e_latency_ms"])
        e2e_p90 = r.get("e2e_latency_ms_p90", r["e2e_latency_ms"])
        print(
            f"{r['n_runs']:>5} "
            f"{r['rate']:>10} "
            f"{r['tx_size']:>8} "
            f"{r['input_rate']:>10,} "
            f"{r['consensus_tps']:>10,} "
            f"{r['consensus_bps']:>12,} "
            f"{r['consensus_latency_ms']:>12} "
            f"{cons_med:>11} "
            f"{cons_p90:>11} "
            f"{r['e2e_tps']:>10,} "
            f"{r['e2e_bps']:>12,} "
            f"{r['e2e_latency_ms']:>11} "
            f"{e2e_med:>10} "
            f"{e2e_p90:>10} "
            f"{r['duration_s']:>5} "
            f"{r['misses']:>5}"
        )
    print(f"{'=' * width}")

    out_path = os.path.join(sweep_dir, "sweep_aggregate.json")
    with open(out_path, "w") as f:
        json.dump(aggregated, f, indent=2)
    print(f"Aggregate (median per rate): {out_path}")


def print_summary(all_results, sweep_dir):
    """Print the sweep summary table, sorted by (round, rate, tx_size)."""
    has_round = any("round" in r for r in all_results)

    all_results = sorted(
        all_results,
        key=lambda r: (r.get("round", 0), r.get("rate", 0), r.get("tx_size", 0)),
    )

    width = 170
    print(f"\n{'=' * width}")
    print(f"  SWEEP SUMMARY  (latency columns: mean / median / p90, all in ms)")
    print(f"{'=' * width}")

    round_hdr = f"{'Round':>6} " if has_round else ""
    header = (
        f"{round_hdr}"
        f"{'Rate':>10} {'TxSize':>8} {'InRate':>10} "
        f"{'ConsTPS':>10} {'ConsBPS':>12} "
        f"{'ConsLatMean':>12} {'ConsLatMed':>11} {'ConsLatP90':>11} "
        f"{'E2E TPS':>10} {'E2E BPS':>12} "
        f"{'E2ELatMean':>11} {'E2ELatMed':>10} {'E2ELatP90':>10} "
        f"{'Dur':>5} {'Miss':>5}"
    )
    print(header)
    print("-" * len(header))

    for r in all_results:
        round_col = f"{r.get('round', ''):>6} " if has_round else ""
        if r.get("error"):
            print(f"{round_col}{r['rate']:>10} {r['tx_size']:>8}   ** FAILED **")
            continue
        # Fall back to mean for legacy entries that lack median/p90.
        cons_med = r.get("consensus_latency_ms_median", r["consensus_latency_ms"])
        cons_p90 = r.get("consensus_latency_ms_p90", r["consensus_latency_ms"])
        e2e_med = r.get("e2e_latency_ms_median", r["e2e_latency_ms"])
        e2e_p90 = r.get("e2e_latency_ms_p90", r["e2e_latency_ms"])
        print(
            f"{round_col}"
            f"{r['rate']:>10} "
            f"{r['tx_size']:>8} "
            f"{r['input_rate']:>10,} "
            f"{r['consensus_tps']:>10,} "
            f"{r['consensus_bps']:>12,} "
            f"{r['consensus_latency_ms']:>12} "
            f"{cons_med:>11} "
            f"{cons_p90:>11} "
            f"{r['e2e_tps']:>10,} "
            f"{r['e2e_bps']:>12,} "
            f"{r['e2e_latency_ms']:>11} "
            f"{e2e_med:>10} "
            f"{e2e_p90:>10} "
            f"{r['duration_s']:>5} "
            f"{r['misses']:>5}"
        )

    print(f"{'=' * width}")
    print(f"\nFull results: {sweep_dir}/sweep_results.json")

    print_aggregate_summary(all_results, sweep_dir)


def analyze(sweep_dir):
    """Re-parse logs from an existing sweep directory and print summary."""
    results_file = os.path.join(sweep_dir, "sweep_results.json")
    if os.path.exists(results_file):
        with open(results_file) as f:
            all_results = json.load(f)
        print_summary(all_results, sweep_dir)
        return

    # Try to re-parse from logs. Support both flat (run_*/) and nested
    # (round_*/run_*/) layouts.
    import glob as globmod
    flat = sorted(globmod.glob(os.path.join(sweep_dir, "run_*")))
    nested = sorted(globmod.glob(os.path.join(sweep_dir, "round_*", "run_*")))
    run_dirs = [(d, None) for d in flat] + [
        (d, int(os.path.basename(os.path.dirname(d)).replace("round_", "")))
        for d in nested
    ]
    if not run_dirs:
        print(f"No run_* directories or sweep_results.json found in {sweep_dir}")
        sys.exit(1)

    all_results = []
    for run_dir, round_idx in run_dirs:
        # Parse rate and tx_size from dir name: run_<rate>_<tx_size>
        basename = os.path.basename(run_dir)
        parts = basename.replace("run_", "").split("_")
        rate = int(parts[0])
        tx_size = int(parts[1]) if len(parts) > 1 else 512

        logger = parse_logs(run_dir, faults=0)
        all_results.append(result_to_entry(logger, rate, tx_size, round_idx=round_idx))

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
    ap.add_argument("--disable-crypto", action="store_true", default=False,
                    dest="disable_crypto",
                    help="Override params.disable_crypto=true for all runs "
                         "(replica sign/verify and client tx signing become no-ops).")
    ap.add_argument("--upload", action="store_true",
                    help="Build and upload binaries before sweeping")
    ap.add_argument("--output-dir", default=None, dest="output_dir",
                    help="Base directory for sweep output (default: scripts/logs/sweep_<timestamp>)")
    ap.add_argument("--debug", action="store_true",
                    help="Use verbose logging (-vvv)")
    ap.add_argument("--pause", type=int, default=10,
                    help="Seconds to pause between runs (default: 10)")
    ap.add_argument("--repeats", type=int, default=1,
                    help="Run the entire sweep N times. Each iteration's logs go under round_<i>/ (default: 1)")
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
    # config["vms"] is empty in non-colocate mode (replicas + clients live in
    # other keys); use the helper so the up-front check works for both modes.
    vms = get_all_vms(config)

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

    repeats = max(1, args.repeats)
    total_runs = len(sweep_points) * repeats

    print(f"\n=== Sweep: {len(sweep_points)} trial(s) x {repeats} round(s) = {total_runs} run(s), duration={duration}s ===")
    for rate, tx_size in sweep_points:
        print(f"  rate={rate:,} tx/s, tx_size={tx_size} B")
    print(f"Output: {sweep_dir}\n")

    all_results = []
    run_counter = 0

    for round_idx in range(1, repeats + 1):
        # Per-round subdir only when repeats > 1, to keep the legacy layout
        # (flat run_*/) intact for single-round sweeps.
        round_dir = (
            os.path.join(sweep_dir, f"round_{round_idx}") if repeats > 1 else sweep_dir
        )
        if repeats > 1:
            os.makedirs(round_dir, exist_ok=True)
            print(f"\n{'=' * 60}\n  ROUND {round_idx}/{repeats}\n{'=' * 60}")

        for idx, (rate, tx_size) in enumerate(sweep_points):
            run_counter += 1
            print(f"\n{'#' * 60}")
            print(
                f"  Run {run_counter}/{total_runs}"
                + (f" [round {round_idx}]" if repeats > 1 else "")
                + f": rate={rate:,} tx/s, tx_size={tx_size} B"
            )
            print(f"{'#' * 60}")

            # Deep copy config and apply overrides for this run
            run_config = copy.deepcopy(config)
            run_config["bench"]["rate"] = rate
            run_config["bench"]["tx_size"] = tx_size
            run_config["bench"]["faults"] = faults
            if args.duration is not None:
                run_config["bench"]["duration"] = args.duration
            if args.disable_crypto:
                run_config["params"]["disable_crypto"] = True

            run_dir = os.path.join(round_dir, f"run_{rate}_{tx_size}")

            entry_round = round_idx if repeats > 1 else None
            try:
                log_dir = run_benchmark(run_config, remote, log_dir=run_dir, debug=args.debug)
                logger = parse_logs(log_dir, faults=faults)
                entry = result_to_entry(logger, rate, tx_size, round_idx=entry_round)

                if logger:
                    print(logger.result())
            except Exception as e:
                print(f"  Run failed: {e}")
                entry = result_to_entry(None, rate, tx_size, round_idx=entry_round)

            all_results.append(entry)

            with open(os.path.join(run_dir, "summary.json"), "w") as f:
                json.dump(entry, f, indent=2)

            # Save partial sweep_results.json after each run so an interrupted
            # sweep is still analyzable.
            with open(os.path.join(sweep_dir, "sweep_results.json"), "w") as f:
                json.dump(all_results, f, indent=2)

            if run_counter < total_runs and args.pause > 0:
                print(f"\nWaiting {args.pause}s before next run...")
                time.sleep(args.pause)

    print_summary(all_results, sweep_dir)


if __name__ == "__main__":
    main()
