"""Regression coverage for preview integrity and offline viewer startup."""
import hashlib
import json
from types import SimpleNamespace
from unittest.mock import Mock

import psycopg
import pytest
from PIL import Image
from typer.testing import CliRunner

from image_atlas import catalogs, cli, previews, worker_env


def source_hash(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def preview_store(path, sha):
    """Capture preview publication while substituting only database I/O."""
    conn = Mock()
    saved = {}

    def execute(sql, params=None):
        if 'SELECT DISTINCT ON' in sql:
            return SimpleNamespace(fetchall=lambda: [{'path': str(path), 'sha256': sha}])
        if 'SELECT status' in sql:
            return SimpleNamespace(fetchone=lambda: saved.get(params[0]))
        if 'INSERT INTO image_previews' in sql:
            saved[params[0]] = dict(zip(
                ('sha256', 'version', 'status', 'width', 'height', 'data', 'error'), params))
            return None
        if 'pg_total_relation_size' in sql:
            return SimpleNamespace(fetchone=lambda: {'payload_bytes': 0, 'database_bytes': 0})
        raise AssertionError(f'Unexpected query: {sql}')

    conn.execute.side_effect = execute
    return conn, saved


def test_backfill_refuses_changed_source_and_can_retry_after_rescan(tmp_path, monkeypatch):
    path = tmp_path / 'photo.png'
    Image.new('RGB', (32, 24), 'red').save(path)
    indexed_sha = source_hash(path)
    Image.new('RGB', (32, 24), 'blue').save(path)
    catalog = catalogs.Catalog('fixture', 'Test', '', (str(tmp_path),))
    monkeypatch.setattr(catalogs, 'resolve', lambda: catalog)
    monkeypatch.setattr(catalogs, 'registry', lambda: {'catalogs': [
        {'id': catalog.id, 'roots': list(catalog.roots), 'readonly': False}]})
    conn, saved = preview_store(path, indexed_sha)

    encode = Mock(wraps=previews.encode)
    monkeypatch.setattr(previews, 'encode', encode)
    result = previews.build(conn)
    assert result['previews_failed'] == 1
    assert result['previews_created'] == 0
    assert saved[indexed_sha]['status'] == 'error'
    assert saved[indexed_sha]['data'] is None
    assert 'rescan' in saved[indexed_sha]['error']
    encode.assert_not_called()

    # A rescan associates the source with its new hash; the retry must succeed.
    current_sha = source_hash(path)
    counts = previews.counters()
    previews.ensure(conn, current_sha, path, counts)
    assert counts['previews_created'] == 1
    assert saved[indexed_sha]['status'] == 'error'
    assert saved[current_sha]['status'] == 'done'
    assert previews.decode(saved[current_sha]['data']).getpixel((0, 0))[2] > 240


def test_source_change_during_encoding_discards_encoded_bytes(tmp_path, monkeypatch):
    path = tmp_path / 'photo.png'
    Image.new('RGB', (32, 24), 'red').save(path)
    sha = source_hash(path)
    conn, saved = preview_store(path, sha)
    encode = previews.encode

    def changing_source(source):
        result = encode(source)
        Image.new('RGB', (32, 24), 'blue').save(source)
        return result

    monkeypatch.setattr(previews, 'encode', changing_source)
    counts = previews.counters()
    previews.ensure(conn, sha, path, counts)
    assert counts['previews_failed'] == 1
    assert saved[sha]['status'] == 'error'
    assert saved[sha]['data'] is None
    assert saved[sha]['width'] is None
    assert saved[sha]['height'] is None


def test_completed_preview_remains_reusable_without_original(tmp_path, monkeypatch):
    path = tmp_path / 'photo.png'
    Image.new('RGB', (32, 24), 'red').save(path)
    sha = source_hash(path)
    conn, saved = preview_store(path, sha)
    counts = previews.counters()
    previews.ensure(conn, sha, path, counts)
    assert saved[sha]['status'] == 'done'
    path.unlink()
    encode = Mock(side_effect=AssertionError('Cached previews must not open originals'))
    monkeypatch.setattr(previews, 'encode', encode)
    previews.ensure(conn, sha, path, counts)
    assert counts['previews_created'] == counts['previews_reused'] == 1
    encode.assert_not_called()


@pytest.mark.parametrize('offline', [False, True])
def test_view_initializes_once_and_launches_even_when_database_is_offline(tmp_path, monkeypatch, offline):
    binary = tmp_path / 'image-atlas-viewer.exe'
    binary.write_bytes(b'fixture')
    monkeypatch.setenv('IMAGE_CLI_VIEWER_BIN', str(binary))
    catalog = catalogs.Catalog('fixture', 'Photos', 'dbname=offline')
    monkeypatch.setattr(catalogs, 'resolve', lambda selector=None: catalog)
    monkeypatch.setattr(catalogs, 'registry', lambda: {'catalogs': [{'id': 'fixture'}]})
    monkeypatch.setattr(worker_env, 'model_python', lambda *args: None)
    initialize = Mock(side_effect=psycopg.OperationalError('offline') if offline else None)
    launch = Mock()
    monkeypatch.setattr(cli, 'initialize', initialize)
    monkeypatch.setattr(cli.subprocess, 'Popen', launch)

    result = CliRunner().invoke(cli.app, ['--json', 'view'])
    assert result.exit_code == 0, result.output
    assert json.loads(result.output)['started'] is True
    initialize.assert_called_once_with()
    launch.assert_called_once()
    assert launch.call_args.args[0] == [str(binary)]
    assert launch.call_args.kwargs['env']['IMAGE_CLI_CATALOG_ID'] == 'fixture'
