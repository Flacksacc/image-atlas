import io
import json
from pathlib import Path
import subprocess
import sys

from PIL import Image
from cli_helpers import JsonCliRunner

from image_atlas import embedding_worker, embeddings
from image_atlas.cli import app
from image_atlas.db import connect


def test_image_embedding_cannot_import_database_modules(tmp_path):
    photo=tmp_path/'photo.png'
    Image.new('RGB',(24,20),'red').save(photo)
    script=r'''
import importlib.abc,sys,json
from contextlib import nullcontext
from pathlib import Path
from types import SimpleNamespace
class NoDatabase(importlib.abc.MetaPathFinder):
    def find_spec(self,name,path=None,target=None):
        if name.split('.')[0]=='psycopg' or name in ('image_atlas.db','image_atlas.indexer','image_atlas.labels'):
            raise ModuleNotFoundError('Unexpected database dependency: '+name)
sys.meta_path.insert(0,NoDatabase())
from image_atlas.embeddings import Embedder
class Tensor:
    def unsqueeze(self,*args):return self
    def to(self,*args):return self
    def norm(self,**kwargs):return self
    def __truediv__(self,*args):return self
    def __getitem__(self,*args):return self
    def float(self):return self
    def cpu(self):return self
    def tolist(self):return [1.0]+[0.0]*511
encoder=Embedder.__new__(Embedder)
encoder.device='cpu'
encoder.torch=SimpleNamespace(inference_mode=nullcontext)
encoder.model=SimpleNamespace(encode_image=lambda pixels:Tensor())
def preprocess(image):
    assert image.mode=='RGB' and image.size==(24,20) and image.getpixel((0,0))==(255,0,0)
    return Tensor()
encoder.preprocess=preprocess
assert len(encoder.image(Path(sys.argv[1])))==512
print('Image decoded and embedded without database imports')
'''
    result=subprocess.run([sys.executable,'-c',script,str(photo)],capture_output=True,text=True,
                          creationflags=subprocess.CREATE_NO_WINDOW if sys.platform=='win32' else 0,timeout=15)
    assert result.returncode==0,result.stderr


def test_worker_preflight_and_bad_image_isolation(monkeypatch):
    class FakeEncoder:
        def __init__(self,**kwargs):pass
        def image(self,path):
            if path.name=='bad.jpg':raise ValueError('corrupt photo')
            return [1.0]+[0.0]*511
        def text(self,value):return [1.0]+[0.0]*511
        def close(self):pass
    monkeypatch.setattr(embedding_worker,'local_embedder',FakeEncoder)
    requests=[{'operation':'check','value':''},{'operation':'image','value':'bad.jpg'},
              {'operation':'image','value':'good.jpg'}]
    out=io.StringIO()
    monkeypatch.setattr(embedding_worker.sys,'stdin',io.StringIO(''.join(json.dumps(r)+'\n' for r in requests)))
    monkeypatch.setattr(embedding_worker.sys,'stdout',out)
    embedding_worker.main([])
    replies=[json.loads(line) for line in out.getvalue().splitlines()]
    assert replies[0]['ready'] is True
    assert replies[1]=={'error':'corrupt photo','fatal':False}
    assert len(replies[2]['embedding'])==512


def test_worker_setup_failure_is_reported_once(monkeypatch):
    def broken(**kwargs):raise ModuleNotFoundError("No module named 'psycopg'")
    monkeypatch.setattr(embedding_worker,'local_embedder',broken)
    out=io.StringIO()
    monkeypatch.setattr(embedding_worker.sys,'stdin',io.StringIO((json.dumps({'operation':'check','value':''})+'\n')*10))
    monkeypatch.setattr(embedding_worker.sys,'stdout',out)
    embedding_worker.main([])
    assert [json.loads(line) for line in out.getvalue().splitlines()]==[{'error':"No module named 'psycopg'",'fatal':True}]


def test_late_worker_setup_failure_does_not_poison_images(monkeypatch,tmp_path):
    calls=[]
    class BrokenEncoder:
        def image(self,path):
            calls.append(path)
            raise embeddings.EmbeddingSetupError("No module named 'psycopg'")
        def abort(self):pass
        def close(self):pass
    monkeypatch.setattr(embeddings,'create_embedder',lambda **kwargs:BrokenEncoder())
    for n in range(3):Image.new('RGB',(24+n,20),'red').save(tmp_path/f'{n}.png')
    result=JsonCliRunner().invoke(app,['mine',str(tmp_path),'--tags','embeddings'])
    assert result.exit_code==1,result.output
    data=json.loads(result.stdout)
    assert data['stages']['embeddings']['error']=="No module named 'psycopg'"
    assert len(calls)==1
    with connect() as conn:
        rows=conn.execute('SELECT embedding_status,embedding_error FROM images').fetchall()
    assert len(rows)==3 and all(r=={'embedding_status':'pending','embedding_error':None} for r in rows)


def test_client_checks_startup_and_decodes_unsupported_formats_locally(monkeypatch,tmp_path):
    class Process:
        stdin=io.StringIO()
        stdout=io.StringIO(json.dumps({'ready':True,'decoders':{'heif':False,'raw':False}})+'\n'+json.dumps({'embedding':[1.0]+[0.0]*511})+'\n')
        def poll(self):return None
        def wait(self,**kwargs):return 0
        def kill(self):pass
    process=Process()
    monkeypatch.setattr(embeddings.subprocess,'Popen',lambda *args,**kwargs:process)
    observed=[]
    real_open=embeddings.open_image
    def decode(path):
        observed.append(path)
        return real_open(path)
    monkeypatch.setattr(embeddings,'open_image',decode)
    photo=tmp_path/'local.heic'
    Image.new('RGB',(24,20),'red').save(photo,format='HEIF')
    client=embeddings.EmbeddingWorkerClient(sys.executable)
    assert len(client.image(photo))==512
    requests=[json.loads(line) for line in process.stdin.getvalue().splitlines()]
    assert requests[0]['operation']=='check'
    assert requests[1]['operation']=='image' and requests[1]['value'].endswith('decoded.png')
    assert observed==[photo]
    assert not Path(requests[1]['value']).exists()
    client.close()
