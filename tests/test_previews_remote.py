import hashlib
import io

from fastapi.testclient import TestClient
from PIL import Image

from image_atlas.catalogs import register
from image_atlas.db import connect, normalized_path
from image_atlas.indexer import scan, select_images_for_mine
from image_atlas import previews
from image_atlas.remote_server import create_app, connection_code
from image_atlas.remote_client import parse_code


def test_server_help_does_not_adopt_or_connect_database(monkeypatch):
    from image_atlas import catalogs
    from image_atlas.cli import app
    from typer.testing import CliRunner
    monkeypatch.delenv('IMAGE_CLI_DSN', raising=False)
    monkeypatch.setattr(catalogs, 'register', lambda *a, **kw: (_ for _ in ()).throw(AssertionError('help connected to database')))
    result = CliRunner().invoke(app, ['serve-images','--help'])
    assert result.exit_code == 0, result.output


def test_finished_images_backfill_without_reset_and_second_scan_skips(monkeypatch, tmp_path):
    path = tmp_path/'pancake.png'
    Image.new('RGB', (1500, 1000), 'orange').save(path)
    with connect() as conn:
        scan(conn, str(tmp_path))
        row = conn.execute('SELECT id,sha256 FROM images').fetchone()
        conn.execute("UPDATE images SET embedding_status='done',face_status='done',labels_status='done',rating=4,metadata_version=4")
        conn.execute("INSERT INTO image_tags VALUES(%s,'favorite')", (row['id'],))
        conn.execute('DELETE FROM image_previews'); conn.commit()
        selected, _, _ = select_images_for_mine(conn, tmp_path, (), 1)
        assert selected == [path]
        result = scan(conn, str(tmp_path))
        assert result['previews_created'] == 1
        assert result['unchanged'] == 1
        saved = conn.execute('SELECT * FROM image_previews').fetchone()
        assert (saved['width'], saved['height'], saved['mime_type']) == (1024, 683, 'image/jxl')
        assert previews.decode(bytes(saved['data'])).size == (1024, 683)
        assert conn.execute('SELECT embedding_status,face_status,labels_status,rating,metadata_version FROM images').fetchone() == dict(embedding_status='done', face_status='done', labels_status='done', rating=4, metadata_version=5)
        monkeypatch.setattr(previews, 'encode', lambda p: (_ for _ in ()).throw(AssertionError('reencoded')))
        assert scan(conn, str(tmp_path))['previews_reused'] == 1
        assert conn.execute('SELECT tag FROM image_tags').fetchone()['tag'] == 'favorite'


def test_duplicates_alpha_orientation_and_version_backfill(tmp_path):
    path = tmp_path/'one.png'
    image = Image.new('RGBA', (20, 30), (10, 80, 120, 70))
    exif = Image.Exif(); exif[274] = 6
    image.save(path, exif=exif)
    (tmp_path/'two.png').write_bytes(path.read_bytes())
    with connect() as conn:
        result = scan(conn, str(tmp_path))
        assert (result['previews_created'], result['previews_reused']) == (1, 1)
        row = conn.execute('SELECT * FROM image_previews').fetchone()
        decoded = previews.decode(bytes(row['data']))
        assert decoded.size == (30, 20)
        assert decoded.getpixel((0, 0))[3] == 70
        conn.execute("UPDATE image_previews SET version='old'"); conn.commit()
        assert scan(conn, str(tmp_path))['previews_created'] == 1


def test_failed_encoder_is_incomplete_retryable_and_metadata_committed(monkeypatch, tmp_path):
    path = tmp_path/'test.jpg'
    Image.new('RGB', (12, 8)).save(path)
    def fail(path):
        raise ImportError('encoder unavailable')
    monkeypatch.setattr(previews, 'encode', fail)
    with connect() as conn:
        first = scan(conn, str(tmp_path))
        assert first['new'] == first['previews_failed'] == 1
        assert conn.execute('SELECT status FROM image_previews').fetchone()['status'] == 'error'
        assert select_images_for_mine(conn, tmp_path, (), 1)[0] == [path]


def test_remote_api_auth_search_preview_and_original(monkeypatch, tmp_path):
    from image_atlas.catalogs import resolve
    dsn = resolve().dsn
    record = register('Remote test', dsn, [str(tmp_path)])
    catalog = resolve(record['id'])
    path = tmp_path/'smores-pancake.jpg'
    Image.new('RGB', (240, 160), 'brown').save(path)
    with connect() as conn:
        scan(conn, str(tmp_path))
        row = conn.execute('SELECT id,sha256 FROM images').fetchone()
        conn.execute("INSERT INTO image_tags VALUES(%s,'plated')", (row['id'],)); conn.commit()
    class Encoder:
        device = 'cpu'
        calls = 0
        def text(self, query):
            self.calls += 1
            return [1.0]+[0.0]*511
        def close(self):
            pass
    encoder = Encoder()
    app = create_app(catalog, cpu=True, encoder_factory=lambda **kw: encoder)
    headers = {'Authorization': 'Bearer '+app.state.token, 'host': '127.0.0.1'}
    code = connection_code(18766, app.state.token, catalog)
    assert parse_code(code)['catalog_id'] == catalog.id
    with TestClient(app) as client:
        assert client.get('/v1/session', headers={'host': '127.0.0.1'}).status_code == 401
        assert client.post('/v1/images', headers=headers).status_code == 405
        assert client.get('/v1/session', headers={**headers, 'Origin': 'http://evil.test'}).status_code == 403
        result = client.get('/v1/images', params={'q':'plated smores','mode':'text'}, headers=headers)
        assert result.status_code == 200, result.text
        assert result.json()['items'][0]['id'] == row['id']
        assert client.get('/v1/images', params={'q':'pancake','mode':'combined'}, headers=headers).status_code == 200
        assert client.get('/v1/images', params={'q':'pancake','mode':'visual'}, headers=headers).status_code == 200
        assert encoder.calls == 2
        url = f"/v1/images/{row['id']}"
        assert client.get(url+'/thumb', headers=headers).headers['content-type'] == 'image/webp'
        assert client.get(url+'/preview', headers=headers).headers['content-type'] == 'image/jxl'
        original = client.get(url+'/original', headers=headers)
        assert original.content == path.read_bytes()
        assert hashlib.sha256(original.content).hexdigest() == row['sha256']
        path.unlink()
        assert client.get(url+'/preview', headers=headers).status_code == 200
        assert client.get(url+'/original', headers=headers).status_code == 404
