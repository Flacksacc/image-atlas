"""Per-user catalog registry and immutable connection/ownership contexts."""
import contextvars
import ctypes
import json
import os
import uuid
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path

import psycopg
from psycopg.conninfo import conninfo_to_dict, make_conninfo
from psycopg import sql

SCHEMA_VERSION = 1
current = contextvars.ContextVar('image_atlas_catalog', default=None)


def directory():
    return Path(os.environ.get('IMAGE_CLI_CONFIG_DIR', str(Path(os.environ.get('LOCALAPPDATA', Path.home())) / 'image-cli')))


@contextmanager
def registry_lock():
    directory().mkdir(parents=True, exist_ok=True)
    with (directory() / 'catalogs.lock').open('a+b') as handle:
        handle.seek(0, os.SEEK_END)
        if handle.tell() == 0:
            handle.write(b'0')
            handle.flush()
        handle.seek(0)
        if os.name == 'nt':
            import msvcrt
            msvcrt.locking(handle.fileno(), msvcrt.LK_LOCK, 1)
        else:
            import fcntl
            fcntl.flock(handle, fcntl.LOCK_EX)
        try:
            yield
        finally:
            handle.seek(0)
            if os.name == 'nt':
                msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)


def registry():
    path = directory() / 'catalogs.json'
    data = json.loads(path.read_text(encoding='utf-8')) if path.exists() else {'version': 1, 'default': None, 'catalogs': []}
    if data.get('version') != 1:
        raise ValueError('Unsupported catalog registry version')
    return data


def save(data):
    path = directory() / 'catalogs.json'
    temp = path.with_name(f'.catalogs-{uuid.uuid4().hex}.tmp')
    temp.write_text(json.dumps(data, indent=2), encoding='utf-8')
    os.replace(temp, path)


class Credential(ctypes.Structure):
    _fields_ = [('Flags', ctypes.c_ulong), ('Type', ctypes.c_ulong), ('TargetName', ctypes.c_wchar_p),
                ('Comment', ctypes.c_wchar_p), ('LastWritten', ctypes.c_ulonglong),
                ('CredentialBlobSize', ctypes.c_ulong), ('CredentialBlob', ctypes.POINTER(ctypes.c_ubyte)),
                ('Persist', ctypes.c_ulong), ('AttributeCount', ctypes.c_ulong), ('Attributes', ctypes.c_void_p),
                ('TargetAlias', ctypes.c_wchar_p), ('UserName', ctypes.c_wchar_p)]


def secret(reference, value=None):
    """Passwords never enter registry JSON or backup manifests."""
    if os.name != 'nt':
        if value:
            raise ValueError('Password storage requires Windows Credential Manager')
        return None
    api = ctypes.WinDLL('advapi32', use_last_error=True)
    if value is not None:
        blob = (ctypes.c_ubyte * len(value.encode('utf-8'))).from_buffer_copy(value.encode('utf-8'))
        entry = Credential(Type=1, TargetName=reference, CredentialBlobSize=len(blob), CredentialBlob=blob, Persist=2)
        if not api.CredWriteW(ctypes.byref(entry), 0):
            raise ctypes.WinError(ctypes.get_last_error())
        return
    pointer = ctypes.POINTER(Credential)()
    if not api.CredReadW(ctypes.c_wchar_p(reference), 1, 0, ctypes.byref(pointer)):
        raise ValueError('Catalog credentials unavailable in Windows Credential Manager')
    try:
        return ctypes.string_at(pointer.contents.CredentialBlob, pointer.contents.CredentialBlobSize).decode('utf-8')
    finally:
        api.CredFree(pointer)


def delete_secret(reference):
    """Remove an owned credential reference (also used by isolated test cleanup)."""
    if os.name == 'nt':
        api = ctypes.WinDLL('advapi32', use_last_error=True)
        if not api.CredDeleteW(ctypes.c_wchar_p(reference), 1, 0) and ctypes.get_last_error() != 1168:
            raise ctypes.WinError(ctypes.get_last_error())


def canonical(path):
    return os.path.normcase(str(Path(path).expanduser().resolve()))


def under(path, root):
    try:
        saved_root = os.path.normcase(os.path.abspath(root))
        resolved_root = canonical(root)
        # A registered directory replaced by a junction must not redirect ownership.
        if saved_root != resolved_root:
            return False
        return os.path.commonpath([canonical(path), resolved_root]) == resolved_root
    except ValueError:
        return False


@dataclass(frozen=True)
class Catalog:
    id: str | None
    name: str
    dsn: str
    roots: tuple[str, ...] = ()
    readonly: bool = False

    def require_path(self, path):
        if not self.id:
            raise ValueError('Register this database and assign allowed roots before processing images')
        if self.readonly:
            raise ValueError('Restored catalog is read-only until roots are assigned')
        record = next((r for r in registry()['catalogs'] if r['id'] == self.id), None)
        roots = record['roots'] if record else self.roots
        if record and record.get('readonly'):
            raise ValueError('Catalog is read-only')
        if not any(under(path, root) for root in roots):
            raise ValueError(f'Path is outside the selected catalog\'s allowed folders: {path}')


def from_record(record):
    settings = dict(record['connection'])
    if record.get('credential'):
        settings['password'] = secret(record['credential'])
    return Catalog(record['id'], record['name'], make_conninfo(**settings), tuple(record['roots']), record.get('readonly', False))


