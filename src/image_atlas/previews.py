"""Versioned, content-deduplicated JPEG XL previews. Never encode on a read request."""
import io
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageCms, ImageOps

EDGE = 1024
# Changing a codec or recipe makes previously processed images eligible for backfill.
VERSION = 'jxl-1024-d2-e7-srgb-imagecodecs2026.10.10-v1'


def encode(path):
    import imagecodecs
    from .image_io import open_image
    with open_image(Path(path)) as (source, *_):
        image = ImageOps.exif_transpose(source)
        image.thumbnail((EDGE, EDGE), Image.Resampling.LANCZOS, reducing_gap=3)
        alpha = image.getchannel('A') if 'A' in image.getbands() else None
        if image.mode == 'P' and 'transparency' in image.info:
            alpha = image.convert('RGBA').getchannel('A')
        profile = image.info.get('icc_profile')
        if profile:
            image = ImageCms.profileToProfile(image, ImageCms.ImageCmsProfile(io.BytesIO(profile)),
                                             ImageCms.createProfile('sRGB'), outputMode='RGB')
        else:
            image = image.convert('RGB')
        if alpha is not None:
            image.putalpha(alpha)
        data = imagecodecs.jpegxl_encode(np.asarray(image), distance=2, effort=7,
                                        lossless=False, numthreads=2)
        return data, image.width, image.height


def decode(data):
    import imagecodecs
    pixels = imagecodecs.jpegxl_decode(data, numthreads=2)
    if pixels.ndim not in (2, 3) or max(pixels.shape[:2]) > EDGE:
        raise ValueError('Invalid cached preview dimensions')
    return Image.fromarray(pixels)


def prepare(sha, path, *, check=None):
    """Encode without a database connection; the coordinator publishes the result."""
    check = check or (lambda: None)
    status, reason, data, width, height = 'done', None, None, None, None
    try:
        from .indexer import digest
        path = Path(path)
        check()
        if digest(path) != sha:
            raise ValueError('Source changed since indexing; rescan it before building a preview')
        check()
        data, width, height = encode(path)
        check()
        if digest(path) != sha:
            raise ValueError('Source changed while building its preview; rescan it and retry')
    except Exception as exc:
        from .documents import PreviewUnavailable
        status = 'unsupported' if isinstance(exc, PreviewUnavailable) else 'error'
        reason = str(exc)[:1000]
        data, width, height = None, None, None
    return dict(sha=sha, status=status, error=reason, data=data, width=width, height=height)


def ensure(conn, sha, path, counts, *, prepared=None):
    """Called only after metadata commit; preview failure cannot undo indexing."""
    row = conn.execute('SELECT status FROM image_previews WHERE sha256=%s AND version=%s',
                       (sha, VERSION)).fetchone()
    if row and row['status'] in {'done', 'unsupported'}:
        key = 'previews_reused' if row['status'] == 'done' else 'previews_skipped'
        counts[key] += 1
        return
    from .cloud import unavailable_cloud_file
    if unavailable_cloud_file(Path(path)):
        counts['previews_pending'] += 1
        return
    artifact = prepared if prepared and prepared['sha'] == sha else prepare(sha, path)
    status, reason = artifact['status'], artifact['error']
    data, width, height = artifact['data'], artifact['width'], artifact['height']
    conn.execute('''INSERT INTO image_previews(sha256,version,status,width,height,data,error)
        VALUES(%s,%s,%s,%s,%s,%s,%s) ON CONFLICT(sha256,version) DO UPDATE SET
        status=excluded.status,width=excluded.width,height=excluded.height,data=excluded.data,
        error=excluded.error,updated_at=now()''', (sha, VERSION, status, width, height, data, reason))
    conn.commit()
    counts[{'done': 'previews_created', 'unsupported': 'previews_skipped', 'error': 'previews_failed'}[status]] += 1
    if reason:
        counts['preview_errors'].append({'path': str(path), 'error': reason, 'status': status})


def counters():
    return dict(previews_created=0, previews_reused=0, previews_pending=0,
                previews_skipped=0, previews_failed=0, preview_errors=[])


def build(conn, *, limit=None, progress=None):
    counts = counters()
    rows = conn.execute('''SELECT DISTINCT ON(i.sha256) i.sha256,i.path FROM images i
        LEFT JOIN image_previews p ON p.sha256=i.sha256 AND p.version=%s
        WHERE NOT i.missing AND i.sha256 IS NOT NULL AND
        (p.status IS NULL OR p.status='error') ORDER BY i.sha256,i.id LIMIT %s''',
        (VERSION, limit)).fetchall()
    from .catalogs import resolve
    for row in rows:
        resolve().require_path(row['path'])
        if progress:
            progress()
        ensure(conn, row['sha256'], row['path'], counts)
    storage = conn.execute('''SELECT coalesce(sum(octet_length(data)),0) payload_bytes,
        pg_total_relation_size('image_previews') database_bytes FROM image_previews''').fetchone()
    return {**counts, 'storage': storage}


def main():
    """Bounded native-viewer bridge: read a JXL file or current catalog preview."""
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument('source', nargs='?')
    parser.add_argument('--id', action='store_true')
    parser.add_argument('--stdin', action='store_true')
    args = parser.parse_args()
    if args.stdin:
        data = sys.stdin.buffer.read(16*1024*1024+1)
        if len(data) > 16*1024*1024:
            raise ValueError('Preview exceeds size limit')
    elif args.id:
        from .db import connect
        with connect() as conn:
            row = conn.execute('''SELECT p.data FROM images i JOIN image_previews p
                ON p.sha256=i.sha256 AND p.version=%s AND p.status='done' WHERE i.id=%s''',
                (VERSION, int(args.source))).fetchone()
        if not row:
            raise SystemExit('No cached JPEG XL preview')
        data = bytes(row['data'])
    else:
        data = Path(args.source).read_bytes()
    image = decode(data).convert('RGBA')
    sys.stdout.buffer.write(f'{image.width} {image.height}\n'.encode() + image.tobytes())


if __name__ == '__main__':
    main()
