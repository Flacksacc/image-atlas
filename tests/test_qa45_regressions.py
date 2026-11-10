import json
from pathlib import Path
from fastapi.testclient import TestClient
from PIL import Image
from psycopg.conninfo import make_conninfo
from image_atlas import collections
from image_atlas.db import connect, initialize
from image_atlas.indexer import scan
from image_atlas.viewer_connection import normalize
from image_atlas.web import app

def test_gallery_requires_session_and_rejects_rebinding_hosts():
    client = TestClient(app, base_url='http://127.0.0.1')
    assert client.get('/api/catalogs').status_code == 401
    landing = client.get('/')
    assert 'Open the session link' in landing.text
    assert app.state.session_token not in landing.text
    assert client.get('/api/catalogs', headers={'Host': 'untrusted.example'}).status_code == 400
    token = {'X-Image-CLI-Session': app.state.session_token}
    assert client.post('/session', headers=token | {'Origin': 'https://untrusted.example'}).status_code == 403
    assert client.post('/session', headers=token).status_code == 204
    assert client.get('/api/catalogs').status_code == 200
    assert client.cookies.get('image-atlas-session') == app.state.session_token
    assert 'httponly' in client.post('/session', headers=token).headers['set-cookie'].lower()

def test_display_dimensions_and_legacy_person_filter_migrate_once(tmp_path):
    path = tmp_path / 'rotated.jpg'
    exif = Image.Exif(); exif[274] = 6
    Image.new('RGB', (200, 100), 'red').save(path, exif=exif)
    with connect() as conn:
        scan(conn, str(tmp_path))
        image = conn.execute('SELECT width,height,metadata_version FROM images').fetchone()
        assert (image['width'], image['height'], image['metadata_version']) == (100, 200, 5)
        conn.execute('UPDATE images SET width=200,height=100,metadata_version=4')
        person = conn.execute("INSERT INTO people(name) VALUES ('Same') RETURNING id").fetchone()['id']
        state = json.dumps({'filters': [{'field': 'person', 'value': 'Same', 'exclude': False}]})
        conn.execute("INSERT INTO saved_views VALUES ('Legacy',%s::jsonb,now())", (state,))
        conn.commit()
    initialize(); initialize()
    with connect() as conn:
        row = conn.execute('SELECT width,height FROM images').fetchone()
        assert (row['width'], row['height']) == (100, 200)
        chip = conn.execute("SELECT state FROM saved_views WHERE name='Legacy'").fetchone()['state']['filters'][0]
        assert chip == {'field': 'person_id', 'value': str(person), 'exclude': False}
        second = conn.execute("INSERT INTO people(name) VALUES ('Same') RETURNING id").fetchone()['id']
        assert second != person

def test_collection_detail_pages_and_preserves_excluded_provenance(tmp_path):
    with connect() as conn:
        cid = conn.execute("INSERT INTO collections(kind,suggested_name) VALUES ('event','Paged') RETURNING id").fetchone()['id']
        for n in range(205):
            iid = conn.execute('INSERT INTO images(path,root_path,size_bytes,mtime_ns) VALUES (%s,%s,1,1) RETURNING id', (str(tmp_path / f'{n}.jpg'), str(tmp_path))).fetchone()['id']
            conn.execute("INSERT INTO collection_images(collection_id,image_id,source) VALUES (%s,%s,%s)", (cid, iid, 'excluded' if n == 0 else 'manual'))
        first = collections.detail(conn, cid, limit=100)
        second = collections.detail(conn, cid, limit=100, offset=100)
        last = collections.detail(conn, cid, limit=100, offset=200)
        assert len(first['images']) == len(second['images']) == 100
        assert len(last['images']) == 4 and not last['image_more']
        assert first['image_more'] and first['image_count'] == 204
        assert not {x['image_id'] for x in first['images']} & {x['image_id'] for x in second['images']}
        assert len(collections.detail(conn, cid)['images']) == 205
        renamed = collections.review(conn, cid, 'rename', name='Renamed', detail_limit=100)
        assert renamed['name'] == 'Renamed' and len(renamed['images']) == 100

def test_native_ssl_normalization_preserves_verification(tmp_path):
    root = tmp_path / 'root.pem'; root.write_text('certificate fixture', encoding='ascii')
    options = normalize(make_conninfo(host='example.test', dbname='photos', sslmode='verify-full', sslrootcert=str(root)))
    assert options['verify_hostname'] is True
    assert options['root_pem'] == 'certificate fixture'
    assert 'sslmode=require' in options['native_dsn']
    assert normalize('host=example.test sslmode=verify-ca')['verify_hostname'] is False
    system = normalize('host=example.test sslrootcert=system')
    assert system['verify_hostname'] and 'sslmode=require' in system['native_dsn']

def test_registered_legacy_catalog_upgrades_before_viewer_writes(tmp_path):
    from image_atlas import catalogs
    import os
    record = catalogs.register('Legacy upgrade', os.environ['IMAGE_CLI_DSN'], [str(tmp_path)])
    token = catalogs.current.set(catalogs.resolve(record['id']))
    try:
        with connect() as conn:
            conn.execute('DELETE FROM catalog_migrations WHERE version=2')
            conn.execute('ALTER TABLE people ADD CONSTRAINT people_name_key UNIQUE(name)')
            conn.commit()
        initialize()
        with connect() as conn:
            conn.execute("INSERT INTO people(name) VALUES ('Shared'),('Shared')")
            assert conn.execute('SELECT 1 FROM catalog_migrations WHERE version=2').fetchone()
    finally:
        catalogs.current.reset(token)

def test_network_gallery_requires_tls_before_start(monkeypatch):
    from typer.testing import CliRunner
    from image_atlas.cli import app as cli
    import uvicorn
    started = []
    monkeypatch.setattr(uvicorn, 'run', lambda *args, **kwargs: started.append(kwargs))
    result = CliRunner().invoke(cli, ['gallery', '--host', '0.0.0.0'])
    assert result.exit_code != 0 and not started
    assert 'tls-cert' in result.output

def test_readonly_legacy_orientation_is_correct_without_mutation(tmp_path):
    from image_atlas import catalogs
    import os
    path = tmp_path / 'archive.jpg'
    Image.new('RGB', (200, 100), 'blue').save(path)
    record = catalogs.register('Readonly archive', os.environ['IMAGE_CLI_DSN'], [str(tmp_path)])
    token = catalogs.current.set(catalogs.resolve(record['id']))
    try:
        with connect() as conn:
            scan(conn, str(tmp_path))
            conn.execute("UPDATE images SET width=200,height=100,metadata_version=4,exif='{\"Orientation\":6}'::jsonb")
            conn.commit()
        catalogs.edit_roots(record['id'], [])
        catalogs.current.set(catalogs.resolve(record['id']))
        client = TestClient(app, base_url='http://127.0.0.1', headers={'X-Image-CLI-Session': app.state.session_token})
        response = client.get(f"/api/catalogs/{record['id']}/images?group_by=Orientation&sort=Width")
        assert response.status_code == 200, response.text
        row = response.json()[0]
        assert (row['width'], row['height'], row['image_group']) == (100, 200, 'Portrait')
        with connect() as conn:
            original = conn.execute('SELECT width,height,metadata_version FROM images').fetchone()
            assert (original['width'], original['height'], original['metadata_version']) == (200, 100, 4)
    finally:
        catalogs.current.reset(token)
