"""One-line mining progress and honest process/device resource measurements."""
import os
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


def _throughput(completions: deque, started: float | None, now: float) -> tuple[float | None, float]:
    while completions and completions[0] <= now - THROUGHPUT_WINDOW_SECONDS:
        completions.popleft()
    elapsed = min(max(now - started, 0), THROUGHPUT_WINDOW_SECONDS) if started is not None else 0
    return (len(completions) / elapsed if elapsed else None), elapsed


class MiningProgress:
    def __init__(self, stream=None, interval: float = 0.75):
        self.stream = stream if stream is not None else sys.stderr
        self.interactive = self.stream.isatty()
        self.interval = interval
        self.started = time.monotonic()
        self.stage_started = self.started
        self.stage = "COUNT"
        self.done = 0
        self.images_processed = 0
        self._stage_completions = deque()
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

    def begin(self, stage: str, total: int | None = None, pictures_total: int | None = None):
        with self._lock:
            self.stage = stage.upper()
            self.done = 0
            self.examined = 0
            self.total = total
            if pictures_total is not None:
                self.pictures_total = pictures_total
            self.stage_started = time.monotonic()
            self._stage_completions = deque()
            self._render()

    def advance(self):
        with self._lock:
            self.done += 1
            if self.stage not in ("COUNT", "FIND"):
                now = time.monotonic()
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
            self._render()
        return {"elapsed_seconds": round(elapsed, 2), "images_per_second": round(rate, 3) if rate is not None else None,
                "throughput_window_seconds": round(window, 3)}

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
        if self.stage == "COUNT":
            return (f"COUNT {bar} {self.done}/? image paths | measuring total"
                    f" | CPU {self.cpu_percent:.0f}% | RAM {_ram_size(self.ram_bytes)} | VRAM {gpu}")
        if self.stage == "FIND":
            if self.pictures_total is None:
                return (f"FIND {bar} {count} paths checked"
                        f" | CPU {self.cpu_percent:.0f}% | RAM {_ram_size(self.ram_bytes)} | VRAM {gpu}")
            checked = self.examined if self.examined else self.done
            return (f"FIND {bar} {count} selected | {checked} paths checked | max {pictures} photos"
                    f" | CPU {self.cpu_percent:.0f}% | RAM {_ram_size(self.ram_bytes)} | VRAM {gpu}")
        return (f"{self.stage} {bar} {count} | {pictures} photos | {average} | CPU {self.cpu_percent:.0f}%"
                f" | RAM {_ram_size(self.ram_bytes)} | VRAM {gpu}")

    def _render(self):
        if not self.interactive:
            return
        try:
            width = max(shutil.get_terminal_size((100, 20)).columns - 1, 1)
            line = self._line()
            if len(line) > width:
                line = line.replace(" photos | ", "p ").replace(" | ", " ").replace(" img/s", "i/s")
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
            "cpu_percent": round(self.cpu_percent, 1),
            "cpu_peak_percent": round(self.cpu_peak_percent, 1),
            "ram_bytes": self.ram_bytes,
            "ram_peak_bytes": self.ram_peak_bytes,
            "gpu_vram_used_bytes": self.gpu[0] if self.gpu else None,
            "gpu_vram_peak_used_bytes": self.gpu_peak_used_bytes if self.gpu_peak_used_bytes else None,
            "gpu_vram_total_bytes": self.gpu[1] if self.gpu else None,
        }
        if self.interactive:
            with self._lock:
                status = "CANCELLED" if cancelled else ("ERROR" if failed else "DONE")
                average = f"{rate:.1f} img/s (5s)" if rate is not None else "-- img/s (5s)"
                gpu = f"{_gib(self.gpu_peak_used_bytes)}/{_gib(self.gpu[1])}" if self.gpu else "n/a"
                line = (f"{status} {_bar(self.done, self.total)} {pictures_total}p {elapsed:.1f}s {average}"
                        f" CPUmax {self.cpu_peak_percent:.0f}% RAMmax {_ram_size(self.ram_peak_bytes)}"
                        f" VRAMmax {gpu}")
                width = max(shutil.get_terminal_size((100, 20)).columns - 1, 1)
                line = line[:width]
                self.stream.write("\r" + line + " " * max(0, self._last_width - len(line)) + "\n")
                self.stream.flush()
        return metrics
