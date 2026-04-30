"""Reusable remote execution backends for benchmark and protocol automation.

Provides an abstract Remote interface with GCloud and SSH implementations.
"""

import subprocess
import sys
from abc import ABC, abstractmethod
from concurrent.futures import ThreadPoolExecutor, as_completed


class Remote(ABC):
    """Abstract interface for remote machine operations."""

    @abstractmethod
    def ssh(self, host, command, bg=False):
        """Run a command on a remote host.

        Args:
            host: Host identifier (VM name for GCloud, IP/hostname for SSH).
            command: Shell command string to execute.
            bg: If True, return Popen handle without waiting. Otherwise block.

        Returns:
            Popen if bg=True, CompletedProcess otherwise.
        """

    @abstractmethod
    def scp_upload(self, local_path, host, remote_path):
        """Upload a local file to a remote host."""

    @abstractmethod
    def scp_download(self, host, remote_path, local_path):
        """Download a file from a remote host."""

    @abstractmethod
    def get_ip(self, host):
        """Get the internal/reachable IP for a host."""

    def run_on_all(self, hosts, command, quiet=False):
        """Run a command on all hosts in parallel."""
        results = {}
        with ThreadPoolExecutor(max_workers=len(hosts)) as pool:
            futures = {pool.submit(self.ssh, h, command): h for h in hosts}
            for f in as_completed(futures):
                host = futures[f]
                try:
                    results[host] = f.result()
                except subprocess.CalledProcessError as e:
                    if not quiet:
                        stderr = (e.stderr or "").strip()
                        stdout = (e.output or "").strip()
                        detail = stderr or stdout or "(no output)"
                        print(f"[{host}] Command failed (exit {e.returncode}): {detail}")
                    results[host] = e
                except Exception as e:
                    if not quiet:
                        print(f"[{host}] Error: {e}")
                    results[host] = e
        return results

    def kill_process(self, hosts, process_name):
        """Kill a process by name on all hosts (SIGKILL)."""
        self.run_on_all(hosts, f"pkill -9 -f {process_name} || true", quiet=True)


