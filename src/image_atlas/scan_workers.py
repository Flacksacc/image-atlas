"""Bounded file preparation workers. Only the calling thread touches PostgreSQL."""
from concurrent.futures import CancelledError, FIRST_COMPLETED, ThreadPoolExecutor, wait
from contextvars import copy_context
from dataclasses import dataclass
from itertools import islice
from pathlib import Path
from threading import Event


@dataclass
class PreparedImage:
    path: Path
    old: dict | None
    stat: object = None
    sha: str | None = None
    info: dict | None = None
    thumbnail: tuple | None = None
    preview: dict | None = None
    cloud: bool = False
    error: Exception | None = None


def _prepare(path, old, thumbnails, previews_ready, cancelled):
    from .cloud import unavailable_cloud_file
    from .indexer import digest, metadata, thumbnail_from_path, _preview_unavailable
    from . import previews

    result = PreparedImage(path, old)
    def check():
        if cancelled.is_set():
            raise CancelledError('Scan cancelled')
    try:
        check()
        if unavailable_cloud_file(path):
            result.cloud = True
            return result
        result.stat = path.stat()
        unchanged = (old and not old['missing'] and old['size_bytes'] == result.stat.st_size
                     and old['mtime_ns'] == result.stat.st_mtime_ns)
        result.sha = old['sha256'] if unchanged else None
        if not result.sha or (result.sha not in thumbnails and not _preview_unavailable(old['exif'] if old else {})):
            result.sha = digest(path)
        check()
        if not unchanged or result.sha != old['sha256'] or old['metadata_version'] < 2:
            result.info = metadata(path, include_thumbnail=result.sha not in thumbnails)
            result.thumbnail = result.info.get('thumbnail')
        elif result.sha not in thumbnails and not _preview_unavailable(old['exif']):
            result.thumbnail = thumbnail_from_path(path)
        check()
        if result.sha and result.sha not in previews_ready:
            result.preview = previews.prepare(result.sha, path, check=check)
        check()
        current = path.stat()
        if (current.st_size, current.st_mtime_ns) != (result.stat.st_size, result.stat.st_mtime_ns):
            raise ValueError('Source changed while preparing its scan; retry it after saving finishes')
    except Exception as exc:
        result.error = exc
    return result


def prepared_paths(conn, paths, *, workers, limit=None, check=None):
    """At most twice the worker count is queued; cancellation stops queued work."""
    check = check or (lambda: None)
    paths = iter(paths if limit is None else islice(paths, limit))
    if workers == 1:
        for path in paths:
            check()
            yield Path(path), None
        return
    from .catalogs import resolve
    from .db import normalized_path
    from .indexer import THUMBNAIL_VERSION
    from .previews import VERSION
    thumbnails = frozenset(row['sha256'] for row in conn.execute(
        'SELECT sha256 FROM thumbnails WHERE version=%s', (THUMBNAIL_VERSION,)))
    previews_ready = frozenset(row['sha256'] for row in conn.execute(
        "SELECT sha256 FROM image_previews WHERE version=%s AND status IN ('done','unsupported')", (VERSION,)))
    cancelled = Event()
    pool = ThreadPoolExecutor(max_workers=workers, thread_name_prefix='image-atlas-scan')
    pending = set()
    exhausted = False
    try:
        while pending or not exhausted:
            while not exhausted and len(pending) < workers * 2:
                check()
                path = next(paths, None)
                if path is None:
                    exhausted = True
                    break
                path = Path(path)
                catalog = resolve()
                if catalog.id:
                    catalog.require_path(path)
                # All database access stays on this coordinator thread.
                old = conn.execute('SELECT id,root_path,size_bytes,mtime_ns,missing,sha256,dhash,metadata_version,exif FROM images WHERE path=%s',
                                   (normalized_path(path),)).fetchone()
                pending.add(pool.submit(copy_context().run, _prepare, path, old, thumbnails, previews_ready, cancelled))
            if not pending:
                break
            check()
            done, _ = wait(pending, timeout=.1, return_when=FIRST_COMPLETED)
            for future in done:
                check()
                pending.remove(future)
                prepared = future.result()
                yield prepared.path, prepared
    finally:
        cancelled.set()
        for future in pending:
            future.cancel()
        pool.shutdown(wait=True, cancel_futures=True)
