"""Keep native decoder/OpenCV crashes outside the mining process."""
import json
import os
import queue
import subprocess
import sys
import threading
import time
from pathlib import Path


class FaceSetupError(RuntimeError):
    pass


class FaceWorkerError(RuntimeError):
    pass


class FaceWorkerClient:
    def __init__(self, *, check=None, python=None, worker_module='image_atlas.face_worker', timeout=120):
        self.check = check or (lambda: None)
        self.python = python or sys.executable
        self.worker_module = worker_module
        self.timeout = timeout
        self.process = None
        self.images = 0
        self.restarts = 0
        self.device = 'unknown'
        self._start()

    def _start(self):
        env = os.environ.copy()
        for name in ('IMAGE_CLI_DSN', 'IMAGEATLAS_DB_PASSWORD', 'PGPASSWORD', 'PGPASSFILE'):
            env.pop(name, None)
        env['PYTHONPATH'] = os.pathsep.join(filter(None, [str(Path(__file__).resolve().parents[1]), env.get('PYTHONPATH', '')]))
        self.process = subprocess.Popen([self.python, '-m', self.worker_module], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                                        encoding='utf-8', env=env, creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
        responses = self.responses = queue.Queue()
        process = self.process
        def read():
            try:
                for line in process.stdout:
                    responses.put(line)
            except (OSError, UnicodeError):
                pass
            finally:
                responses.put(None)
                process.stdout.close()
        threading.Thread(target=read, daemon=True).start()
        try:
            ready = self._exchange({'operation': 'check'})
            if ready.get('ready') is not True:
                raise FaceSetupError('Face worker did not confirm startup')
            self.device = str(ready.get('device', 'unknown'))
        except Exception as exc:
            self.abort()
            raise FaceSetupError(str(exc)) from exc
        except BaseException:
            self.abort()
            raise

    def _exchange(self, request):
        try:
            self.process.stdin.write(json.dumps(request) + '\n')
            self.process.stdin.flush()
            deadline = time.monotonic() + self.timeout
            while True:
                self.check()
                if time.monotonic() >= deadline:
                    raise FaceWorkerError(f'Face worker exceeded {self.timeout}s for this image')
                try:
                    line = self.responses.get(timeout=.1)
                    break
                except queue.Empty:
                    continue
            if line is None:
                code = self.process.wait(timeout=5)
                raise FaceWorkerError(f'Face worker exited unexpectedly (0x{code & 0xffffffff:08X}); image skipped')
            response = json.loads(line)
            if response.get('fatal'):
                raise FaceSetupError(response['error'])
            if 'error' in response:
                raise ValueError(response['error'])
            return response
        except (OSError, json.JSONDecodeError, subprocess.TimeoutExpired) as exc:
            raise FaceWorkerError('Face worker stopped or returned an invalid response') from exc

    def detect(self, path):
        from .catalogs import resolve
        catalog = resolve()
        if catalog.id:
            catalog.require_path(path)
        if self.process is None or self.images >= 200:
            self.close()
            self._start()
            self.images = 0
            self.restarts += 1
        try:
            response = self._exchange({'operation': 'detect', 'path': str(path)})
            self.images += 1
            return response['faces']
        except FaceWorkerError:
            self.abort()
            raise
        except BaseException:
            if not isinstance(sys.exception(), Exception):
                self.abort()
            raise

    def close(self):
        if self.process is not None:
            if self.process.poll() is None:
                self.process.stdin.close()
                try:
                    self.process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=5)
            else:
                self.process.stdin.close()
            self.process = None

    def abort(self):
        if self.process is not None:
            if self.process.poll() is None:
                self.process.kill()
                self.process.wait(timeout=5)
            self.process.stdin.close()
            self.process = None
