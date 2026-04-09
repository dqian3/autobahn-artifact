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
        "colocate": True,
        "vms": [],
        # Non-colocate fields:
        "replica_vms": [],
        "clients": [],  # list of {vm, authority, worker} dicts
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
    for key in ("platform", "zone", "project", "colocate", "base_port", "workers"):
        cfg.setdefault(key, defaults[key])

    cfg.setdefault("vms", defaults["vms"])
    cfg.setdefault("replica_vms", defaults["replica_vms"])
    cfg.setdefault("clients", defaults["clients"])
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


def generate_committee(keys, host_lists, base_port, workers):
    """Build the committee JSON consumed by the Rust binary.

    Port allocation exactly matches benchmark/benchmark/config.py:Committee.__init__.
    Single incrementing port counter across all authorities.

    Args:
        keys: list of dicts with 'name' field (public key strings), one per authority
        host_lists: list of host-lists, one per authority. Each host-list is:
            - colocate mode:   [ip] * (1 + workers)  (all same IP)
            - non-colocate:    [primary_ip, worker0_ip, worker1_ip, ...]
          The first entry is used for consensus + primary; remaining for workers.
        base_port: starting port number
        workers: number of workers per authority

    Returns:
        dict matching the Committee JSON schema expected by the Rust binary.
    """
    assert len(keys) == len(host_lists)

    port = base_port
    authorities = OrderedDict()

    for key, hosts in zip(keys, host_lists):
        name = key["name"]
        assert len(hosts) == 1 + workers, (
            f"Authority needs 1 primary + {workers} worker host(s), got {len(hosts)}"
        )

        primary_ip = hosts[0]
        worker_ips = hosts[1:]

        # Consensus address (on primary host)
        consensus_addr = {
            "consensus_to_consensus": f"{primary_ip}:{port}",
        }
        port += 1

        # Primary addresses (on primary host)
        primary_addr = {
            "primary_to_primary": f"{primary_ip}:{port}",
            "worker_to_primary": f"{primary_ip}:{port + 1}",
        }
        port += 2

        # Worker addresses
        workers_addr = OrderedDict()
        for j in range(workers):
            wip = worker_ips[j]
            workers_addr[j] = {
                "primary_to_worker": f"{wip}:{port}",
                "transactions": f"{wip}:{port + 1}",
                "worker_to_worker": f"{wip}:{port + 2}",
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


def get_all_vms(config):
    """Return deduplicated list of all VM names from config (preserving order)."""
    if config.get("colocate", True):
        return list(config["vms"])
    else:
        seen = set()
        result = []
        for vm in config["replica_vms"]:
            if vm not in seen:
                seen.add(vm)
                result.append(vm)
        for c in config["clients"]:
            vm = c["vm"]
            if vm not in seen:
                seen.add(vm)
                result.append(vm)
        return result


def get_num_authorities(config):
    """Return the number of authorities (nodes) in the config."""
    if config.get("colocate", True):
        return len(config["vms"])
    else:
        return len(config["replica_vms"])


def get_client_assignments(config):
    """Return list of client assignments for non-colocate mode.

    Each entry is a dict: {vm, authority, worker}.
    In colocate mode, synthesizes one client per worker on the same VM.
    """
    workers = config["workers"]

    if config.get("colocate", True):
        result = []
        for i, vm in enumerate(config["vms"]):
            for wid in range(workers):
                result.append({"vm": vm, "authority": i, "worker": wid})
        return result
    else:
        return list(config["clients"])


def build_host_lists(config, ips):
    """Build per-authority host lists for generate_committee().

    Primary + workers are always co-located on the replica VM, so each
    authority's host list is [replica_ip] * (1 + workers).

    Args:
        config: loaded config dict
        ips: dict of {vm_name: internal_ip}

    Returns:
        list of [primary_ip, worker0_ip, ...] per authority
    """
    workers = config["workers"]
    if config.get("colocate", True):
        vms = config["vms"]
    else:
        vms = config["replica_vms"]

    host_lists = []
    for vm in vms:
        ip = ips[vm]
        host_lists.append([ip] * (1 + workers))
    return host_lists


def get_replica_vms(config):
    """Return the list of replica VMs (one per authority)."""
    if config.get("colocate", True):
        return list(config["vms"])
    else:
        return list(config["replica_vms"])


def validate_config(config):
    """Validate config and raise ValueError on problems."""
    colocate = config.get("colocate", True)
    workers = config["workers"]

    if colocate:
        if not config["vms"]:
            raise ValueError("colocate=true requires 'vms' list")
    else:
        if not config["replica_vms"]:
            raise ValueError("colocate=false requires 'replica_vms' list")
        if not config["clients"]:
            raise ValueError("colocate=false requires 'clients' list")
        num_auth = len(config["replica_vms"])
        for c in config["clients"]:
            if not isinstance(c, dict) or "vm" not in c or "authority" not in c or "worker" not in c:
                raise ValueError(
                    f"Each client entry must have 'vm', 'authority', 'worker' fields, got: {c}"
                )
            if c["authority"] >= num_auth:
                raise ValueError(
                    f"Client authority {c['authority']} >= num authorities {num_auth}"
                )
            if c["worker"] >= workers:
                raise ValueError(
                    f"Client worker {c['worker']} >= num workers {workers}"
                )


def ip_from_address(address):
    """Extract IP from 'ip:port' string."""
    return address.split(":")[0]
