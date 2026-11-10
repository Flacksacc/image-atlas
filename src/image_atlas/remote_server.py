"""Ephemeral, catalog-pinned read API; never shares gallery mutation routes."""
import base64
import json
import logging
import secrets
import threading
from contextlib import asynccontextmanager, contextmanager
from pathlib import Path

import psycopg
from fastapi import FastAPI, HTTPException, Query, Request
from fastapi.responses import Response, StreamingResponse
from psycopg.rows import dict_row
from urllib.parse import quote, urlsplit

from .previews import VERSION
from .indexer import THUMBNAIL_VERSION

LOG = logging.getLogger(__name__)


def connection_code(port, token, catalog):
    return base64.urlsafe_b64encode(json.dumps(dict(version=1, port=port, token=token,
        catalog_id=catalog.id, name=catalog.name)).encode()).decode()


@contextmanager
def readonly(catalog):
    with psycopg.connect(catalog.dsn, row_factory=dict_row,
                         options='-c default_transaction_read_only=on -c statement_timeout=30000') as conn:
        identity = conn.execute('SELECT id FROM catalog_identity').fetchone()
        if not identity or str(identity['id']) != catalog.id:
            raise HTTPException(409, 'Catalog identity changed; restart this session')
        yield conn


def create_app(catalog, *, cpu=False, encoder_factory=None):
    token = secrets.token_urlsafe(32)
    lock = threading.Lock()
    encoder = None
    device = 'not loaded'

    @asynccontextmanager
    async def lifespan(app):
        yield
        if encoder is not None and hasattr(encoder, 'close'):
            encoder.close()

    app = FastAPI(title='ImageAtlas remote session', docs_url=None, redoc_url=None,
                  openapi_url=None, lifespan=lifespan)
    app.state.token = token
    app.state.catalog = catalog

    @app.middleware('http')
    async def security(request: Request, call_next):
        host = urlsplit('http://' + request.headers.get('host', '')).hostname
        if host not in {'127.0.0.1', 'localhost', '::1'} or request.headers.get('origin'):
            return Response(status_code=403)
        supplied = request.headers.get('authorization', '')
        if not secrets.compare_digest(supplied, 'Bearer ' + token):
            return Response(status_code=401)
        if request.method not in {'GET', 'HEAD'}:
            return Response(status_code=405)
        result = await call_next(request)
        result.headers['Cache-Control'] = 'private, no-store'
        result.headers['X-Content-Type-Options'] = 'nosniff'
        return result

    @app.get('/v1/session')
    def session():
        from .embeddings import MODEL_ID
        with readonly(catalog) as conn:
            count = conn.execute('SELECT count(*) n FROM images').fetchone()['n']
        return dict(catalog_id=catalog.id, name=catalog.name, images=count,
                    preview_version=VERSION, encoder_device=device, image_search_model=MODEL_ID)

    def query_vector(q):
        nonlocal encoder, device
        from .embeddings import create_embedder
        from .db import vector_literal
        with lock:
            try:
                if encoder is None:
                    encoder = (encoder_factory or create_embedder)(cpu=cpu)
                    device = str(getattr(encoder, 'device', 'worker: GPU preferred' if not cpu else 'cpu'))
                    if not cpu and device == 'cpu':
                        LOG.warning('No supported GPU available; visual text search is using CPU')
                return vector_literal(encoder.text(q))
            except Exception as exc:
                LOG.error('Visual search failed: %s', type(exc).__name__)
                raise HTTPException(503, 'Visual search encoder unavailable; use mode=text or check server setup') from None

    @app.get('/v1/images')
    def search(q: str = Query('', max_length=1000), mode: str = 'combined',
               limit: int = Query(100, ge=1, le=500), offset: int = Query(0, ge=0),
               min_pixels: int = Query(0, ge=0), orientation: str = 'any', format: str = '',
               image_vector: str = Query('', max_length=16000)):
        if mode not in {'text', 'visual', 'combined'} or orientation not in {'any', 'landscape', 'portrait', 'square'}:
            raise HTTPException(400, 'Invalid search mode or orientation')
        if image_vector:
            from .image_query import validate_vector
            from .db import vector_literal
            try:
                vector = vector_literal(validate_vector(json.loads(image_vector)))
            except (ValueError, TypeError):
                raise HTTPException(400, 'Invalid image search vector') from None
            mode, q = 'visual', 'reference image'
        else:
            vector = query_vector(q) if q.strip() and mode != 'text' else None
        from .embeddings import MODEL_ID
        # Token matching handles folders and collections with spaces; visual matching supplies semantics.
        tokens = q.casefold().replace("'", '').replace('’', '').split()
        with readonly(catalog) as conn:
            rows = conn.execute('''WITH candidates AS (
              SELECT i.*,coalesce(s.search_text,'') || ' ' || coalesce((SELECT string_agg(tag,' ')
                FROM image_tags WHERE image_id=i.id),'') || ' ' || coalesce((SELECT string_agg(label,' ')
                FROM image_labels WHERE image_id=i.id),'') || ' ' || coalesce((SELECT string_agg(
                coalesce(c.user_name,c.suggested_name),' ') FROM collection_images m JOIN collections c
                ON c.id=m.collection_id WHERE m.image_id=i.id AND m.source<>'excluded'
                AND c.status<>'dismissed'),'') AS words,
                CASE WHEN %s::halfvec IS NOT NULL AND embedding_model=%s
                  THEN 1-(embedding <=> %s::halfvec) ELSE NULL END similarity
              FROM images i LEFT JOIN image_search s ON s.image_id=i.id
              WHERE NOT i.missing AND coalesce(i.width::bigint*i.height,0)>=%s
              AND (%s='' OR lower(i.format)=lower(%s))
              AND (%s='any' OR (%s='landscape' AND width>height)
                OR (%s='portrait' AND height>width) OR (%s='square' AND width=height))
            ), matches AS (
              SELECT *,NOT EXISTS(SELECT 1 FROM unnest(%s::text[]) t
                WHERE replace(replace(lower(words),chr(39),''),'’','') NOT LIKE '%%'||t||'%%') text_match
              FROM candidates
            ) SELECT id,path,sha256,width,height,format,size_bytes,capture_at,camera_make,camera_model,
              lens,rating,review_state,exif,generation,similarity,text_match,
              (SELECT coalesce(json_agg(tag),'[]') FROM image_tags WHERE image_id=matches.id) tags,
              (SELECT coalesce(json_agg(json_build_object('label',label,'score',score)),'[]')
                FROM image_labels WHERE image_id=matches.id) labels,
              EXISTS(SELECT 1 FROM thumbnails t WHERE t.sha256=matches.sha256 AND t.version=%s) has_thumbnail,
              EXISTS(SELECT 1 FROM image_previews p WHERE p.sha256=matches.sha256
                AND p.version=%s AND p.status='done') has_preview,
              count(*) OVER() total
              FROM matches WHERE (%s='' OR (%s='text' AND text_match)
                OR (%s='visual' AND similarity IS NOT NULL) OR (%s='combined' AND (text_match OR similarity IS NOT NULL)))
              ORDER BY (CASE WHEN %s='combined' THEN coalesce(similarity,0)+CASE WHEN text_match THEN 1 ELSE 0 END
                WHEN %s='visual' THEN similarity ELSE 0 END) DESC NULLS LAST,id
              LIMIT %s OFFSET %s''',
              (vector, MODEL_ID, vector, min_pixels, format, format, orientation, orientation, orientation,
               orientation, tokens, THUMBNAIL_VERSION, VERSION, q.strip(), mode, mode, mode, mode, mode, limit, offset)).fetchall()
        for row in rows:
            row['match_reasons'] = (['indexed text'] if row['text_match'] and q else []) + (['visual similarity'] if row['similarity'] is not None else [])
            row['preview_version'] = VERSION
            row['thumbnail_url'] = f"/v1/images/{row['id']}/thumb"
            row['preview_url'] = f"/v1/images/{row['id']}/preview"
        return dict(catalog_id=catalog.id, catalog_name=catalog.name, items=rows, total=rows[0]['total'] if rows else 0,
                    offset=offset, encoder_device=device)

    def image_record(conn, image_id):
        row = conn.execute('SELECT * FROM images WHERE id=%s', (image_id,)).fetchone()
        if not row:
            raise HTTPException(404, 'Image ID not found in this catalog')
        return row

    @app.get('/v1/images/{image_id}')
    def metadata(image_id: int):
        with readonly(catalog) as conn:
            row = image_record(conn, image_id)
            # Do not transfer vectors with the metadata response.
            return {k: v for k, v in row.items() if k != 'embedding'}

    @app.get('/v1/images/{image_id}/{kind}')
    def image_bytes(image_id: int, kind: str):
        with readonly(catalog) as conn:
            row = image_record(conn, image_id)
            if kind in {'thumb', 'preview'}:
                table, version = ('thumbnails', THUMBNAIL_VERSION) if kind == 'thumb' else ('image_previews', VERSION)
                suffix = " AND status='done'" if kind == 'preview' else ''
                saved = conn.execute(f'SELECT data,mime_type FROM {table} WHERE sha256=%s AND version=%s{suffix}',
                                     (row['sha256'], version)).fetchone()
                if not saved or saved['data'] is None:
                    raise HTTPException(404, 'Cached image unavailable; rescan to backfill it')
                return Response(bytes(saved['data']), media_type=saved['mime_type'],
                                headers={'X-Image-SHA256': row['sha256'] or '', 'X-Preview-Version': VERSION})
            if kind != 'original':
                raise HTTPException(404, 'Unknown image representation')
        from .cloud import unavailable_cloud_file
        path = Path(row['path'])
        try:
            catalog.require_path(path)
        except ValueError:
            raise HTTPException(403, 'Original is outside the catalog allowed folders') from None
        if unavailable_cloud_file(path):
            raise HTTPException(409, 'Original requires cloud download; make it available locally on the remote computer')
        try:
            handle = path.open('rb')
            import os
            stat = os.fstat(handle.fileno())
            if stat.st_size != row['size_bytes'] or stat.st_mtime_ns != row['mtime_ns']:
                handle.close()
                raise HTTPException(409, 'Original changed; rescan before downloading')
        except FileNotFoundError:
            raise HTTPException(404, 'Original is missing') from None
        except OSError:
            raise HTTPException(403, 'Original cannot be read by this account') from None
        def chunks():
            with handle:
                while chunk := handle.read(1024*1024):
                    yield chunk
        headers = {'Content-Length': str(stat.st_size),
                   'Content-Disposition': "attachment; filename*=UTF-8''" + quote(path.name),
                   'X-Image-SHA256': row['sha256'] or ''}
        return StreamingResponse(chunks(), media_type='application/octet-stream', headers=headers)

    return app