class GCloudRemote(Remote):
    """Uses gcloud compute ssh/scp. Hosts are VM instance names.

    Automatically discovers the zone for each VM via `gcloud compute instances list`.
    A default zone can be provided as fallback.
    """

    def __init__(self, zone=None, project=None):
        self.default_zone = zone
        self.project = project
        self._zone_cache = {}  # vm_name -> zone

    def _resolve_zone(self, vm_name):
        """Look up the zone for a VM, using cache or gcloud discovery."""
        if vm_name in self._zone_cache:
            return self._zone_cache[vm_name]

        # Try to discover via gcloud
        cmd = [
            "gcloud", "compute", "instances", "list",
            f"--filter=name={vm_name}",
            "--format=value(zone)",
        ]
        if self.project:
            cmd.append(f"--project={self.project}")

        result = subprocess.run(cmd, capture_output=True, text=True)
        zone = result.stdout.strip()

        if zone:
            self._zone_cache[vm_name] = zone
            return zone

        if self.default_zone:
            self._zone_cache[vm_name] = self.default_zone
            return self.default_zone

        raise RuntimeError(
            f"Could not determine zone for VM '{vm_name}'. "
            "Set 'zone' in config as a fallback."
        )

    def _base_args(self, vm_name):
        zone = self._resolve_zone(vm_name)
        args = [f"--zone={zone}"]
        if self.project:
            args.append(f"--project={self.project}")
        return args

    def _discover_all(self, vm_names):
        """Pre-fetch zones for all VMs in a single gcloud call."""
        unknown = [v for v in vm_names if v not in self._zone_cache]
        if not unknown:
            return

        filter_expr = " OR ".join(f"name={v}" for v in unknown)
        cmd = [
            "gcloud", "compute", "instances", "list",
            f"--filter={filter_expr}",
            "--format=value(name,zone)",
        ]
        if self.project:
            cmd.append(f"--project={self.project}")

        result = subprocess.run(cmd, capture_output=True, text=True)
        for line in result.stdout.strip().splitlines():
            parts = line.split()
            if len(parts) == 2:
                self._zone_cache[parts[0]] = parts[1]

    def check_vms_running(self, vm_names):
        """Check that all VMs are RUNNING. Exit with an error if any are not."""
        self._discover_all(vm_names)
        statuses = self.vm_status(vm_names)
        not_running = []
        for vm in vm_names:
            status = statuses.get(vm, "NOT_FOUND")
            if status != "RUNNING":
                not_running.append((vm, status))
        if not_running:
            print("ERROR: The following VMs are not running:", file=sys.stderr)
            for vm, status in not_running:
                print(f"  {vm}: {status}", file=sys.stderr)
            print("\nStart them with: python bench.py vm-start --config <config>", file=sys.stderr)
            sys.exit(1)

    def ssh(self, vm_name, command, bg=False):
        cmd = [
            "gcloud", "compute", "ssh", vm_name,
            *self._base_args(vm_name),
            "--command", command,
        ]
        if bg:
            return subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

        result = subprocess.run(cmd, capture_output=True, text=True)
        if result.returncode != 0:
            raise subprocess.CalledProcessError(
                result.returncode, cmd,
                output=result.stdout, stderr=result.stderr,
            )
        return result

    def scp_upload(self, local_path, vm_name, remote_path):
        cmd = [
            "gcloud", "compute", "scp",
            local_path, f"{vm_name}:{remote_path}",
            *self._base_args(vm_name),
        ]
        result = subprocess.run(cmd, capture_output=True, text=True)
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or "(no output)"
            raise RuntimeError(f"scp upload to {vm_name}:{remote_path} failed (exit {result.returncode}): {detail}")

    def scp_download(self, vm_name, remote_path, local_path):
        cmd = [
            "gcloud", "compute", "scp",
            f"{vm_name}:{remote_path}", local_path,
            *self._base_args(vm_name),
        ]
        result = subprocess.run(cmd, capture_output=True, text=True)
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or "(no output)"
            raise RuntimeError(f"scp download {vm_name}:{remote_path} failed (exit {result.returncode}): {detail}")

    def get_ip(self, vm_name):
        if hasattr(self, '_ip_cache') and vm_name in self._ip_cache:
            return self._ip_cache[vm_name]
        cmd = [
            "gcloud", "compute", "instances", "describe", vm_name,
            *self._base_args(vm_name),
            "--format=get(networkInterfaces[0].networkIP)",
        ]
        result = subprocess.run(cmd, capture_output=True, text=True)
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or "(no output)"
            raise RuntimeError(f"Failed to get IP for '{vm_name}' (exit {result.returncode}): {detail}")
        return result.stdout.strip()

    def get_all_ips(self, vm_names):
        """Fetch IPs for all VMs in a single gcloud call instead of N separate ones."""
        self._discover_all(vm_names)
        filter_expr = " OR ".join(f"name={v}" for v in vm_names)
        cmd = [
            "gcloud", "compute", "instances", "list",
            f"--filter={filter_expr}",
            "--format=value(name,networkInterfaces[0].networkIP)",
        ]
        if self.project:
            cmd.append(f"--project={self.project}")
        result = subprocess.run(cmd, capture_output=True, text=True)
        if result.returncode != 0:
            detail = result.stderr.strip() or result.stdout.strip() or "(no output)"
            raise RuntimeError(f"Failed to get IPs (exit {result.returncode}): {detail}")
        ips = {}
        for line in result.stdout.strip().splitlines():
            parts = line.split()
            if len(parts) == 2:
                ips[parts[0]] = parts[1]
        if not hasattr(self, '_ip_cache'):
            self._ip_cache = {}
        self._ip_cache.update(ips)
        missing = [v for v in vm_names if v not in ips]
        if missing:
            raise RuntimeError(f"Could not resolve IPs for VMs: {missing}")
        return ips

    def vm_start(self, vm_names):
        self._discover_all(vm_names)
        def _start_one(vm):
            cmd = [
                "gcloud", "compute", "instances", "start",
                vm, *self._base_args(vm),
            ]
            result = subprocess.run(cmd, capture_output=True, text=True)
            if result.returncode != 0:
                detail = result.stderr.strip() or result.stdout.strip() or "(no output)"
                raise RuntimeError(f"Failed to start VM '{vm}' (exit {result.returncode}): {detail}")
        with ThreadPoolExecutor(max_workers=len(vm_names)) as pool:
            futures = {pool.submit(_start_one, vm): vm for vm in vm_names}
            for f in as_completed(futures):
                vm = futures[f]
                try:
                    f.result()
                    print(f"  {vm}: started")
                except Exception as e:
                    print(f"  [{vm}] {e}")

    def vm_status(self, vm_names):
        """Return {vm_name: status} for each VM (e.g. 'RUNNING', 'TERMINATED')."""
        self._discover_all(vm_names)
        filter_expr = " OR ".join(f"name={v}" for v in vm_names)
        cmd = [
            "gcloud", "compute", "instances", "list",
            f"--filter={filter_expr}",
            "--format=value(name,status)",
        ]
        if self.project:
            cmd.append(f"--project={self.project}")
        result = subprocess.run(cmd, capture_output=True, text=True, check=True)
        statuses = {}
        for line in result.stdout.strip().splitlines():
            parts = line.split()
            if len(parts) == 2:
                statuses[parts[0]] = parts[1]
        return statuses

    def vm_stop(self, vm_names):
        self._discover_all(vm_names)
        def _stop_one(vm):
            cmd = [
                "gcloud", "compute", "instances", "stop",
                vm, *self._base_args(vm),
            ]
            result = subprocess.run(cmd, capture_output=True, text=True)
            if result.returncode != 0:
                detail = result.stderr.strip() or result.stdout.strip() or "(no output)"
                raise RuntimeError(f"Failed to stop VM '{vm}' (exit {result.returncode}): {detail}")
        with ThreadPoolExecutor(max_workers=len(vm_names)) as pool:
            futures = {pool.submit(_stop_one, vm): vm for vm in vm_names}
            for f in as_completed(futures):
                vm = futures[f]
                try:
                    f.result()
                    print(f"  {vm}: stopped")
                except Exception as e:
                    print(f"  [{vm}] {e}")