def resolve(selector=None):
    if selector is None and current.get() is not None:
        return current.get()
    data = registry()
    if selector:
        matches = [r for r in data['catalogs'] if r['id'] == selector or r['name'].casefold() == selector.casefold()]
        if len(matches) != 1:
            raise ValueError('Unknown or ambiguous catalog')
        return from_record(matches[0])
    env = os.environ.get('IMAGE_CLI_DSN')
    if env:
        for record in data['catalogs']:
            catalog = from_record(record)
            if conninfo_to_dict(env) == conninfo_to_dict(catalog.dsn):
                return catalog
        return Catalog(None, 'Unregistered connection', env)
    if data['default']:
        return resolve(data['default'])
    from .db import DEFAULT_DSN
    return Catalog(None, 'Current', DEFAULT_DSN)


def require_path(path):
    resolve().require_path(path)


def register(name, dsn, roots=None, readonly=False):
    from .db import SCHEMA
    settings = conninfo_to_dict(dsn)
    name = name.strip()
    if not name:
        raise ValueError('Catalog name cannot be empty')
    if 'sslpassword' in settings:
        raise ValueError('Use an external client certificate key store instead of a connection sslpassword')
    with registry_lock():
        data = registry()
        if any(r['name'].casefold() == name.casefold() for r in data['catalogs']):
            raise ValueError('Catalog name already exists')
        with psycopg.connect(dsn) as conn:
            tables = {row[0] for row in conn.execute("SELECT tablename FROM pg_tables WHERE schemaname='public'")}
            if tables and 'images' not in tables:
                raise ValueError('Database contains unrelated data; registration refused')
            if 'images' in tables:
                columns = {row[0] for row in conn.execute("SELECT column_name FROM information_schema.columns WHERE table_schema='public' AND table_name='images'")}
                if not {'id', 'path', 'root_path', 'sha256', 'embedding_status', 'face_status'} <= columns:
                    raise ValueError('Database contains an incompatible images table')
            if 'catalog_identity' in tables:
                version = conn.execute('SELECT schema_version FROM catalog_identity').fetchone()
                if version and version[0] != SCHEMA_VERSION:
                    raise ValueError('Unsupported catalog schema version')
            if 'catalog_identity' not in tables:
                conn.execute(SCHEMA)
            conn.execute('CREATE TABLE IF NOT EXISTS catalog_identity (singleton boolean PRIMARY KEY DEFAULT true CHECK(singleton), id uuid NOT NULL, schema_version integer NOT NULL)')
            identity = conn.execute('SELECT id FROM catalog_identity').fetchone()
            identifier = str(identity[0]) if identity else str(uuid.uuid4())
            if any(r['id'] == identifier for r in data['catalogs']):
                raise ValueError('Database is already registered')
            conn.execute('INSERT INTO catalog_identity VALUES (true,%s,%s) ON CONFLICT(singleton) DO NOTHING', (identifier, SCHEMA_VERSION))
            if roots is None:
                roots = [row[0] for row in conn.execute('SELECT DISTINCT root_path FROM images WHERE root_path IS NOT NULL')]
            roots = validate_roots(data, identifier, roots)
        reference = os.environ.get('IMAGE_CLI_CREDENTIAL_PREFIX', 'image-cli/catalog') + '/' + identifier
        password = settings.pop('password', None)
        if password:
            secret(reference, password)
        record = {'id': identifier, 'name': name, 'connection': settings, 'credential': reference if password else None,
                  'roots': roots, 'readonly': readonly}
        data['catalogs'].append(record)
        data['default'] = data['default'] or identifier
        save(data)
        return record


def validate_roots(data, identifier, roots):
    paths = sorted(set(canonical(p) for p in roots))
    for record in data['catalogs']:
        if record['id'] != identifier:
            for old in record['roots']:
                if any(under(p, old) or under(old, p) for p in paths):
                    raise ValueError(f'Folder overlaps catalog {record["name"]}')
    return paths


def edit_roots(selector, roots):
    from .jobs import active_jobs
    catalog = resolve(selector)
    with registry_lock():
        if any(j.get('catalog_id') == catalog.id or (not j.get('catalog_id') and
               (j.get('source') == 'searcheverything' or j.get('source', '').startswith('collections:') or
                any(under(j.get('source', ''), p) for p in catalog.roots))) for j in active_jobs()):
            raise ValueError('Stop this catalog\'s running jobs before editing roots')
        data = registry()
        record = next(r for r in data['catalogs'] if r['id'] == catalog.id)
        record['roots'] = validate_roots(data, catalog.id, roots)
        record['readonly'] = not bool(record['roots'])
        save(data)
        return record


def use(selector):
    catalog = resolve(selector)
    with registry_lock():
        data = registry()
        data['default'] = catalog.id
        save(data)
    return {'default': catalog.id, 'name': catalog.name}


def create(name, roots=(), server=None):
    settings = conninfo_to_dict(server or resolve().dsn)
    database = 'imagecli_' + uuid.uuid4().hex[:16]
    settings['dbname'] = 'postgres'
    with psycopg.connect(make_conninfo(**settings), autocommit=True) as conn:
        conn.execute(sql.SQL('CREATE DATABASE {}').format(sql.Identifier(database)))
    settings['dbname'] = database
    try:
        return register(name, make_conninfo(**settings), roots=list(roots))
    except BaseException:
        settings['dbname'] = 'postgres'
        with psycopg.connect(make_conninfo(**settings), autocommit=True) as conn:
            conn.execute(sql.SQL('DROP DATABASE {}').format(sql.Identifier(database)))
        raise
