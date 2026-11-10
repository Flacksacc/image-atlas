"""One-line mining progress and honest process/device resource measurements."""
import os
import math
import re
import shutil
import subprocess
import sys
import threading
import time
from collections import deque

import psutil


def _process_usage() -> tuple[float, int]:
    """CPU seconds and resident RAM for this process and live model workers."""
    parent = psutil.Process(os.getpid())
    processes = [parent, *parent.children(recursive=True)]
    cpu_seconds = 0.0
    ram_bytes = 0
    for process in processes:
        try:
            times = process.cpu_times()
            cpu_seconds += times.user + times.system
            ram_bytes += process.memory_info().rss
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            continue
    return cpu_seconds, ram_bytes


def _gpu_usage() -> tuple[int, int] | None:
    """Device-wide memory from NVIDIA SMI; per-process VRAM is unavailable on some Windows drivers."""
    executable = shutil.which("nvidia-smi")
    if executable is None:
        return None
    try:
        result = subprocess.run(
            [executable, "--query-gpu=memory.used,memory.total", "--format=csv,noheader,nounits"],
            capture_output=True, text=True, check=False, timeout=2,
            creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0,
        )
        if result.returncode:
            return None
        values = [tuple(int(part.strip()) for part in line.split(",")) for line in result.stdout.splitlines()]
        if not values or any(len(pair) != 2 for pair in values):
            return None
        return sum(pair[0] for pair in values) * 1024**2, sum(pair[1] for pair in values) * 1024**2
    except (OSError, ValueError, subprocess.TimeoutExpired):
        return None


def _gib(value: int) -> str:
    return f"{value / 1024**3:.1f}G"


def _ram_size(value: int) -> str:
    return _gib(value) if value >= 1024**3 else f"{value / 1024**2:.0f}M"


