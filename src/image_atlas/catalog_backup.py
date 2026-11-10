"""Binary PostgreSQL archives, published only after successful completion."""
import hashlib
import json
import os
import shutil
import subprocess
import tempfile
import time
import uuid
from pathlib import Path

import psycopg
from psycopg import sql
from psycopg.conninfo import conninfo_to_dict, make_conninfo

from .catalogs import SCHEMA_VERSION, register, resolve
from .jobs import MineJob


def command(tool, dsn):
    settings = conninfo_to_dict(dsn)
    env = os.environ.copy()
    env.pop('IMAGE_CLI_DSN', None)
    env['PGPASSWORD'] = settings.get('password', '')
    public_settings = {k: v for k, v in settings.items() if k != 'password'}
    args = [tool, '--dbname', make_conninfo(**public_settings)]
    if shutil.which(tool):
        return args, env
    # Only the already running project service, and only its published local connection.
    if settings.get('host') not in {'127.0.0.1', 'localhost'} or settings.get('port') != '54329':
        raise ValueError('Install compatible PostgreSQL command-line tools for this server')
    compose = Path(__file__).resolve().parents[2] / 'compose.yaml'
    result = subprocess.run(['docker', 'compose', '-f', str(compose), 'ps', '-q', 'db'], capture_output=True,
                            text=True, check=True, creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    container = result.stdout.strip()
    if not container or '\n' in container:
        raise ValueError('The existing local PostgreSQL container is unavailable')
    public_settings.update(host='127.0.0.1', port='5432')
    args[2] = make_conninfo(**public_settings)
    return ['docker', 'exec', '-i', '-e', 'PGPASSWORD', '-e', 'PGAPPNAME', container, *args], env


def run(tool, dsn, arguments, *, source=None, destination=None):
    args, env = command(tool, dsn)
    with tempfile.TemporaryFile() as errors:
        job = MineJob(tool)
        import sys
        print(f'{tool} job ID: {job.id}', file=sys.stderr, flush=True)
        env['PGAPPNAME'] = 'image-atlas-' + job.id
        process = None
        started = last_update = time.monotonic()
        try:
            process = subprocess.Popen([*args, *arguments], env=env, stdin=source or subprocess.DEVNULL,
                                       stdout=destination or subprocess.DEVNULL, stderr=errors,
                                       creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
            while process.poll() is None:
                job.check()
                if time.monotonic() - started > 3500:
                    raise TimeoutError('Transfer timed out; incomplete archive will be removed')
                if time.monotonic() - last_update >= 1:
                    last_update = time.monotonic()
                    metrics = {'elapsed_seconds': round(last_update - started, 1), 'status': 'running'}
                    if destination:
                        metrics['archive_bytes_written'] = destination.tell()
                    if source:
                        metrics['archive_bytes_read'] = source.tell()
                    job.update(**metrics)
                    details = ', '.join(f'{key.replace("_", " ")}={value}' for key, value in metrics.items())
                    print(f'{tool}: {details}', file=sys.stderr, flush=True)
                time.sleep(.1)
            if process.returncode:
                errors.seek(0)
                detail = errors.read(8192).decode('utf-8', errors='replace')
                password = conninfo_to_dict(dsn).get('password')
                if password:
                    detail = detail.replace(password, '[redacted]')
                raise ValueError(f'{tool} failed: {detail}')
        finally:
            try:
                if process is not None and process.poll() is None:
                    process.terminate()
                    # Docker exec can outlive its client: terminate only this tool's named DB session.
                    try:
                        with psycopg.connect(dsn, autocommit=True) as conn:
                            conn.execute('SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE application_name=%s AND pid<>pg_backend_pid()', (env['PGAPPNAME'],))
                    finally:
                        try:
                            process.wait(timeout=15)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait()
            finally:
                job.close()


def digest(path):
    with Path(path).open('rb') as handle:
        return hashlib.file_digest(handle, 'sha256').hexdigest()


def backup(folder):
    catalog = resolve()
    if not catalog.id:
        raise ValueError('Register the catalog before backing it up')
    from .db import connect
    with connect() as conn:
        version = conn.execute("SELECT current_setting('server_version') AS version").fetchone()['version']
        extensions = {r['extname']: r['extversion'] for r in conn.execute('SELECT extname,extversion FROM pg_extension')}
    folder = Path(folder).resolve()
    folder.mkdir(parents=True, exist_ok=True)
    stem = f'{catalog.id}-{time.strftime("%Y%m%d-%H%M%S")}-{uuid.uuid4().hex}'
    temporary = folder / ('.' + stem + '.partial')
    archive = folder / (stem + '.dump')
    manifest = archive.with_suffix('.json')
    published = False
    try:
        with temporary.open('xb') as output:
            run('pg_dump', catalog.dsn, ['--format=custom', '--no-acl'], destination=output)
        metadata = {'version': 1, 'catalog_id': catalog.id, 'name': catalog.name, 'roots': catalog.roots,
                    'schema_version': SCHEMA_VERSION, 'postgresql_version': version, 'extensions': extensions,
                    'sha256': digest(temporary), 'archive': archive.name}
        manifest_temp = temporary.with_suffix('.json.partial')
        manifest_temp.write_text(json.dumps(metadata, indent=2), encoding='utf-8')
        os.replace(temporary, archive)
        os.replace(manifest_temp, manifest)
        published = True
        return {'catalog_id': catalog.id, 'archive': str(archive), 'manifest': str(manifest)}
    finally:
        temporary.unlink(missing_ok=True)
        temporary.with_suffix('.json.partial').unlink(missing_ok=True)
        if not published:
            archive.unlink(missing_ok=True)
            manifest.unlink(missing_ok=True)


def restore(archive, name):
    archive = Path(archive).resolve()
    metadata = json.loads(archive.with_suffix('.json').read_text(encoding='utf-8'))
    if metadata.get('version') != 1 or metadata.get('schema_version') != SCHEMA_VERSION or digest(archive) != metadata.get('sha256'):
        raise ValueError('Backup manifest, schema version, or archive checksum is invalid')
    settings = conninfo_to_dict(resolve().dsn)
    database = 'imagecli_restore_' + uuid.uuid4().hex[:16]
    settings['dbname'] = 'postgres'
    admin_dsn = make_conninfo(**settings)
    with psycopg.connect(admin_dsn, autocommit=True) as conn:
        conn.execute(sql.SQL('CREATE DATABASE {}').format(sql.Identifier(database)))
    settings['dbname'] = database
    target = make_conninfo(**settings)
    complete = False
    try:
        with archive.open('rb') as source:
            run('pg_restore', target, ['--single-transaction', '--exit-on-error', '--no-owner', '--no-privileges'], source=source)
        with psycopg.connect(target) as conn:
            identity = conn.execute('SELECT id,schema_version FROM catalog_identity').fetchall()
            extensions = {row[0] for row in conn.execute('SELECT extname FROM pg_extension')}
            if len(identity) != 1 or str(identity[0][0]) != metadata.get('catalog_id') or identity[0][1] != SCHEMA_VERSION or not {'vector', 'pg_trgm'} <= extensions:
                raise ValueError('Restored catalog schema or extensions are incompatible')
            conn.execute('UPDATE catalog_identity SET id=%s', (uuid.uuid4(),))
            conn.execute('SELECT count(*) FROM images').fetchone()
            conn.execute('SELECT count(*) FROM faces').fetchone()
        record = register(name, target, roots=[], readonly=True)
        complete = True
        return {k: v for k, v in record.items() if k not in {'connection', 'credential'}}
    finally:
        if not complete:
            # Only the uniquely named database created by this operation is removed.
            with psycopg.connect(admin_dsn, autocommit=True) as conn:
                conn.execute(sql.SQL('DROP DATABASE {}').format(sql.Identifier(database)))
