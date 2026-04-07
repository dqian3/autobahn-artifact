"""Configuration loading and generation for Autobahn benchmarks.

Handles YAML config parsing, committee JSON generation, and parameters JSON
generation. Port allocation exactly matches benchmark/benchmark/config.py.
"""

import json
from collections import OrderedDict

import yaml


def default_params():
    """Return default Parameters matching the Rust Parameters::default() impl."""
    return {
        "timeout_delay": 1000,
        "header_size": 1000,
        "max_header_delay": 100,
        "gc_depth": 50,
        "sync_retry_delay": 5000,
        "sync_retry_nodes": 3,
        "batch_size": 500_000,
        "max_batch_delay": 100,
        "use_optimistic_tips": True,
        "use_parallel_proposals": True,
        "k": 4,
        "use_fast_path": True,
        "fast_path_timeout": 500,
        "use_ride_share": False,
        "car_timeout": 2000,
        "simulate_asynchrony": False,
        "asynchrony_type": [0],
        "asynchrony_start": [20000],
        "asynchrony_duration": [10000],
        "affected_nodes": [0],
        "egress_penalty": 0,
        "use_fast_sync": False,
        "use_exponential_timeouts": False,
    }


def default_bench():
    """Return default benchmark parameters."""
    return {
        "faults": 0,
        "rate": 50_000,
        "tx_size": 512,
        "duration": 20,
        "runs": 1,
    }


def default_config():
    """Return a full default config dict."""
    return {
        "platform": "gcloud",
        "zone": None,
        "project": None,
        "vms": [],
        "repo": {
            "url": "https://github.com/neilgiri/autobahn-artifact.git",
            "name": "autobahn-artifact",
            "branch": "autobahn-blips",
        },
        "base_port": 5000,
        "workers": 1,
        "bench": default_bench(),
        "params": default_params(),
    }


def load_config(path):
    """Load a YAML config file and fill in defaults for missing fields."""
    with open(path, "r") as f:
        cfg = yaml.safe_load(f) or {}

    defaults = default_config()

    # Top-level scalars
    for key in ("platform", "zone", "project", "base_port", "workers"):
        cfg.setdefault(key, defaults[key])

    cfg.setdefault("vms", defaults["vms"])
    cfg.setdefault("repo", {})
    for key in ("url", "name", "branch"):
        cfg["repo"].setdefault(key, defaults["repo"][key])

    # Bench params: merge over defaults
    bench_defaults = default_bench()
    cfg.setdefault("bench", {})
    for key, val in bench_defaults.items():
        cfg["bench"].setdefault(key, val)

    # Node params: merge over defaults
    param_defaults = default_params()
    cfg.setdefault("params", {})
    for key, val in param_defaults.items():
        cfg["params"].setdefault(key, val)

    return cfg


def generate_committee(keys, ips, base_port, workers):
    """Build the committee JSON consumed by the Rust binary.

    Port allocation exactly matches benchmark/benchmark/config.py:Committee.__init__.
    Single incrementing port counter across all authorities.

    Args:
        keys: list of dicts with 'name' field (public key strings), one per authority
        ips: list of IP strings, one per authority (co-locate mode)
        base_port: starting port number
        workers: number of workers per authority

    Returns:
        dict matching the Committee JSON schema expected by the Rust binary.
    """
    assert len(keys) == len(ips)

    port = base_port
    authorities = OrderedDict()

    for key, ip in zip(keys, ips):
        name = key["name"]

        # Consensus address
        consensus_addr = {
            "consensus_to_consensus": f"{ip}:{port}",
        }
        port += 1

        # Primary addresses
        primary_addr = {
            "primary_to_primary": f"{ip}:{port}",
            "worker_to_primary": f"{ip}:{port + 1}",
        }
        port += 2

        # Worker addresses (co-locate: all on same IP)
        workers_addr = OrderedDict()
        for j in range(workers):
            workers_addr[j] = {
                "primary_to_worker": f"{ip}:{port}",
                "transactions": f"{ip}:{port + 1}",
                "worker_to_worker": f"{ip}:{port + 2}",
            }
            port += 3

        authorities[name] = {
            "stake": 1,
            "consensus": consensus_addr,
            "primary": primary_addr,
            "workers": workers_addr,
        }

    return {"authorities": authorities}


def generate_parameters(config):
    """Build the Parameters JSON from config, using defaults for missing fields."""
    params = dict(default_params())
    params.update(config.get("params", {}))
    return params


def committee_workers_addresses(committee, faults=0):
    """Extract worker transaction addresses from committee JSON.

    Returns list of list of (worker_id, transaction_address) per authority.
    Skips the last `faults` authorities.
    """
    authorities = list(committee["authorities"].values())
    good_nodes = len(authorities) - faults
    result = []
    for auth in authorities[:good_nodes]:
        auth_workers = []
        for wid, worker in auth["workers"].items():
            auth_workers.append((int(wid), worker["transactions"]))
        result.append(auth_workers)
    return result


def committee_primary_addresses(committee, faults=0):
    """Extract primary addresses from committee JSON.

    Returns list of primary_to_primary addresses.
    Skips the last `faults` authorities.
    """
    authorities = list(committee["authorities"].values())
    good_nodes = len(authorities) - faults
    return [
        auth["primary"]["primary_to_primary"]
        for auth in authorities[:good_nodes]
    ]


def all_vms(config):
    """Return the list of VM names from config."""
    return config["vms"]


def ip_from_address(address):
    """Extract IP from 'ip:port' string."""
    return address.split(":")[0]
