import json
import time
from pathlib import Path
from types import SimpleNamespace
import numpy as np
import pytest
from PIL import Image
from typer.testing import CliRunner

from image_atlas.cli import app
from image_atlas.db import connect
from image_atlas.face_client import FaceWorkerClient, FaceWorkerError
from image_atlas.face_engine import FaceEngine, MAX_EDGE
from image_atlas.indexer import scan


@pytest.fixture
def fault_worker(tmp_path, monkeypatch):
    (tmp_path / 'fault_face_worker.py').write_text('''
import json, os, sys, time
assert 'IMAGE_CLI_DSN' not in os.environ
assert 'PGPASSWORD' not in os.environ
for line in sys.stdin:
    request=json.loads(line)
    if request['operation']=='check': result={'ready':True}
    elif 'crash' in os.path.basename(request['path']): os._exit(57)
    elif 'hang' in os.path.basename(request['path']): time.sleep(60)
    else: result={'faces':[{'box':[0,0,8,8],'embedding':[1.0]+[0.0]*127,'confidence':0.9}]}
    print(json.dumps(result),flush=True)
''')
    monkeypatch.setenv('PYTHONPATH', str(tmp_path))
    monkeypatch.setenv('PGPASSWORD', 'must-not-reach-worker')
    return 'fault_face_worker'


def test_process_recovers_from_real_worker_exit_and_keeps_next_image(tmp_path, monkeypatch, fault_worker):
    from image_atlas import faces
    for name in ('crash.png', 'good.png'):
        Image.new('RGB', (24,24), 'red').save(tmp_path / name)
    monkeypatch.setattr(faces, 'FaceEngine', lambda: FaceWorkerClient(worker_module=fault_worker))
    with connect() as conn:
        scan(conn, str(tmp_path))
        result = faces.process_pending(conn)
        statuses = {Path(r['path']).name:r for r in conn.execute('SELECT path,face_status,face_error FROM images')}
        assert statuses['crash.png']['face_status'] == 'error'
        assert '0x00000039' in statuses['crash.png']['face_error']
        assert statuses['good.png']['face_status'] == 'done'
        assert result['images'] == 1 and result['faces'] == 1 and result['worker_restarts'] == 1
        assert len(result['errors']) == 1


def test_hung_worker_is_killed_then_next_request_succeeds(fault_worker):
    client = FaceWorkerClient(worker_module=fault_worker, timeout=1)
    process = client.process
    started = time.monotonic()
    try:
        with pytest.raises(FaceWorkerError, match='exceeded'):
            client.detect(Path('hang.png'))
        assert time.monotonic()-started < 5
        assert process.poll() is not None
        assert len(client.detect(Path('good.png'))) == 1
    finally:
        client.close()


def test_worker_cancellation_terminates_child(fault_worker):
    from image_atlas.jobs import MiningCancelled
    cancel = False
    def check():
        if cancel: raise MiningCancelled()
    client = FaceWorkerClient(worker_module=fault_worker, check=check)
    process = client.process
    cancel = True
    with pytest.raises(MiningCancelled):
        client.detect(Path('good.png'))
    assert process.poll() is not None
    assert client.process is None


def test_bounded_inference_scales_face_boxes_to_original_coordinates(tmp_path):
    path = tmp_path/'panorama.jpg'
    Image.new('RGB', (8192, 1024), 'blue').save(path)
    sizes=[]
    detector=SimpleNamespace(setInputSize=lambda size:sizes.append(size),
                             detect=lambda image:(None,np.asarray([[10,20,30,40]+[0]*10+[.9]],dtype=np.float32)))
    recognizer=SimpleNamespace(alignCrop=lambda image,face:image,
                               feature=lambda image:np.asarray([1.0]+[0]*127,dtype=np.float32))
    engine=FaceEngine.__new__(FaceEngine)
    engine.cv2=SimpleNamespace(cvtColor=lambda image,code:image,COLOR_RGB2BGR=0)
    engine.detector=detector
    engine.recognizer=recognizer
    faces=list(engine.detect(path))
    assert sizes == [(MAX_EDGE,256)]
    assert faces[0]['box'] == (40,80,120,160)
    assert len(faces[0]['embedding']) == 128


def test_cli_default_output_is_text_even_for_redirected_output():
    runner = CliRunner()
    result = runner.invoke(app, ['status'])
    assert result.exit_code == 0 and 'Images:' in result.stdout
    with pytest.raises(json.JSONDecodeError): json.loads(result.stdout)
    structured = runner.invoke(app, ['--json','status'])
    assert structured.exit_code == 0 and isinstance(json.loads(structured.stdout),dict)


def test_mine_error_and_cancel_are_text_without_json(tmp_path, monkeypatch):
    from image_atlas import faces
    from image_atlas.catalogs import register, resolve
    register('Test',resolve().dsn,[str(tmp_path)])
    photo=tmp_path/'photo.png'
    Image.new('RGB',(24,24),'red').save(photo)
    class Broken:
        def detect(self,path):raise ValueError('bad image')
    monkeypatch.setattr(faces,'FaceEngine',Broken)
    result=CliRunner().invoke(app,['mine',str(tmp_path),'--tags','faces'])
    assert result.exit_code == 1
    assert 'Completed with errors' in result.stdout and 'bad image' in result.stdout
    assert not result.stdout.lstrip().startswith('{')
    structured=CliRunner().invoke(app,['mine',str(tmp_path),'--tags','metadata','--json'])
    assert structured.exit_code == 0 and 'stages' in json.loads(structured.stdout)


def test_cancelled_face_stage_keeps_counts_and_committed_work(tmp_path, monkeypatch):
    from image_atlas import faces
    from image_atlas.catalogs import register, resolve
    from image_atlas.jobs import MiningCancelled
    register('Test',resolve().dsn,[str(tmp_path)])
    for name in ('first.png','second.png'):
        Image.new('RGB',(24,24),'red').save(tmp_path/name)
    class Engine:
        calls=0
        def detect(self,path):
            self.calls+=1
            if self.calls==2:raise MiningCancelled()
            return []
    monkeypatch.setattr(faces,'FaceEngine',Engine)
    result=CliRunner().invoke(app,['mine',str(tmp_path),'--tags','faces'])
    assert result.exit_code==130 and 'Cancelled:' in result.stdout
    assert 'images=1' in result.stdout and not result.stdout.lstrip().startswith('{')
    with connect() as conn:
        rows=conn.execute('SELECT face_status,count(*) AS n FROM images GROUP BY face_status').fetchall()
        assert {r['face_status']:r['n'] for r in rows}=={'done':1,'pending':1}
