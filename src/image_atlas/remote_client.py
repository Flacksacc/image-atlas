"""Foreground SSH session, authenticated local broker, bounded preview prefetch."""
import base64
import hashlib
import json
import os
import queue
import secrets
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlencode

import psutil

from .catalogs import directory, secret, delete_secret

DISK_BUDGET = 512*1024*1024


def parse_code(code):
    try:
        data = json.loads(base64.urlsafe_b64decode(code.strip()).decode())
        if data['version'] != 1 or not 1 <= int(data['port']) <= 65535 or len(data['token']) < 32 or not data['catalog_id']:
            raise ValueError()
        return data
    except (ValueError, KeyError, TypeError):
        raise ValueError('Invalid ImageAtlas connection code') from None


def session_file():
    return directory()/'remote-session.json'


def active_session():
    try:
        data = json.loads(session_file().read_text())
        proc = psutil.Process(data['pid'])
        if abs(proc.create_time()-data['created_at']) > 0.1:
            raise ValueError()
        data['token'] = secret(data['credential'])
        return data
    except (OSError, ValueError, KeyError, psutil.Error):
        raise ValueError('No active remote session; run image-atlas remote connect first') from None


def request(path, *, data=None):
    session = active_session()
    return urllib.request.urlopen(urllib.request.Request(
        f"http://127.0.0.1:{session['port']}" + path, data=data,
        headers={'Authorization': 'Bearer '+session['token'], 'Content-Type': 'application/json'}), timeout=120)


def get_json(path):
    with request(path) as response:
        return json.load(response)


def search(q='', *, mode='combined', limit=100, offset=0, min_pixels=0, orientation='any', format='', image_vector=None):
    if image_vector is not None:
        from .image_query import validate_vector
        from .embeddings import MODEL_ID
        if get_json('/v1/session').get('image_search_model') != MODEL_ID:
            raise ValueError('Update the remote ImageAtlas server before searching by image')
        image_vector = json.dumps([round(value, 7) for value in validate_vector(image_vector)], separators=(',', ':'))
    result = get_json('/v1/images?'+urlencode(dict(q=q, mode=mode, limit=limit, offset=offset,
                          min_pixels=min_pixels, orientation=orientation, format=format,
                          image_vector=image_vector or '')))
    with request('/prefetch', data=json.dumps(result['items'][:24]).encode()):
        pass
    return result


def download(image_id, destination=None, *, kind='original', open_file=False):
    row = get_json(f'/v1/images/{image_id}')
    # Path names come from another Windows host, even on a non-Windows client.
    name = row['path'].replace('\\', '/').rsplit('/', 1)[-1]
    if kind != 'original':
        name = f"image-{image_id}." + ('jxl' if kind == 'preview' else 'webp')
    target = Path(destination) if destination else Path.home()/'Downloads'/name
    if target.is_dir():
        target /= name
    target.parent.mkdir(parents=True, exist_ok=True)
    partial = target.with_name(target.name+'.'+secrets.token_hex(6)+'.partial')
    try:
        with request(f'/v1/images/{image_id}/{kind}') as response, partial.open('xb') as output:
            digest = hashlib.sha256()
            written = 0
            while chunk := response.read(1024*1024):
                written += len(chunk)
                output.write(chunk)
                digest.update(chunk)
            expected = response.headers.get('Content-Length')
            if expected is not None and written != int(expected):
                raise ValueError('Incomplete image download')
            if kind == 'original' and row.get('sha256') and digest.hexdigest() != row['sha256']:
                raise ValueError('Original checksum mismatch; rescan the remote image')
        # Hard-link publication is atomic and refuses existing destinations.
        os.link(partial, target)
    finally:
        partial.unlink(missing_ok=True)
    if open_file:
        _open_file(target)
    return str(target)


def _open_file(path):
    if sys.platform == 'win32':
        os.startfile(path)
    else:
        subprocess.Popen(['open' if sys.platform == 'darwin' else 'xdg-open', str(path)])


