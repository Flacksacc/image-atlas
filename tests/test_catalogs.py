"""Catalog isolation tests only use the dedicated test database and owned new databases."""
import json
from concurrent.futures import ThreadPoolExecutor

import psycopg
import pytest
from fastapi.testclient import TestClient
from PIL import Image
from psycopg import sql
from psycopg.conninfo import conninfo_to_dict, make_conninfo
from cli_helpers import JsonCliRunner

from image_atlas import catalogs
from image_atlas.cli import app as cli
from image_atlas.db import connect
from image_atlas.indexer import scan
from image_atlas.web import app


@pytest.fixture
def pair(tmp_path):
    first = tmp_path / 'a'
    second = tmp_path / 'b'
    first.mkdir()
    second.mkdir()
    initial = catalogs.resolve().dsn
    a = catalogs.register('Alpha', initial, [str(first)])
    b = catalogs.create('Beta', [str(second)])
    created = [catalogs.resolve(b['id']).dsn]
    try:
        yield a, b, first, second, created
    finally:
        catalogs.current.set(None)
        for dsn in created:
            settings = conninfo_to_dict(dsn)
            database = settings['dbname']
            assert database.startswith(('imagecli_', 'imagecli_restore_')) and database != 'imagecli_test'
            settings['dbname'] = 'postgres'
            with psycopg.connect(make_conninfo(**settings), autocommit=True) as conn:
                conn.execute(sql.SQL('DROP DATABASE {}').format(sql.Identifier(database)))


def seed(identifier, root, color):
    token = catalogs.current.set(catalogs.resolve(identifier))
    try:
        Image.new('RGB', (28, 24), color).save(root / 'same.png')
        with connect() as conn:
            scan(conn, str(root))
            conn.execute('INSERT INTO people(name,ignored) VALUES (%s,false)', (color,))
    finally:
        catalogs.current.reset(token)


def test_catalog_ids_namespace_images_people_and_requests(pair):
    a, b, first, second, _ = pair
    seed(a['id'], first, 'red')
    seed(b['id'], second, 'blue')
    client = TestClient(app, base_url="http://127.0.0.1", headers={"X-Image-CLI-Session": app.state.session_token})
    def read(record):
        base = '/api/catalogs/' + record['id']
        images = client.get(base + '/images').json()
        people = client.get(base + '/people').json()
        thumb = client.get(base + '/images/1/thumb')
        return images, people, thumb
    with ThreadPoolExecutor(max_workers=2) as pool:
        alpha, beta = list(pool.map(read, [a, b]))
    assert alpha[0][0]['id'] == beta[0][0]['id'] == 1
    assert alpha[0][0]['path'] != beta[0][0]['path']
    assert alpha[1][0]['name'] == 'red' and beta[1][0]['name'] == 'blue'
    assert alpha[2].content != beta[2].content
    assert client.get('/api/images').status_code == 400
    assert client.get('/api/catalogs/not-a-catalog/images').status_code == 400
    assert client.get('/api/catalogs').json()['default'] == a['id']


def test_roots_overlap_boundary_alias_and_jobs(pair, tmp_path):
    a, b, first, second, _ = pair
    with pytest.raises(ValueError, match='overlaps'):
        catalogs.edit_roots(b['id'], [str(first / 'nested')])
    with pytest.raises(ValueError, match='overlaps'):
        catalogs.edit_roots(b['id'], [str(tmp_path)])
    catalog = catalogs.resolve(a['id'])
    catalog.require_path(first / 'photo.png')
    with pytest.raises(ValueError, match='outside'):
        catalog.require_path(tmp_path / 'ab' / 'photo.png')
    from image_atlas.jobs import MineJob, cancel_job
    token = catalogs.current.set(catalog)
    job = MineJob('testing')
    try:
        with pytest.raises(ValueError, match='running jobs'):
            catalogs.edit_roots(a['id'], [])
        catalogs.current.set(catalogs.resolve(b['id']))
        with pytest.raises(ValueError, match='No matching'):
            cancel_job(job.id)
    finally:
        job.close()
        catalogs.current.reset(token)
    catalogs.edit_roots(a['id'], [])
    with pytest.raises(ValueError, match='read-only'):
        catalog.require_path(first / 'photo.png')


def test_cli_pins_catalog_and_rejects_before_file_reads(pair, monkeypatch):
    a, b, first, second, _ = pair
    seed(a['id'], first, 'red')
    seed(b['id'], second, 'blue')
    runner = JsonCliRunner()
    catalogs.use(b['id'])
    # Environment overrides the saved default; an explicit selection wins both.
    assert catalogs.resolve().id == a['id']
    result = runner.invoke(cli, ['--database', b['id'], 'search', 'same'])
    assert result.exit_code == 0
    assert json.loads(result.stdout)[0]['path'].lower().startswith(str(second).lower())
    monkeypatch.setattr(Image, 'open', lambda *a, **k: pytest.fail('opened foreign image'))
    result = runner.invoke(cli, ['--database', a['id'], 'mine', str(second), '--tags', 'metadata'])
    assert result.exit_code != 0 and 'outside' in str(result.exception)


