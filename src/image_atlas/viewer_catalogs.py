"""Private shared-viewer database metadata and read-only storage inspection."""
import hashlib
import json
import os
import sys
import uuid
from datetime import datetime, timezone

import psycopg
from psycopg.conninfo import conninfo_to_dict
from psycopg.rows import dict_row

from . import catalogs


def metadata_path():
    return catalogs.directory() / 'viewer-catalogs.json'


def _overlay():
    path = metadata_path()
    value = json.loads(path.read_text(encoding='utf-8')) if path.exists() else {'version': 1, 'catalogs': {}}
    if value.get('version') != 1 or not isinstance(value.get('catalogs'), dict):
        raise ValueError('Unsupported viewer database metadata format')
    return value


def _key(target):
    kind = target.get('kind')
    if kind not in {'assets', 'images'}:
        raise ValueError('Unknown database kind')
    identifier = target.get('id')
    if kind == 'images' and identifier:
        return 'images:' + str(identifier)
    try:
        settings = conninfo_to_dict(target['dsn'])
    except (KeyError, psycopg.Error):
        raise ValueError('Database connection settings are invalid') from None
    # Store a non-secret identity; passwords, certificate paths, and DSNs stay out.
    identity = {k: settings.get(k, default) for k, default in
                [('host', 'localhost'), ('hostaddr', ''), ('port', '5432'), ('dbname', '')]}
    return kind + ':' + hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()


def metadata(target):
    key = _key(target)
    if target['kind'] == 'images' and target.get('id'):
        record = next((r for r in catalogs.registry()['catalogs'] if r['id'] == target['id']), None)
        if record is None:
            raise ValueError('Image database is no longer registered')
    else:
        record = _overlay()['catalogs'].get(key, {})
    return {field: record.get(field, target.get('name', '') if field == 'name' else '')
            for field in ['name', 'description', 'notes', 'metadata_updated_at']}


def save_metadata(target, fields):
    clean = {}
    for key, limit in [('name', 120), ('description', 2000), ('notes', 8000)]:
        value = fields.get(key, '')
        if not isinstance(value, str) or len(value) > limit:
            raise ValueError(f'{key.capitalize()} must be text of at most {limit} characters')
        value = value.strip()
        if any(ord(c) < 32 and (key == 'name' or c not in '\n\r\t') for c in value):
            raise ValueError(f'{key.capitalize()} contains unsupported control characters')
        if key == 'name' and not value:
            raise ValueError('Display name cannot be empty')
        clean[key] = value
    clean['metadata_updated_at'] = datetime.now(timezone.utc).isoformat()
    key = _key(target)
    with catalogs.registry_lock():
        if target['kind'] == 'images' and target.get('id'):
            data = catalogs.registry()
            record = next((r for r in data['catalogs'] if r['id'] == target['id']), None)
            if record is None:
                raise ValueError('Image database is no longer registered')
            if any(r['id'] != record['id'] and r['name'].casefold() == clean['name'].casefold() for r in data['catalogs']):
                raise ValueError('Another image database already has that display name')
            record.update(clean)
            catalogs.save(data)
        else:
            data = _overlay()
            data['catalogs'].setdefault(key, {}).update(clean)
            path = metadata_path()
            temporary = path.with_name('.viewer-catalogs-' + uuid.uuid4().hex + '.tmp')
            try:
                temporary.write_text(json.dumps(data, indent=2), encoding='utf-8')
                os.replace(temporary, path)
            finally:
                temporary.unlink(missing_ok=True)
    return clean


def details(target):
    """Inspect this exact connection without schema changes or reading asset files."""
    settings = conninfo_to_dict(target['dsn'])
    with psycopg.connect(target['dsn'], connect_timeout=5, row_factory=dict_row) as conn:
        conn.execute('SET TRANSACTION READ ONLY')
        conn.execute("SET LOCAL statement_timeout='15s'")
        relation = 'images' if target['kind'] == 'images' else 'assets'
        if conn.execute('SELECT to_regclass(%s) AS relation', ('public.' + relation,)).fetchone()['relation'] is None:
            raise ValueError('Database schema does not match this catalog type')
        if target['kind'] == 'images' and target.get('id'):
            identity = conn.execute('SELECT id::text FROM catalog_identity').fetchone()
            if not identity or identity['id'] != target['id']:
                raise ValueError('Database identity does not match the selected catalog')
        result = dict(conn.execute("""SELECT current_database() AS database,
            current_user AS username, current_setting('server_version') AS server_version,
            current_setting('server_encoding') AS encoding,
            pg_database_size(current_database()) AS database_bytes""").fetchone())
        result.update(host=settings.get('host') or settings.get('hostaddr') or 'localhost',
                      port=settings.get('port', '5432'), kind=target['kind'], catalog_id=target.get('id'))
        result['tables'] = [dict(row) for row in conn.execute("""SELECT c.relname AS name,
            pg_table_size(c.oid) AS data_bytes, pg_indexes_size(c.oid) AS index_bytes,
            pg_total_relation_size(c.oid) AS total_bytes,
            greatest(c.reltuples,0)::bigint AS estimated_rows
            FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
            WHERE n.nspname='public' AND c.relkind IN ('r','m')
            ORDER BY pg_total_relation_size(c.oid) DESC""")]
        result['table_data_bytes'] = sum(t['data_bytes'] for t in result['tables'])
        result['index_bytes'] = sum(t['index_bytes'] for t in result['tables'])
    if target['kind'] == 'images' and target.get('id'):
        record = next((r for r in catalogs.registry()['catalogs'] if r['id'] == target['id']), {})
        result['readonly'] = record.get('readonly', False)
        result['roots'] = record.get('roots', [])
    return result


def request(value):
    target = value['target']
    action = value.get('action', 'load')
    if action == 'save':
        return {'metadata': save_metadata(target, value.get('fields', {}))}
    result = {'metadata': metadata(target)}
    if action == 'metadata':
        return result
    if action != 'load':
        raise ValueError('Unknown database metadata operation')
    try:
        result['details'] = details(target)
    except (psycopg.Error, OSError, ValueError):
        result['details_error'] = 'Database details unavailable. Check the connection and refresh details.'
    return result


def main():
    try:
        raw = sys.stdin.buffer.read(128 * 1024 + 1)
        if len(raw) > 128 * 1024:
            raise ValueError('Database metadata request is too large')
        result = request(json.loads(raw))
    except ValueError as error:
        # Validation messages contain only field names; no connection diagnostics.
        print(json.dumps({'error': str(error) if not isinstance(error, json.JSONDecodeError)
                          else 'Invalid database metadata request'}))
    except Exception:
        print(json.dumps({'error': 'Could not access database metadata. Check local configuration permissions.'}))
    else:
        print(json.dumps(result))


if __name__ == '__main__':
    main()
