"""Database display edits preserve identity, credentials, folders and contents."""
import json
import os
import subprocess
import sys

import psycopg
import pytest

from image_atlas import catalogs, viewer_catalogs as service


def asset_target(dsn='host=archive dbname=assets user=viewer password=private-test-password'):
    return {'kind': 'assets', 'id': None, 'dsn': dsn, 'name': 'Asset library'}


def fields(name='My assets'):
    return {'name': name, 'description': 'Creative library', 'notes': 'Stored on this workstation'}


def test_asset_metadata_is_per_database_and_never_stores_connections(monkeypatch):
    monkeypatch.setattr(psycopg, 'connect', lambda *a, **k: pytest.fail('Metadata edits must work offline'))
    target = asset_target()
    result = service.request({'action': 'save', 'target': target, 'fields': fields()})
    assert result['metadata']['name'] == 'My assets'
    assert service.metadata(target)['notes'] == fields()['notes']
    assert service.metadata(asset_target(target['dsn'].replace('private-test-password', 'rotated-secret')))['name'] == 'My assets'
    assert service.metadata(asset_target(target['dsn'].replace('dbname=assets', 'dbname=other')))['name'] == 'Asset library'
    saved = service.metadata_path().read_text(encoding='utf-8')
    for secret in ['private-test-password', 'rotated-secret', 'host=archive', target['dsn']]:
        assert secret not in saved


def test_image_rename_preserves_identity_default_roots_and_credentials(tmp_path, monkeypatch):
    record = catalogs.register('Original', catalogs.resolve().dsn, [str(tmp_path)])
    before = catalogs.registry()
    original = next(r for r in before['catalogs'] if r['id'] == record['id'])
    target = {'kind': 'images', 'id': record['id'], 'dsn': 'unused while editing', 'name': 'Original'}
    monkeypatch.setattr(psycopg, 'connect', lambda *a, **k: pytest.fail('Rename must not reconnect or rename a physical database'))
    service.save_metadata(target, fields('Renamed'))
    after = catalogs.registry()
    current = next(r for r in after['catalogs'] if r['id'] == record['id'])
    assert after['default'] == before['default']
    for key in original:
        if key not in {'name', 'description', 'notes', 'metadata_updated_at'}:
            assert current[key] == original[key]
    assert catalogs.resolve('Renamed').id == record['id']
    assert service.metadata(target)['description'] == 'Creative library'
    after['catalogs'].append({**current, 'id': 'another-fixture', 'name': 'Duplicate'})
    catalogs.save(after)
    with pytest.raises(ValueError, match='already has'):
        service.save_metadata(target, fields('duplicate'))
    assert catalogs.registry()['catalogs'][0]['name'] == 'Renamed'


@pytest.mark.parametrize('value', ['', 'x' * 121, 'invalid\nname', 42])
def test_invalid_names_leave_existing_metadata_unchanged(value):
    target = asset_target()
    service.save_metadata(target, fields())
    before = service.metadata_path().read_bytes()
    with pytest.raises(ValueError):
        service.save_metadata(target, {**fields(), 'name': value})
    assert service.metadata_path().read_bytes() == before


def test_unreadable_metadata_is_not_overwritten():
    path = service.metadata_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text('{broken', encoding='utf-8')
    with pytest.raises(ValueError):
        service.save_metadata(asset_target(), fields())
    assert path.read_text() == '{broken'


def test_offline_details_keep_metadata_editable_and_hide_connection_errors(monkeypatch):
    target = asset_target()
    service.save_metadata(target, fields())
    def fail(_):
        raise psycopg.OperationalError('private-test-password')
    monkeypatch.setattr(service, 'details', fail)
    result = service.request({'action': 'load', 'target': target})
    assert result['metadata']['name'] == 'My assets'
    assert 'details_error' in result
    assert 'private-test-password' not in json.dumps(result)
    assert service.request({'action': 'save', 'target': target, 'fields': fields('Offline rename')})['metadata']['name'] == 'Offline rename'


def test_image_storage_inspection_uses_readonly_transaction(tmp_path, monkeypatch):
    record = catalogs.register('Images', catalogs.resolve().dsn, [str(tmp_path)])
    target = {'kind': 'images', 'id': record['id'], 'dsn': catalogs.resolve(record['id']).dsn, 'name': 'Images'}
    connect = psycopg.connect
    readonly = []
    class Connection:
        def __init__(self, *a, **k): self.inner = connect(*a, **k)
        def __enter__(self): self.inner.__enter__(); return self
        def __exit__(self, *args): return self.inner.__exit__(*args)
        def execute(self, query, *a, **k):
            if query.startswith('SELECT'):
                readonly.append(self.inner.execute("SHOW transaction_read_only").fetchone()['transaction_read_only'])
            return self.inner.execute(query, *a, **k)
    monkeypatch.setattr(psycopg, 'connect', Connection)
    result = service.details(target)
    assert result['database'] == 'imagecli_test'
    assert result['database_bytes'] > 0
    assert any(t['name'] == 'images' for t in result['tables'])
    assert all(value == 'on' for value in readonly) and readonly
    assert target['dsn'] not in json.dumps(result)


def test_actual_asset_database_details_are_available():
    dsn = os.environ.get('ASSET_CLI_TEST_DSN')
    if not dsn:
        pytest.skip('Set ASSET_CLI_TEST_DSN for read-only asset inspection')
    result = service.details(asset_target(dsn))
    assert result['database_bytes'] > 0
    assert any(t['name'] == 'assets' for t in result['tables'])
    assert any(t['name'] == 'packs' for t in result['tables'])


def test_private_stdin_bridge_roundtrips_metadata():
    target = asset_target()
    result = subprocess.run([sys.executable, '-m', 'image_atlas.viewer_catalogs'],
                            input=json.dumps({'target': target, 'action': 'save', 'fields': fields('Bridge test')}),
                            capture_output=True, text=True,
                            creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == 'win32' else 0)
    assert result.returncode == 0
    assert json.loads(result.stdout)['metadata']['name'] == 'Bridge test'
    assert result.stderr == ''
    assert target['dsn'] not in result.stdout
