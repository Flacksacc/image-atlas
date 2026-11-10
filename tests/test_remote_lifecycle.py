import json
import os
import subprocess
import sys
import threading
import time
import urllib.request

import pytest
from PIL import Image

from image_atlas.catalogs import register, resolve
from image_atlas.db import connect
from image_atlas.indexer import scan
from image_atlas.remote_server import create_app, connection_code


def test_broker_prefetch_download_and_disconnect(monkeypatch, tmp_path):
    """Exercise real HTTP and credential storage; substitute only the SSH transport."""
    import socket
    import uvicorn
    from image_atlas import remote_client as client
    record = register('Broker test', resolve().dsn, [str(tmp_path)])
    catalog = resolve(record['id'])
    photo = tmp_path/'smores photo.jpg'
    Image.new('RGB', (600, 400), 'orange').save(photo)
    with connect() as conn:
        scan(conn, str(tmp_path))
        ident = conn.execute('SELECT id FROM images').fetchone()['id']
    app = create_app(catalog, cpu=True)
    sock = socket.socket(); sock.bind(('127.0.0.1', 0))
    source_port = sock.getsockname()[1]
    server = uvicorn.Server(uvicorn.Config(app, log_level='error'))
    serving = threading.Thread(target=server.run, kwargs={'sockets':[sock]}, daemon=True)
    serving.start()
    tunnel_port = []
    class Tunnel:
        def __init__(self, args, **kw):
            tunnel_port.append(int(args[args.index('-L')+1].split(':')[1]))
        def poll(self): return None
        def terminate(self): pass
        def wait(self, timeout=None): return 0
    original_open = urllib.request.urlopen
    def transport(request, **kw):
        url = request.full_url if isinstance(request, urllib.request.Request) else request
        if tunnel_port and url.startswith(f'http://127.0.0.1:{tunnel_port[0]}/'):
            url = url.replace(f':{tunnel_port[0]}/', f':{source_port}/', 1)
            request = urllib.request.Request(url, data=request.data, headers=dict(request.header_items()))
        return original_open(request, **kw)
    monkeypatch.setattr(client.subprocess, 'Popen', Tunnel)
    monkeypatch.setattr(client.urllib.request, 'urlopen', transport)
    errors = []
    def run():
        try: client.connect('test-host', connection_code(source_port, app.state.token, catalog))
        except Exception as exc: errors.append(exc)
    broker = threading.Thread(target=run, daemon=True); broker.start()
    try:
        deadline = time.monotonic()+10
        while not client.session_file().exists() and time.monotonic()<deadline:
            time.sleep(.05)
        assert client.session_file().exists(), errors
        result = client.search('smores', mode='text')
        assert result['items'][0]['id'] == ident
        target = tmp_path/'download.jpg'
        assert client.download(ident, target) == str(target)
        assert target.read_bytes() == photo.read_bytes()
        with pytest.raises(FileExistsError): client.download(ident, target)
        preview = tmp_path/'preview.jxl'
        client.download(ident, preview, kind='preview')
        assert preview.stat().st_size > 0
        with client.request('/disconnect', data=b'{}'): pass
        broker.join(10)
        assert not broker.is_alive()
        assert not client.session_file().exists()
        assert not errors
    finally:
        server.should_exit = True
        serving.join(10);sock.close()


@pytest.mark.skipif(os.name != 'nt', reason='Windows process containment')
def test_forced_foreground_exit_kills_child(tmp_path):
    import psutil
    record = tmp_path/'child.json'
    code = '''from image_atlas.process_scope import process_scope
import subprocess,sys,time,json
from pathlib import Path
with process_scope():
 p=subprocess.Popen([sys.executable,'-c','import time;time.sleep(90)'],creationflags=subprocess.CREATE_NO_WINDOW)
 Path(sys.argv[1]).write_text(json.dumps({'pid':p.pid}))
 time.sleep(90)
'''
    parent = subprocess.Popen([sys.executable, '-c', code, str(record)], creationflags=subprocess.CREATE_NO_WINDOW)
    child = None
    try:
        deadline=time.monotonic()+10
        while not record.exists() and time.monotonic()<deadline:time.sleep(.05)
        child=psutil.Process(json.loads(record.read_text())['pid'])
        parent.kill(); parent.wait(timeout=5)
        _,alive=psutil.wait_procs([child],timeout=5)
        assert not alive
    finally:
        if parent.poll() is None:parent.kill();parent.wait()
        if child and child.is_running():child.kill()