def open_original(image_id):
    """Open a verified download again, or save a new copy without overwriting files."""
    row = get_json(f'/v1/images/{image_id}')
    name = row['path'].replace('\\', '/').rsplit('/', 1)[-1]
    target = Path.home() / 'Downloads' / name
    if target.is_file() and row.get('sha256'):
        with target.open('rb') as source:
            if hashlib.file_digest(source, 'sha256').hexdigest() == row['sha256']:
                _open_file(target)
                return str(target)
    if target.exists():
        target = target.with_name(f'{target.stem}-{secrets.token_hex(6)}{target.suffix}')
    return download(image_id, target, open_file=True)


def connect(host, code):
    if not host or host.startswith('-') or any(c.isspace() for c in host):
        raise ValueError('Use a single SSH host alias')
    details = parse_code(code)
    try:
        active_session()
    except ValueError:
        # Forced closure cannot execute Python cleanup. Remove only credentials
        # owned by this feature, after active_session verified that it is stale.
        try:
            stale = json.loads(session_file().read_text())
            ref = stale.get('credential', '')
            proc = psutil.Process(stale['pid'])
            running = abs(proc.create_time()-stale['created_at']) < .1
        except (OSError, ValueError, KeyError, psutil.Error):
            running = False
        if running:
            raise ValueError('A remote connection process is running but its credential is unavailable; close it first')
        if not running:
            try:
                stale = json.loads(session_file().read_text())
                ref = stale.get('credential', '')
                if ref.startswith('image-cli/remote/'):
                    delete_secret(ref)
                session_file().unlink(missing_ok=True)
            except (OSError, ValueError):
                pass
    else:
        raise ValueError('A remote session is already connected')
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        tunnel_port = sock.getsockname()[1]
    flags = subprocess.CREATE_NO_WINDOW if sys.platform == 'win32' else 0
    tunnel = subprocess.Popen(['ssh', '-N', '-T', '-o', 'BatchMode=yes', '-o', 'ExitOnForwardFailure=yes',
        '-o', 'ServerAliveInterval=5', '-o', 'ServerAliveCountMax=2', '-L',
        f"127.0.0.1:{tunnel_port}:127.0.0.1:{details['port']}", host],
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=flags)
    base = f'http://127.0.0.1:{tunnel_port}'
    authorization = 'Bearer '+details['token']
    reference = 'image-cli/remote/'+secrets.token_hex(16)
    work = queue.PriorityQueue(maxsize=256)
    stopped = threading.Event()
    generation = [0]
    serial = [0]
    cache_lock = threading.Lock()
    transfers = threading.Semaphore(2)
    cache = directory()/'remote-cache'
    cache.mkdir(parents=True, exist_ok=True)

    def upstream(path):
        return urllib.request.urlopen(urllib.request.Request(base+path, headers={'Authorization': authorization}), timeout=30)

    def cache_path(item, kind):
        key = json.dumps([details['catalog_id'], item['sha256'], details.get('preview_version'), kind])
        return cache/(hashlib.sha256(key.encode()).hexdigest()+'.'+('jxl' if kind == 'preview' else 'webp'))

    def cached(path, item, kind):
        target = cache_path(item, kind)
        with cache_lock:
            if target.exists():
                os.utime(target, None)
                return target.read_bytes()
        with transfers:
            with upstream(path) as response:
                if response.headers.get('X-Image-SHA256') != item['sha256']:
                    raise ValueError('Image changed during preview retrieval; repeat the search')
                blob = response.read(16*1024*1024+1)
                if len(blob) > 16*1024*1024:
                    raise ValueError('Cached representation exceeds size limit')
        with cache_lock:
            temp = target.with_suffix('.'+secrets.token_hex(6)+'.partial')
            temp.write_bytes(blob)
            os.replace(temp, target)
            files = sorted(cache.glob('*'), key=lambda p:p.stat().st_mtime)
            total = sum(p.stat().st_size for p in files)
            for old in files:
                if total <= DISK_BUDGET:
                    break
                total -= old.stat().st_size
                old.unlink(missing_ok=True)
        return blob

    def worker():
        while not stopped.is_set():
            try:
                priority, seq, gen, item = work.get(timeout=.2)
            except queue.Empty:
                continue
            try:
                if gen == generation[0] and item.get('has_preview'):
                    cached(f"/v1/images/{item['id']}/preview", item, 'preview')
            except (OSError, ValueError):
                pass
            finally:
                work.task_done()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def authorized(self):
            return (self.headers.get('Host') == f'127.0.0.1:{self.server.server_port}'
                    and not self.headers.get('Origin')
                    and secrets.compare_digest(self.headers.get('Authorization', ''), authorization))

        def do_POST(self):
            if self.authorized() and self.path == '/disconnect':
                stopped.set()
                self.send_response(204); self.end_headers()
                return
            if not self.authorized() or self.path != '/prefetch':
                self.send_error(403)
                return
            try:
                size = int(self.headers.get('Content-Length', 0))
                if not 0 < size <= 1024*1024:
                    raise ValueError()
                items = json.loads(self.rfile.read(size))
                generation[0] += 1
                gen = generation[0]
                while True:
                    try:
                        work.get_nowait(); work.task_done()
                    except queue.Empty:
                        break
                for item in items[:100]:
                    serial[0] += 1
                    work.put_nowait((1, serial[0], gen, item))
                self.send_response(204); self.end_headers()
            except (ValueError, TypeError, KeyError, queue.Full):
                self.send_error(400)

        def do_GET(self):
            if not self.authorized() or not self.path.startswith('/v1/') or '\\' in self.path:
                self.send_error(403)
                return
            try:
                if self.path.endswith(('/preview', '/thumb')):
                    prefix, kind = self.path.rsplit('/', 1)
                    with upstream(prefix) as response:
                        item = json.load(response)
                    from .previews import VERSION
                    item['preview_version'] = VERSION
                    blob = cached(self.path, item, kind)
                    self.send_response(200)
                    self.send_header('Content-Type', 'image/jxl' if kind == 'preview' else 'image/webp')
                    self.send_header('Content-Length', str(len(blob))); self.end_headers()
                    self.wfile.write(blob)
                else:
                    with upstream(self.path) as response:
                        self.send_response(response.status)
                        for header in ['Content-Type', 'Content-Length', 'Content-Disposition', 'X-Image-SHA256']:
                            if header in response.headers:
                                self.send_header(header, response.headers[header])
                        self.end_headers()
                        while chunk := response.read(1024*1024):
                            self.wfile.write(chunk)
            except urllib.error.HTTPError as exc:
                self.send_error(exc.code, 'Remote image unavailable')
            except (OSError, ValueError):
                self.send_error(502, 'Remote session unavailable')

    server = None
    try:
        deadline = time.monotonic()+15
        while True:
            if tunnel.poll() is not None:
                raise ValueError('SSH tunnel failed; check host alias, key access, and port availability')
            try:
                with upstream('/v1/session') as response:
                    health = json.load(response)
                break
            except urllib.error.HTTPError:
                raise ValueError('Remote session rejected the connection code') from None
            except OSError:
                if time.monotonic() > deadline:
                    raise ValueError('Remote server did not become available') from None
                time.sleep(.1)
        if health['catalog_id'] != details['catalog_id']:
            raise ValueError('Remote catalog identity mismatch')
        details['preview_version'] = health['preview_version']
        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        server.daemon_threads = True
        secret(reference, details['token'])
        proc = psutil.Process()
        record = dict(pid=proc.pid, created_at=proc.create_time(), port=server.server_port,
                      credential=reference, catalog_id=details['catalog_id'], name=details['name'])
        session_file().write_text(json.dumps(record))
        for _ in range(2):
            threading.Thread(target=worker, daemon=True).start()
        threading.Thread(target=server.serve_forever, daemon=True).start()
        print(f"Connected to {details['name']} ({health['images']} images). Keep this command open.", flush=True)
        while tunnel.poll() is None and not stopped.is_set():
            time.sleep(1)
            try:
                with upstream('/v1/session'):
                    pass
            except OSError:
                raise ValueError('Remote session closed') from None
    finally:
        stopped.set()
        if server:
            server.shutdown(); server.server_close()
        tunnel.terminate()
        try:
            tunnel.wait(timeout=5)
        except subprocess.TimeoutExpired:
            tunnel.kill(); tunnel.wait()
        delete_secret(reference)
        try:
            if json.loads(session_file().read_text()).get('credential') == reference:
                session_file().unlink(missing_ok=True)
        except (OSError, ValueError):
            pass
