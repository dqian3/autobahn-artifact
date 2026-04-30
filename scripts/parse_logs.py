#!/usr/bin/env python3
"""Parse benchmark logs and print latency/throughput metrics.

Usage:
    python3 scripts/parse_logs.py
    python3 scripts/parse_logs.py scripts/logs
    python3 scripts/parse_logs.py /path/to/logs --faults 1
"""

import argparse
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "benchmark"))

from benchmark.logs import LogParser, ParseError


def main():
    ap = argparse.ArgumentParser(description="Parse Autobahn benchmark logs.")
    ap.add_argument("log_dir", nargs="?", default=str(REPO_ROOT / "scripts" / "logs"),
                    help="Directory containing client-*.log, primary-*.log, worker-*.log "
                         "(default: scripts/logs)")
    ap.add_argument("--faults", type=int, default=0,
                    help="Number of faulty nodes in the run (default: 0)")
    args = ap.parse_args()

    log_dir = Path(args.log_dir)
    if not log_dir.is_dir():
        print(f"ERROR: {log_dir} is not a directory", file=sys.stderr)
        sys.exit(1)

    try:
        parser = LogParser.process(str(log_dir), faults=args.faults)
    except ParseError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        sys.exit(1)

    print(parser.result())


if __name__ == "__main__":
    main()