def test_backup_restore_preserves_data_identity_and_readonly(pair, tmp_path):
    from image_atlas.catalog_backup import backup, restore
    a, b, first, second, created = pair
    seed(a['id'], first, 'red')
    token = catalogs.current.set(catalogs.resolve(a['id']))
    try:
        with connect() as conn:
            conn.execute('UPDATE people SET ignored=true')
            conn.execute("INSERT INTO image_tags(image_id,tag) VALUES (1,'keep')")
            conn.execute("INSERT INTO saved_views(name,state) VALUES ('saved','{}')")
            conn.execute("UPDATE images SET embedding=%s::halfvec,embedding_status='done' WHERE id=1", ('[1,' + ','.join(['0'] * 511) + ']',))
            conn.execute('INSERT INTO face_groups DEFAULT VALUES')
            conn.execute("INSERT INTO faces(image_id,x,y,width,height,embedding,group_id,person_id) VALUES (1,0,0,10,10,%s::halfvec,1,1)", ('[1,' + ','.join(['0'] * 127) + ']',))
            conn.execute("INSERT INTO sort_proposals(image_id,destination,reason,status) VALUES (1,'review-only','keep','approved')")
            conn.execute("INSERT INTO collections(kind,suggested_name,user_name,status,locked) VALUES ('event','Suggested','Manual name','dismissed',true)")
            conn.execute("INSERT INTO collection_images(collection_id,image_id,source) VALUES (1,1,'manual')")
        result = backup(tmp_path / 'backups')
        manifest = json.loads(__import__('pathlib').Path(result['manifest']).read_text())
        assert 'password' not in json.dumps(manifest) and 'connection' not in manifest
        restored = restore(result['archive'], 'Restored')
        restored_context = catalogs.resolve(restored['id'])
        created.append(restored_context.dsn)
        assert restored['id'] != a['id'] and restored['readonly'] and restored['roots'] == []
        catalogs.current.set(restored_context)
        with connect() as conn:
            assert conn.execute('SELECT count(*) AS n FROM images').fetchone()['n'] == 1
            assert conn.execute('SELECT ignored FROM people').fetchone()['ignored']
            assert conn.execute('SELECT tag FROM image_tags').fetchone()['tag'] == 'keep'
            assert conn.execute('SELECT count(*) AS n FROM thumbnails').fetchone()['n'] == 1
            assert conn.execute('SELECT name FROM saved_views').fetchone()['name'] == 'saved'
            assert conn.execute('SELECT embedding IS NOT NULL AS present FROM images').fetchone()['present']
            assert conn.execute('SELECT person_id FROM faces').fetchone()['person_id'] == 1
            assert conn.execute('SELECT status FROM sort_proposals').fetchone()['status'] == 'approved'
            collection = conn.execute('SELECT user_name,status,locked FROM collections').fetchone()
            assert collection == {'user_name': 'Manual name', 'status': 'dismissed', 'locked': True}
            assert conn.execute('SELECT source FROM collection_images').fetchone()['source'] == 'manual'
            with pytest.raises(psycopg.errors.ReadOnlySqlTransaction):
                conn.execute("UPDATE people SET name='wrong'")
            conn.rollback()
        with pytest.raises(ValueError, match='overlaps'):
            catalogs.edit_roots(restored['id'], [str(first)])
        catalogs.edit_roots(a['id'], [])
        catalogs.edit_roots(restored['id'], [str(first)])
        catalogs.current.set(catalogs.resolve(restored['id']))
        with connect() as conn:
            assert conn.execute("INSERT INTO people(name) VALUES ('next') RETURNING id").fetchone()['id'] == 2
    finally:
        catalogs.current.reset(token)


def test_corrupt_backup_rejected_without_new_catalog(pair, tmp_path):
    from image_atlas.catalog_backup import restore
    archive = tmp_path / 'broken.dump'
    archive.write_bytes(b'broken')
    archive.with_suffix('.json').write_text(json.dumps({'version': 1, 'schema_version': 1, 'sha256': 'wrong'}))
    before = catalogs.registry()
    with pytest.raises(ValueError, match='checksum'):
        restore(archive, 'Broken')
    assert catalogs.registry() == before


def test_unregistered_environment_cannot_mine(tmp_path):
    runner = JsonCliRunner()
    result = runner.invoke(cli, ['mine', str(tmp_path), '--tags', 'metadata'])
    assert result.exit_code != 0 and 'Register' in str(result.exception)


def test_identity_mismatch_fails_closed(pair):
    a, b, first, second, _ = pair
    context = catalogs.resolve(a['id'])
    from dataclasses import replace
    token = catalogs.current.set(replace(context, dsn=catalogs.resolve(b['id']).dsn))
    try:
        with pytest.raises(ValueError, match='identity'):
            connect()
    finally:
        catalogs.current.reset(token)