class SSHRemote(Remote):
    """Uses plain ssh/scp. Hosts are IPs or hostnames."""

    def __init__(self, user="root", key_file=None):
        self.user = user
        self.key_file = key_file

    def _ssh_opts(self):
        opts = ["-o", "StrictHostKeyChecking=no"]
        if self.key_file:
            opts += ["-i", self.key_file]
        return opts

    def _target(self, host):
        return f"{self.user}@{host}"

    def ssh(self, host, command, bg=False):
        cmd = ["ssh", *self._ssh_opts(), self._target(host), command]
        if bg:
            return subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        return subprocess.run(cmd, check=True, capture_output=True, text=True)

    def scp_upload(self, local_path, host, remote_path):
        cmd = [
            "scp", *self._ssh_opts(),
            local_path, f"{self._target(host)}:{remote_path}",
        ]
        subprocess.run(cmd, check=True)

    def scp_download(self, host, remote_path, local_path):
        cmd = [
            "scp", *self._ssh_opts(),
            f"{self._target(host)}:{remote_path}", local_path,
        ]
        subprocess.run(cmd, check=True)

    def get_ip(self, host):
        return host


def load_remote(config):
    """Factory: reads 'platform' field from config, returns appropriate Remote."""
    platform = config.get("platform", "gcloud")
    if platform == "gcloud":
        return GCloudRemote(zone=config.get("zone"), project=config.get("project"))
    elif platform == "ssh":
        return SSHRemote(user=config.get("user", "root"), key_file=config.get("key_file"))
    else:
        raise ValueError(f"Unknown platform: {platform}")