def _bar(done: int, total: int | None, width: int = 10) -> str:
    if total is None:
        position = (done // 100) % width
        return "[" + "." * position + "#" + "." * (width - position - 1) + "] --%"
    fraction = min(done / total, 1.0) if total else 1.0
    filled = round(fraction * width)
    return "[" + "#" * filled + "-" * (width - filled) + f"] {fraction:.0%}"


THROUGHPUT_WINDOW_SECONDS = 5.0
ETA_WINDOW_SECONDS = 60.0


def _throughput(completions: deque, started: float | None, now: float,
                window_seconds: float = THROUGHPUT_WINDOW_SECONDS) -> tuple[float | None, float]:
    while completions and completions[0] <= now - window_seconds:
        completions.popleft()
    elapsed = min(max(now - started, 0), window_seconds) if started is not None else 0
    return (len(completions) / elapsed if elapsed else None), elapsed


class MiningProgress:
    def __init__(self, stream=None, interval: float = 0.75):
        self.stream = stream if stream is not None else sys.stderr
        self.interactive = self.stream.isatty()
        self.interval = interval
        self.started = time.monotonic()
        self.stage_started = self.started
        self.stage = "COUNT"
        self.model = None
        self.model_id = None
        self.device = 'cpu'
        self.runtime_state = 'active'
        self.stage_gpu_used = False
        self._unknown_runtime_used = False
        self.devices = ['cpu']
        self.stage_runtimes = {}
        self.done = 0
        self.images_processed = 0
        self._stage_completions = deque()
        self._eta_completions = deque()
        self._last_completions = self._stage_completions
        self._last_processing_started = None
        self._last_processing_stage = None
        self.examined = 0
        self.total = None
        self.pictures_total = None
        self.cpu_percent = 0.0
        self.cpu_peak_percent = 0.0
        self._cpu_seconds, self.ram_bytes = _process_usage()
        self.ram_peak_bytes = self.ram_bytes
        self.gpu = None
        self.gpu_peak_used_bytes = 0
        self._sampled_at = self.started
        self._last_width = 0
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, name="image-atlas-progress", daemon=True)

    def start(self):
        self._thread.start()
        return self

    def begin(self, stage: str, total: int | None = None, pictures_total: int | None = None,
              *, model=None, model_id=None, device='cpu', runtime_state='active'):
        with self._lock:
            self.stage = stage.upper()
            self.model = model
            self.model_id = model_id
            self.device = device
            self.runtime_state = runtime_state
            self.devices = []
            self.stage_gpu_used = None
            self._unknown_runtime_used = False
            self._record_runtime()
            self.done = 0
            self.examined = 0
            self.total = total
            if pictures_total is not None:
                self.pictures_total = pictures_total
            self.stage_started = time.monotonic()
            self._stage_completions = deque()
            self._eta_completions = deque()
            self._render()

    def _eta_metrics(self, now):
        seconds = None
        if self.total is not None:
            remaining = max(self.total - self.done, 0)
            if remaining == 0:
                seconds = 0.0
            else:
                rate, elapsed = _throughput(self._eta_completions, self.stage_started, now, ETA_WINDOW_SECONDS)
                if len(self._eta_completions) >= 2 and elapsed >= 2 and rate and rate > 0:
                    seconds = remaining / rate
        return {'estimated_remaining_seconds': round(seconds, 2) if seconds is not None else None,
                'estimated_remaining_minutes': round(seconds / 60, 2) if seconds is not None else None,
                'estimated_remaining_hours': round(seconds / 3600, 3) if seconds is not None else None}

    def _eta_label(self):
        seconds = self._eta_metrics(time.monotonic())['estimated_remaining_seconds']
        if seconds is None:
            return 'ETA calculating'
        minutes = math.ceil(seconds / 60)
        if self.total is not None and self.done < self.total:
            minutes = max(1, minutes)
        return f'ETA {minutes // 60}h {minutes % 60:02d}m'

    def _record_runtime(self):
        device = str(self.device).lower()
        gpu_used = (True if device.startswith(('cuda', 'mps', 'gpu')) else
                    False if device in {'cpu', 'not used'} else None)
        if gpu_used is not None:
            if device not in self.devices and device != 'not used':
                self.devices.append(device)
            self.stage_gpu_used = (True if self.stage_gpu_used is True or gpu_used else
                                   None if self._unknown_runtime_used else False)
        elif device != 'loading':
            self._unknown_runtime_used = True
            if self.stage_gpu_used is not True:
                self.stage_gpu_used = None
        self.stage_runtimes[self.stage.lower()] = self._runtime()

    def _runtime(self):
        return {'model': self.model, 'model_id': self.model_id, 'device': self.device,
                'devices': list(self.devices), 'gpu_used': self.stage_gpu_used,
                'runtime_state': self.runtime_state}

    def set_runtime(self, *, model, model_id=None, device='unknown', runtime_state='active'):
        """Called by the actual encoder after loading, not inferred from system VRAM."""
        with self._lock:
            self.model, self.model_id = model, model_id
            self.device, self.runtime_state = str(device), runtime_state
            self._record_runtime()
            self._render()

    def _runtime_label(self):
        device = str(self.device).lower()
        if device.startswith(('cuda', 'mps', 'gpu')):
            usage = f'yes ({self.device})'
        elif device == 'cpu':
            usage = 'no (CPU)'
        elif device == 'not used':
            usage = 'no (not run)'
        else:
            usage = 'pending' if device == 'loading' else 'unknown'
        return f'Model {self.model or "none"} | GPU {usage}'

    def end_runtime(self, *, failed=False):
        with self._lock:
            if failed:
                self.runtime_state = 'failed'
                if self.device == 'loading':
                    self.device = 'unknown'
            elif self.device == 'loading':
                self.device, self.runtime_state = 'not used', 'no model work performed'
            self._record_runtime()
            self._render()

    def advance(self):
        with self._lock:
            self.done += 1
            now = time.monotonic()
            self._eta_completions.append(now)
            _throughput(self._eta_completions, self.stage_started, now, ETA_WINDOW_SECONDS)
            if self.stage not in ("COUNT", "FIND"):
                self._stage_completions.append(now)
                _throughput(self._stage_completions, self.stage_started, now)
                self._last_completions = self._stage_completions
                self._last_processing_started = self.stage_started
                self._last_processing_stage = self.stage
            if self.stage == "SCAN":
                self.images_processed += 1

    def examine(self):
        with self._lock:
            self.examined += 1

    def set_total(self, total: int):
        with self._lock:
            self.total = total
            self._render()

    def stage_metrics(self) -> dict:
        with self._lock:
            now = time.monotonic()
            elapsed = now - self.stage_started
            rate, window = _throughput(self._stage_completions, self.stage_started, now)
            runtime = self._runtime()
            eta = self._eta_metrics(now)
            self._render()
        return {"elapsed_seconds": round(elapsed, 2), "images_per_second": round(rate, 3) if rate is not None else None,
                "throughput_window_seconds": round(window, 3), **runtime, **eta}

    def _sample(self):
        try:
            cpu_seconds, ram_bytes = _process_usage()
            gpu = _gpu_usage()
            now = time.monotonic()
            elapsed = max(now - self._sampled_at, 1e-6)
            cpu = 100 * max(cpu_seconds - self._cpu_seconds, 0) / elapsed / (psutil.cpu_count() or 1)
            with self._lock:
                self.cpu_percent = cpu
                self.cpu_peak_percent = max(cpu, self.cpu_peak_percent)
                self.ram_bytes = ram_bytes
                self.ram_peak_bytes = max(ram_bytes, self.ram_peak_bytes)
                self.gpu = gpu
                if gpu:
                    self.gpu_peak_used_bytes = max(gpu[0], self.gpu_peak_used_bytes)
                self._render()
            self._sampled_at = now
            self._cpu_seconds = cpu_seconds
        except (OSError, psutil.Error):
            pass

    def _line(self) -> str:
        count = f"{self.done}/{self.total}" if self.total is not None else str(self.done)
        pictures = self.pictures_total if self.pictures_total is not None else self.done
        rate, _ = _throughput(self._stage_completions, self.stage_started, time.monotonic())
        average = f"{rate:.1f} img/s (5s)" if rate is not None else "-- img/s (5s)"
        gpu = f"{_gib(self.gpu[0])}/{_gib(self.gpu[1])}" if self.gpu else "n/a"
        bar = _bar(self.done, self.total)
        runtime = self._runtime_label()
        eta = self._eta_label()
        if self.stage == "COUNT":
            return (f"COUNT {bar} {self.done}/? image paths | measuring total"
                    f" | {eta}"
                    f" | {runtime}"
                    f" | CPU {self.cpu_percent:.0f}% | RAM {_ram_size(self.ram_bytes)} | VRAM {gpu}")
        if self.stage == "FIND":
            if self.pictures_total is None:
                return (f"FIND {bar} {count} paths checked"
                        f" | {eta}"
                        f" | {runtime}"
                        f" | CPU {self.cpu_percent:.0f}% | RAM {_ram_size(self.ram_bytes)} | VRAM {gpu}")
            checked = self.examined if self.examined else self.done
            return (f"FIND {bar} {count} selected | {checked} paths checked | max {pictures} photos"
                    f" | {eta}"
                    f" | {runtime}"
                    f" | CPU {self.cpu_percent:.0f}% | RAM {_ram_size(self.ram_bytes)} | VRAM {gpu}")
        return (f"{self.stage} {bar} {count} | {eta} | {runtime} | {pictures} photos | {average} | CPU {self.cpu_percent:.0f}%"
                f" | RAM {_ram_size(self.ram_bytes)} | VRAM {gpu}")

    def _render(self):
        if not self.interactive:
            return
        try:
            width = max(shutil.get_terminal_size((100, 20)).columns - 1, 1)
            line = self._line()
            if len(line) > width:
                line = line.replace(" photos | ", "p ").replace(" | ", " ").replace(" img/s", "i/s")
            if len(line) > width:
                line = line.replace('Model ', '').replace('GPU no (CPU)', 'GPU:no/CPU').replace('GPU yes (', 'GPU:yes(').replace(' (5s)', '')
            if len(line) > width:
                line = re.sub(r'\[[#.\-]+\] ?', '', line)
            line = line[:width]
            self.stream.write("\r" + line + " " * max(0, self._last_width - len(line)))
            self.stream.flush()
            self._last_width = len(line)
        except OSError:
            self.interactive = False

    def _run(self):
        while not self._stop.is_set():
            self._sample()
            self._stop.wait(self.interval)

    def finish(self, pictures_total: int, *, failed: bool = False, cancelled: bool = False) -> dict:
        self._stop.set()
        self._thread.join(timeout=3)
        self._sample()
        now = time.monotonic()
        elapsed = now - self.started
        rate, window = _throughput(self._last_completions, self._last_processing_started, now)
        metrics = {
            "pictures_total": pictures_total,
            "images_processed": self.images_processed,
            "elapsed_seconds": round(elapsed, 2),
            "images_per_second": round(rate, 3) if rate is not None else None,
            "throughput_window_seconds": round(window, 3),
            "throughput_stage": self._last_processing_stage.lower() if self._last_processing_stage else None,
            'current_step': self.stage.lower(),
            **self._eta_metrics(now),
            "cpu_percent": round(self.cpu_percent, 1),
            "cpu_peak_percent": round(self.cpu_peak_percent, 1),
            "ram_bytes": self.ram_bytes,
            "ram_peak_bytes": self.ram_peak_bytes,
            "gpu_vram_used_bytes": self.gpu[0] if self.gpu else None,
            "gpu_vram_peak_used_bytes": self.gpu_peak_used_bytes if self.gpu_peak_used_bytes else None,
            "gpu_vram_total_bytes": self.gpu[1] if self.gpu else None,
            'stage_runtimes': dict(self.stage_runtimes),
            'gpu_used': (True if any(step['gpu_used'] is True for step in self.stage_runtimes.values()) else
                         None if any(step['gpu_used'] is None for step in self.stage_runtimes.values()) else False),
        }
        if self.interactive:
            with self._lock:
                status = "CANCELLED" if cancelled else ("ERROR" if failed else "DONE")
                average = f"{rate:.1f} img/s (5s)" if rate is not None else "-- img/s (5s)"
                gpu = f"{_gib(self.gpu_peak_used_bytes)}/{_gib(self.gpu[1])}" if self.gpu else "n/a"
                line = (f"{status} {_bar(self.done, self.total)} {self._runtime_label()} | {pictures_total}p {elapsed:.1f}s {average}"
                        f" CPUmax {self.cpu_peak_percent:.0f}% RAMmax {_ram_size(self.ram_peak_bytes)}"
                        f" VRAMmax {gpu}")
                width = max(shutil.get_terminal_size((100, 20)).columns - 1, 1)
                line = line[:width]
                self.stream.write("\r" + line + " " * max(0, self._last_width - len(line)) + "\n")
                self.stream.flush()
        return metrics
