import threading

import pytest
from PIL import Image

from cli_helpers import JsonCliRunner
from image_atlas import catalogs, indexer, previews, scan_workers
from image_atlas.cli import app
from image_atlas.db import connect
from image_atlas.jobs import MiningCancelled


class CoordinatorConnection:
    def __init__(self, conn):
        self.conn, self.thread = conn, threading.get_ident()

    def execute(self, *args, **kwargs):
        assert threading.get_ident() == self.thread, 'A worker touched PostgreSQL'
        return self.conn.execute(*args, **kwargs)

    def commit(self):
        assert threading.get_ident() == self.thread
        self.conn.commit()

    def rollback(self):
        assert threading.get_ident() == self.thread
        self.conn.rollback()


def photos(folder, count):
    for number in range(count):
        Image.new('RGB', (32+number, 24), (number*30 % 255, 0, 100)).save(folder / f'{number}.png')


def test_default_pool_prepares_two_files_together_copies_catalog_context_and_serializes_writes(tmp_path, monkeypatch):
    record = catalogs.register('Concurrent', catalogs.resolve().dsn, [str(tmp_path)])
    token = catalogs.current.set(catalogs.resolve(record['id']))
    photos(tmp_path, 2)
    barrier = threading.Barrier(2)
    threads = set()
    real_metadata = indexer.metadata
    def metadata(path, **kwargs):
        assert catalogs.resolve().id == record['id']
        threads.add(threading.get_ident())
        barrier.wait(timeout=5)
        return real_metadata(path, **kwargs)
    monkeypatch.setattr(indexer, 'metadata', metadata)
    try:
        with connect() as conn:
            bound = CoordinatorConnection(conn)
            result = indexer.scan(bound, str(tmp_path))
            assert result['workers'] == 2 and result['new'] == 2 and not result['errors']
            assert len(threads) == 2 and bound.thread not in threads
            assert conn.execute('SELECT count(*) n FROM image_previews WHERE status=\'done\'').fetchone()['n'] == 2
            # Fully cached files must not be decoded or encoded by the pool again.
            monkeypatch.setattr(indexer, 'metadata', lambda *a, **k: pytest.fail('Unchanged metadata decoded again'))
            monkeypatch.setattr(previews, 'encode', lambda *a, **k: pytest.fail('Cached preview encoded again'))
            result = indexer.scan(bound, str(tmp_path))
            assert result['unchanged'] == result['previews_reused'] == 2
    finally:
        catalogs.current.reset(token)


def test_duplicate_previews_are_published_once_and_bad_files_do_not_stop_other_workers(tmp_path):
    photos(tmp_path, 1)
    (tmp_path / 'duplicate.png').write_bytes((tmp_path / '0.png').read_bytes())
    (tmp_path / 'broken.png').write_text('broken image')
    with connect() as conn:
        result = indexer.scan(CoordinatorConnection(conn), str(tmp_path), workers=2)
        assert result['new'] == 2 and len(result['errors']) == 1
        assert result['thumbnails_created'] == result['previews_created'] == result['previews_reused'] == 1
        assert conn.execute('SELECT count(*) n FROM images').fetchone()['n'] == 2
        assert conn.execute('SELECT count(*) n FROM image_previews').fetchone()['n'] == 1


def test_source_change_before_publication_cannot_overwrite_a_catalog_entry(tmp_path, monkeypatch):
    photos(tmp_path, 1)
    path = tmp_path / '0.png'
    real_prepare = scan_workers._prepare
    def changed(*args, **kwargs):
        result = real_prepare(*args, **kwargs)
        Image.new('RGB', (49, 25), 'blue').save(path)
        return result
    with connect() as conn:
        indexer.scan(conn, str(tmp_path), workers=1)
        old = conn.execute('SELECT id,sha256 FROM images').fetchone()
        conn.execute('UPDATE images SET favorite=true WHERE id=%s', (old['id'],))
        conn.commit()
        Image.new('RGB', (43, 25), 'red').save(path)
        monkeypatch.setattr(scan_workers, '_prepare', changed)
        result = indexer.scan(conn, str(tmp_path))
        assert result['new'] == result['updated'] == 0 and len(result['errors']) == 1
        assert 'changed' in result['errors'][0]['error']
        saved = conn.execute('SELECT id,sha256,favorite FROM images').fetchall()
        assert saved == [dict(old, favorite=True)]
        assert conn.execute('SELECT count(*) n FROM image_previews').fetchone()['n'] == 1


def test_limit_does_not_prepare_extra_files_and_one_worker_is_sequential(tmp_path, monkeypatch):
    photos(tmp_path, 6)
    real_metadata = indexer.metadata
    calls = []
    def metadata(path, **kwargs):
        calls.append((path, threading.get_ident()))
        return real_metadata(path, **kwargs)
    monkeypatch.setattr(indexer, 'metadata', metadata)
    with connect() as conn:
        result = indexer.scan(conn, str(tmp_path), workers=2, limit=2)
        assert result['new'] == len(calls) == 2
        calls.clear()
        result = indexer.scan(conn, str(tmp_path), workers=1)
        assert result['new'] == 4 and result['unchanged'] == 2
        assert len(calls) == 4 and all(thread == threading.get_ident() for _, thread in calls)


def test_cancellation_keeps_committed_image_and_closes_bounded_workers(tmp_path, monkeypatch):
    photos(tmp_path, 10)
    calls=[]
    real_metadata=indexer.metadata
    def metadata(path, **kwargs):
        calls.append(path)
        return real_metadata(path, **kwargs)
    monkeypatch.setattr(indexer, 'metadata', metadata)
    def cancel():
        raise MiningCancelled()
    with connect() as conn:
        with pytest.raises(MiningCancelled):
            indexer.scan(conn, str(tmp_path), workers=2, progress=cancel)
        assert conn.execute('SELECT count(*) n FROM images').fetchone()['n'] == 1
        assert conn.execute("SELECT count(*) n FROM image_previews WHERE status='done'").fetchone()['n'] == 1
    assert not any(thread.name.startswith('image-atlas-scan') for thread in threading.enumerate())
    assert len(calls) <= 4


@pytest.mark.parametrize('workers', ['0', '9'])
def test_cli_rejects_unbounded_worker_counts(tmp_path, workers):
    result = JsonCliRunner().invoke(app, ['mine', str(tmp_path), '--workers', workers])
    assert result.exit_code == 2


@pytest.mark.parametrize('workers', [1, 4])
def test_cli_honors_explicit_worker_count(tmp_path, workers):
    import json
    photos(tmp_path, 2)
    record=catalogs.register('Worker selection', catalogs.resolve().dsn, [str(tmp_path)])
    result=JsonCliRunner().invoke(app,['--database',record['id'],'mine',str(tmp_path),'--tags','metadata','--workers',str(workers)])
    assert result.exit_code == 0, result.output
    metadata=json.loads(result.stdout)['stages']['metadata']
    assert metadata['workers'] == workers and metadata['new'] == 2