def test_everything_cannot_widen_ownership(pair):
    from image_atlas.everything import allowed_image, image_query
    a, b, first, second, _ = pair
    token = catalogs.current.set(catalogs.resolve(a['id']))
    try:
        assert allowed_image(first / 'own.jpg', include_system=True)
        assert not allowed_image(second / 'foreign.jpg', include_system=True)
        assert str(first).lower() in image_query(include_system=True)
    finally:
        catalogs.current.reset(token)


def test_cancelled_backup_is_not_published_and_job_is_removed(pair, tmp_path, monkeypatch):
    from image_atlas import catalog_backup
    from image_atlas.jobs import MineJob, MiningCancelled, active_jobs
    a, *_ = pair
    token = catalogs.current.set(catalogs.resolve(a['id']))
    class Process:
        done = False
        def poll(self): return 0 if self.done else None
        def terminate(self): self.done = True
        def wait(self, timeout=None): return 0
    def spawn(args, **kwargs):
        assert 'IMAGE_CLI_DSN' not in kwargs['env']
        kwargs['stdout'].write(b'incomplete archive')
        return Process()
    def cancel(self): raise MiningCancelled()
    monkeypatch.setattr(catalog_backup.subprocess, 'Popen', spawn)
    monkeypatch.setattr(catalog_backup, 'command', lambda *a: (['pg_dump'], {}))
    monkeypatch.setattr(MineJob, 'check', cancel)
    try:
        with pytest.raises(MiningCancelled):
            catalog_backup.backup(tmp_path / 'cancelled')
        assert list((tmp_path / 'cancelled').iterdir()) == []
        assert active_jobs() == []
    finally:
        catalogs.current.reset(token)


def test_junction_cannot_cross_catalogs(pair):
    import os
    import subprocess
    if os.name != 'nt':
        pytest.skip('Windows junction check')
    a, b, first, second, _ = pair
    link = first / 'foreign-junction'
    result = subprocess.run(['cmd.exe', '/c', 'mklink', '/J', str(link), str(second)], capture_output=True,
                            creationflags=subprocess.CREATE_NO_WINDOW)
    assert result.returncode == 0, result.stderr.decode(errors='replace')
    with pytest.raises(ValueError, match='outside'):
        catalogs.resolve(a['id']).require_path(link / 'image.png')
    with pytest.raises(ValueError, match='overlaps'):
        catalogs.edit_roots(a['id'], [str(link)])


def test_missing_backup_tool_leaves_no_partial_files(pair, tmp_path, monkeypatch):
    from image_atlas import catalog_backup
    a, *_ = pair
    token = catalogs.current.set(catalogs.resolve(a['id']))
    def missing(*args): raise FileNotFoundError('PostgreSQL tools unavailable')
    monkeypatch.setattr(catalog_backup, 'command', missing)
    try:
        with pytest.raises(FileNotFoundError):
            catalog_backup.backup(tmp_path / 'missing')
        assert list((tmp_path / 'missing').iterdir()) == []
    finally:
        catalogs.current.reset(token)


def test_invalid_archive_cleans_only_its_new_restore_database(pair, tmp_path, monkeypatch):
    import uuid
    from image_atlas import catalog_backup
    a, *_ = pair
    archive = tmp_path / 'invalid.dump'
    archive.write_bytes(b'not a PostgreSQL archive')
    archive.with_suffix('.json').write_text(json.dumps({'version': 1, 'schema_version': 1, 'catalog_id': a['id'],
                                                     'sha256': catalog_backup.digest(archive)}))
    identifier = uuid.uuid4()
    target = 'imagecli_restore_' + identifier.hex[:16]
    monkeypatch.setattr(catalog_backup.uuid, 'uuid4', lambda: identifier)
    before = catalogs.registry()
    with pytest.raises(ValueError, match='pg_restore failed'):
        catalog_backup.restore(archive, 'Invalid')
    assert catalogs.registry() == before
    with connect() as conn:
        assert conn.execute('SELECT 1 FROM pg_database WHERE datname=%s', (target,)).fetchone() is None


def test_gallery_flag_sets_initial_tab_catalog(pair, monkeypatch):
    import uvicorn
    a, b, *_ = pair
    calls = []
    previous = getattr(app.state, 'initial_catalog_id', None)
    monkeypatch.setattr(uvicorn, 'run', lambda *args, **kwargs: calls.append(args[0]))
    try:
        result = JsonCliRunner().invoke(cli, ['--database', b['id'], 'gallery'])
        assert result.exit_code == 0, result.output
        assert calls == [app] and app.state.initial_catalog_id == b['id']
        assert b['id'] in TestClient(app, base_url="http://127.0.0.1", headers={"X-Image-CLI-Session": app.state.session_token}).get('/').text
        assert catalogs.registry()['default'] == a['id']
    finally:
        app.state.initial_catalog_id = previous
