"""Readable command reports, with explicit JSON output for automation."""
from contextvars import ContextVar
import json
import os
import subprocess
import sys
from pathlib import Path

import psycopg
import typer

from . import __version__
from .db import connect, initialize, normalized_path, vector_literal

app = typer.Typer(help="Index and review local images without moving originals.", no_args_is_help=True,
                  pretty_exceptions_show_locals=False)
faces_app = typer.Typer(help="Detect, group, and tag faces.")
sort_app = typer.Typer(help="Create and inspect sorting proposals.")
db_app = typer.Typer(help="Check the PostgreSQL database and its storage use.")
app.add_typer(faces_app, name="faces")
app.add_typer(sort_app, name="sort")
app.add_typer(db_app, name="db")
collections_app = typer.Typer(help="Detect and review events, trips, and places without opening originals.")
app.add_typer(collections_app, name="collections")


JSON_OUTPUT = ContextVar('image_atlas_json_output', default=False)


def emit(value, *, json_output: bool = False):
    if json_output or JSON_OUTPUT.get():
        typer.echo(json.dumps(value, ensure_ascii=False, default=str))
        return
    def lines(item, indent=0, max_entries=None):
        prefix = ' ' * indent
        if isinstance(item, dict):
            for key, child in item.items():
                label = key.replace('_', ' ').capitalize()
                if isinstance(child, (dict, list, tuple)):
                    yield prefix + label + ':'
                    yield from lines(child, indent + 2)
                else:
                    text = 'n/a' if child is None else ('yes' if child is True else 'no' if child is False else str(child))
                    yield prefix + label + ': ' + text
        elif isinstance(item, (list, tuple)):
            if not item:
                yield prefix + 'None'
            entries = item if max_entries is None else item[:max_entries]
            for child in entries:
                yield prefix + '-'
                yield from lines(child, indent + 2)
            if max_entries is not None and len(item) > max_entries:
                yield prefix + f'{len(item) - max_entries} more entries; use --json for the complete report.'
        else:
            yield prefix + str(item)
    if isinstance(value, dict) and 'stages' in value and 'metrics' in value:
        status = 'Cancelled' if value.get('cancelled') else ('Completed with errors' if any(s.get('error') or s.get('errors') or s.get('previews_failed') for s in value['stages'].values()) else 'Completed')
        typer.echo(f'{status}: {value["root"]}')
        for name, stage in value['stages'].items():
            counts = {k: v for k, v in stage.items() if k in {'new', 'updated', 'unchanged', 'moved', 'processed', 'images', 'faces', 'skipped_cloud', 'worker_restarts', 'error', 'previews_created', 'previews_reused', 'previews_failed', 'previews_pending', 'previews_skipped'}}
            typer.echo(f'{name.capitalize()}: ' + ', '.join(f'{k.replace("_", " ")}={v}' for k, v in counts.items()))
            if stage.get('errors'):
                typer.echo(f'  {len(stage["errors"])} image errors:')
                typer.echo('\n'.join(lines(stage['errors'], 2, max_entries=20)))
        metrics = value['metrics']
        rate = metrics.get('images_per_second')
        typer.echo(f'{metrics.get("pictures_total", 0)} images examined; {metrics.get("elapsed_seconds", 0):.1f}s elapsed; ' + (f'{rate:.1f} images/s (last 5s)' if rate is not None else 'throughput unavailable'))
    else:
        typer.echo('\n'.join(lines(value)))


@app.callback()
def select_database(ctx: typer.Context, database: str | None = typer.Option(None, '--database', help='Catalog name or UUID; pins this invocation.'),
                    json_output: bool = typer.Option(False, '--json', help='Print structured JSON instead of a readable report.')):
    JSON_OUTPUT.set(json_output)
    from .catalogs import current, resolve, registry, register
    current.set(None)
    if ctx.invoked_subcommand == 'remote':
        return
    try:
        if ctx.invoked_subcommand != 'serve-images' and database is None and not os.environ.get('IMAGE_CLI_DSN') and not registry()['catalogs']:
            from .db import DEFAULT_DSN
            # Adopt the legacy local database once, without changing indexed contents.
            try:
                register('Current', DEFAULT_DSN)
            except psycopg.OperationalError:
                pass  # Registry inspection remains available with an offline server.
        current.set(resolve(database))
    except ValueError as exc:
        raise typer.BadParameter(str(exc)) from exc


@db_app.command('list')
def db_list():
    from .catalogs import registry
    data = registry()
    emit({'default': data['default'], 'catalogs': [{k: v for k, v in r.items() if k not in {'credential', 'connection'}} for r in data['catalogs']]})


@db_app.command('register')
def db_register(name: str, dsn: str | None = None, root: list[Path] | None = None, dsn_stdin: bool = False):
    from .catalogs import register, resolve
    if dsn_stdin:
        import sys
        if dsn is not None:
            raise typer.BadParameter("Choose --dsn or --dsn-stdin")
        dsn = sys.stdin.readline(65536).strip()
        if not dsn:
            raise typer.BadParameter("A connection string is required on stdin")
    record = register(name, dsn or resolve().dsn, [str(p) for p in root] if root is not None else None)
    emit({k: v for k, v in record.items() if k not in {'credential', 'connection'}})


@db_app.command('create')
def db_create(name: str, root: list[Path] | None = None):
    from .catalogs import create
    record = create(name, [str(p) for p in root or []])
    emit({k: v for k, v in record.items() if k not in {'credential', 'connection'}})


@db_app.command('use')
def db_use(name: str):
    from .catalogs import use
    emit(use(name))


@db_app.command('roots')
def db_roots(name: str, root: list[Path] | None = None, clear: bool = False):
    from .catalogs import edit_roots, resolve
    if root is not None or clear:
        record = edit_roots(name, [str(p) for p in root or []])
        emit({'id': record['id'], 'roots': record['roots'], 'readonly': record['readonly']})
    else:
        catalog = resolve(name)
        emit({'id': catalog.id, 'roots': catalog.roots, 'readonly': catalog.readonly})


@db_app.command('backup')
def db_backup(folder: Path):
    from .catalog_backup import backup
    from .jobs import MiningCancelled
    from .catalogs import resolve
    try:
        emit(backup(folder))
    except (MiningCancelled, KeyboardInterrupt):
        emit({'catalog_id': resolve().id, 'cancelled': True})
        raise typer.Exit(130)


@db_app.command('context', hidden=True)
def db_context():
    """Private local viewer bridge; contains connection credentials, never log it."""
    from .catalogs import resolve
    initialize()
    catalog = resolve()
    from .worker_env import model_python
    vision_python = model_python("IMAGE_CLI_VISION_PYTHON", ("torch", "open_clip")) or sys.executable
    emit({'id': catalog.id, 'name': catalog.name, 'dsn': catalog.dsn, 'roots': catalog.roots, 'readonly': catalog.readonly,
          'vision_python': vision_python})


@db_app.command('allow-path', hidden=True)
def db_allow_path(path: Path):
    from .catalogs import require_path
    require_path(path)
    emit({'allowed': True})


@db_app.command('restore')
def db_restore(archive: Path, name: str):
    from .catalog_backup import restore
    from .jobs import MiningCancelled
    from .catalogs import resolve
    try:
        emit(restore(archive, name))
    except (MiningCancelled, KeyboardInterrupt):
        emit({'catalog_id': resolve().id, 'cancelled': True})
        raise typer.Exit(130)


@collections_app.command("detect")
def collections_detect(root: str | None = None):
    from .collections import detect
    from .jobs import MineJob, MiningCancelled
    initialize()
    job = MineJob("collections:" + (root or "catalog"))
    typer.echo(f"Collection job ID: {job.id}", err=True)
    count = 0
    def progress(n):
        nonlocal count
        count = n
        if n % 500 == 0:
            typer.echo(f"Clustering {n} distinct images", err=True)
    try:
        with connect() as conn:
            emit(detect(conn, root, check=job.check, progress=progress))
    except (MiningCancelled, KeyboardInterrupt):
        emit({"cancelled": True, "images_examined": count})
        raise typer.Exit(130)
    finally:
        job.close()


@collections_app.command("list")
def collections_list(kind: str | None = None, include_dismissed: bool = False, limit: int = 100, offset: int = 0):
    from .collections import listing
    if not 1 <= limit <= 500 or offset < 0:
        raise typer.BadParameter("Use limit 1–500 and a nonnegative offset")
    initialize()
    with connect() as conn:
        emit(listing(conn, kind, include_dismissed, limit, offset))


@collections_app.command("show")
def collections_show(collection_id: int, limit: int = typer.Option(100, min=1, max=500), offset: int = typer.Option(0, min=0)):
    from .collections import detail
    initialize()
    with connect() as conn:
        emit(detail(conn, collection_id, limit=limit, offset=offset))


@collections_app.command("review")
def collections_review(collection_id: int, action: str, name: str | None = None,
                       image_id: list[int] | None = None, other_id: int | None = None):
    from .collections import review
    initialize()
    with connect() as conn:
        emit(review(conn, collection_id, action, name=name, image_ids=image_id, other_id=other_id, detail_limit=100))


@collections_app.command("lookup")
def collections_lookup(collection_id: int):
    """Explicitly send one place coordinate to the configured reverse geocoder."""
    from .collections import lookup_place
    initialize()
    with connect() as conn:
        emit(lookup_place(conn, collection_id))


@collections_app.command("settings")
def collections_settings(values: str | None = None):
    """Read settings, or update using --values '{\"event_gap_hours\": 3}'."""
    from .collections import settings
    initialize()
    changes = json.loads(values) if values else None
    if changes is not None and not isinstance(changes, dict):
        raise typer.BadParameter("Settings must be a JSON object")
    with connect() as conn:
        emit(settings(conn, changes))


MINE_TAGS = ("embeddings", "labels", "faces")


def parse_mine_tags(value: str) -> tuple[str, ...]:
    selected = {tag.strip().lower() for tag in value.split(",")}
    if "all" in selected and len(selected) == 1:
        return MINE_TAGS
    allowed = set(MINE_TAGS) | {"metadata"}
    if not selected or "" in selected or selected - allowed:
        raise typer.BadParameter("Use all, metadata, embeddings, labels, or faces (comma-separated)")
    return tuple(tag for tag in MINE_TAGS if tag in selected)


@app.command()
def mine(root: Path, tags: str = "all", limit: int | None = None, retry_errors: bool = False,
         cpu: bool = False, prune: bool = False, json_output: bool = typer.Option(False, "--json"),
         exclude_dir: list[Path] | None = None, include_system: bool = False):
    """Mine a file, folder, or searcheverything: thumbnails, embeddings, and faces."""
    from . import embeddings, faces, labels
    from .cloud import unavailable_cloud_file
    from .indexer import EXTENSIONS, iter_images, select_images_for_mine
    from .indexer import scan as run_scan
    from .jobs import MineJob, MiningCancelled
    from .progress import MiningProgress
    selected = parse_mine_tags(tags)
    everything_source = str(root).lower() == "searcheverything"
    from .catalogs import resolve
    catalog = resolve()
    if everything_source:
        if not catalog.id or catalog.readonly or not catalog.roots:
            raise typer.BadParameter('Select a registered, writable catalog with allowed folders')
    else:
        catalog.require_path(root)
    if limit is not None and limit < 1:
        raise typer.BadParameter("--limit must be at least 1")
    if limit is not None and prune:
        raise typer.BadParameter("--prune requires a complete run without --limit")
    if everything_source and prune:
        raise typer.BadParameter("--prune is unavailable for searcheverything")
    if not everything_source and (exclude_dir or include_system):
        raise typer.BadParameter("--exclude-dir and --include-system require searcheverything")
    excluded_roots = tuple(Path(normalized_path(path)) for path in (exclude_dir or []))
    root_key = "searcheverything" if everything_source else normalized_path(root)
    if not everything_source and not Path(root_key).exists():
        raise typer.BadParameter(f"Path does not exist: {root_key}")
    if not everything_source and Path(root_key).is_file() and Path(root_key).suffix.lower() not in EXTENSIONS:
        raise typer.BadParameter(f"Unsupported image type: {root_key}")
    result = {"root": root_key, "catalog_id": catalog.id, "tags": ["metadata", *selected], "stages": {}}
    if everything_source:
        result["filters"] = {"system_and_program_dirs": not include_system,
                             "exclude_dirs": [str(path) for path in excluded_roots]}
    progress = MiningProgress().start()
    job = MineJob(root_key)
    result["job_id"] = job.id
    pictures_total = 0
    skipped_cloud = 0
    failed = True
    cancelled = False
    def advance():
        progress.advance()
        job.check()
    def examine():
        progress.examine()
        job.check()
    try:
        initialize()
        with connect() as conn:
            candidates = None
            if everything_source:
                from .everything import count_indexed_images
                from .everything import iter_images as everything_images
                candidates = everything_images(exclude_dirs=excluded_roots, include_system=include_system)
            image_ids = []
            if limit is not None:
                progress.begin("find", total=limit, pictures_total=limit)
                paths, examined, skipped_cloud = select_images_for_mine(
                    conn, Path(root_key), selected, limit, retry_errors=retry_errors,
                    progress=examine, selected_progress=advance, candidates=candidates)
                pictures_total = len(paths)
                progress.set_total(pictures_total)
            else:
                progress.begin("count")
                discover_total = 0
                if everything_source:
                    discover_total = count_indexed_images(exclude_dirs=excluded_roots,
                                                          include_system=include_system, progress=advance)
                else:
                    for _ in iter_images(Path(root_key)):
                        discover_total += 1
                        advance()
                progress.begin("find", total=discover_total)
                paths = [] if everything_source else None
                for path in candidates if everything_source else iter_images(Path(root_key)):
                    progress.examine()
                    advance()
                    if unavailable_cloud_file(path):
                        skipped_cloud += 1
                    else:
                        pictures_total += 1
                        if paths is not None:
                            paths.append(path)
                examined = discover_total
            result["pictures_examined"] = examined
            result["skipped_cloud"] = skipped_cloud
            progress.begin("scan", pictures_total, pictures_total)
            result["stages"]["metadata"] = {}
            result["stages"]["metadata"] = run_scan(conn, root_key, prune=prune, paths=paths,
                                                        image_ids=image_ids, progress=advance,
                                                        virtual_source=everything_source, counts=result["stages"]["metadata"])
            result["stages"]["metadata"].update(progress.stage_metrics())
            result["stages"]["metadata"]["completed"] = True
            root_filter = None if everything_source else root_key
            selected_table = limit is None
            if selected_table:
                conn.execute("CREATE TEMP TABLE mine_selected_ids(id bigint PRIMARY KEY) ON COMMIT PRESERVE ROWS")
                conn.execute("INSERT INTO mine_selected_ids SELECT DISTINCT unnest(%s::bigint[])", (image_ids,))
                conn.commit()
                image_ids = None
            for tag in selected:
                job.check()
                try:
                    column = {"embeddings": "embedding_status", "labels": "labels_status",
                              "faces": "face_status"}[tag]
                    statuses = ["pending", "error"] if retry_errors else ["pending"]
                    scope = "AND EXISTS (SELECT 1 FROM mine_selected_ids s WHERE s.id=images.id)" if selected_table else ""
                    status_condition = ("(labels_status=ANY(%s) OR (labels_status='done' AND labels_version<%s))"
                                        if tag == "labels" else f"{column}=ANY(%s)")
                    status_args = (statuses, labels.LABEL_VERSION) if tag == "labels" else (statuses,)
                    pending = conn.execute(f"""SELECT count(*) AS n FROM images WHERE (%s::text IS NULL OR root_path=%s)
                                             AND missing=false AND {status_condition}
                                             {"AND embedding IS NOT NULL" if tag == "labels" else ""}
                                             AND (%s::bigint[] IS NULL OR id=ANY(%s::bigint[])) {scope}""",
                                           (root_filter, root_filter, *status_args, image_ids, image_ids)).fetchone()["n"]
                    progress.begin(tag, pending, pictures_total)
                    if tag == "embeddings":
                        stage = embeddings.process_pending(conn, retry_errors=retry_errors, cpu=cpu,
                                                           root_path=root_filter, image_ids=image_ids,
                                                           progress=advance, selected_table=selected_table)
                    elif tag == "labels":
                        stage = labels.process_pending(conn, retry_errors=retry_errors, cpu=cpu,
                                                       root_path=root_filter, image_ids=image_ids,
                                                       progress=advance, selected_table=selected_table)
                    else:
                        result['stages'][tag] = {}
                        stage = faces.process_pending(conn, retry_errors=retry_errors, root_path=root_filter,
                                                      image_ids=image_ids, progress=advance,
                                                      selected_table=selected_table, check=job.check,
                                                      counts=result['stages'][tag], job=job)
                    result["stages"][tag] = stage
                except Exception as exc:  # noqa: BLE001 - report failed stage, continue other stages
                    conn.rollback()
                    result["stages"].setdefault(tag, {})['error'] = str(exc)
                result["stages"][tag].update(progress.stage_metrics())
        failed = any(stage.get("error") or stage.get("errors") or stage.get('previews_failed') for stage in result["stages"].values())
    except (MiningCancelled, KeyboardInterrupt):
        cancelled = True
        result["cancelled"] = True
        if "metadata" in result["stages"] and not result["stages"]["metadata"].get("completed"):
            result["stages"]["metadata"].update(progress.stage_metrics())
        if progress.stage.lower() in result['stages']:
            result['stages'][progress.stage.lower()].update(progress.stage_metrics())
    finally:
        result["metrics"] = progress.finish(pictures_total, failed=failed, cancelled=cancelled)
        job.close()
    emit(result, json_output=json_output)
    if cancelled:
        raise typer.Exit(code=130)
    if failed:
        raise typer.Exit(code=1)


@app.command()
def jobs():
    """List running mine jobs and their IDs."""
    from .jobs import active_jobs
    from .catalogs import resolve
    emit([j for j in active_jobs() if j.get('catalog_id') == resolve().id])


@app.command()
def cancel(job_id: str | None = typer.Argument(None)):
    """Request cancellation of a mine job (ID optional when only one runs)."""
    from .jobs import cancel_job
    try:
        emit({"cancel_requested": cancel_job(job_id)})
    except ValueError as exc:
        raise typer.BadParameter(str(exc)) from exc


@app.command(hidden=True)
def init():
    """Create tables and indexes in the configured PostgreSQL database."""
    initialize()
    emit({"ok": True, "version": __version__})


@app.command()
def status():
    """Report indexed counts, enrichment progress, and PostgreSQL disk usage."""
    from .indexer import THUMBNAIL_VERSION
    with connect() as conn:
        row = conn.execute("""SELECT count(*) FILTER (WHERE NOT missing) AS images,
                            count(*) FILTER (WHERE missing) AS missing,
                            count(*) FILTER (WHERE embedding_status='done' AND NOT missing) AS embeddings_done,
                            count(*) FILTER (WHERE labels_status='done' AND NOT missing) AS labels_done,
                            count(*) FILTER (WHERE face_status='done' AND NOT missing) AS faces_checked
                            FROM images""").fetchone()
        row["faces"] = conn.execute("SELECT count(*) AS n FROM faces").fetchone()["n"]
        thumbs = conn.execute("""SELECT count(*) AS thumbnails_stored,
                               coalesce(sum(octet_length(data)),0) AS thumbnail_bytes
                               FROM thumbnails WHERE version=%s""", (THUMBNAIL_VERSION,)).fetchone()
        row.update(thumbs)
        row["images_with_thumbnails"] = conn.execute("""SELECT count(*) AS n FROM images i
                            WHERE EXISTS (SELECT 1 FROM thumbnails t WHERE t.sha256=i.sha256 AND t.version=%s)""",
                            (THUMBNAIL_VERSION,)).fetchone()["n"]
        row["database_bytes"] = conn.execute("SELECT pg_database_size(current_database()) AS n").fetchone()["n"]
        from .previews import VERSION
        if conn.execute("SELECT to_regclass('public.image_previews') name").fetchone()['name']:
            row['previews'] = conn.execute('''SELECT count(*) FILTER(WHERE status='done') ready,
                count(*) FILTER(WHERE status='error') failed,count(*) FILTER(WHERE status='unsupported') unsupported,
                coalesce(sum(octet_length(data)),0) payload_bytes,
                pg_total_relation_size('image_previews') database_bytes FROM image_previews WHERE version=%s''', (VERSION,)).fetchone()
    emit(row)


@db_app.command("check")
def db_check():
    """Check connectivity, required tables, and PostgreSQL extensions."""
    try:
        with connect() as conn:
            row = conn.execute("""SELECT current_database() AS database,
                                  current_setting('server_version') AS postgresql_version,
                                  to_regclass('public.images') IS NOT NULL AS images_table,
                                  to_regclass('public.thumbnails') IS NOT NULL AS thumbnails_table""").fetchone()
            extensions = conn.execute("SELECT extname,extversion FROM pg_extension WHERE extname=ANY(%s)",
                                      (["vector", "pg_trgm"],)).fetchall()
            row["extensions"] = {item["extname"]: item["extversion"] for item in extensions}
            row["ok"] = (row["images_table"] and row["thumbnails_table"]
                         and "vector" in row["extensions"] and "pg_trgm" in row["extensions"])
            row["images"] = conn.execute("SELECT count(*) AS n FROM images").fetchone()["n"] if row["images_table"] else None
    except psycopg.Error as exc:
        emit({"ok": False, "error": str(exc).splitlines()[0]})
        raise typer.Exit(code=1) from exc
    from .catalogs import resolve
    catalog = resolve()
    emit({**row, 'catalog_id': catalog.id, 'catalog_name': catalog.name, 'readonly': catalog.readonly})
    if not row["ok"]:
        raise typer.Exit(code=1)


@db_app.command("storage")
def db_storage():
    """Report database, table, index, and derived payload sizes in bytes."""
    try:
        with connect() as conn:
            if conn.execute("SELECT to_regclass('public.images') AS relation").fetchone()["relation"] is None:
                emit({"ok": False, "error": "Database is not initialized; run image-atlas mine PATH"})
                raise typer.Exit(code=1)
            info = conn.execute("""SELECT current_database() AS database,
                                  pg_database_size(current_database()) AS database_bytes""").fetchone()
            relations = conn.execute("""SELECT c.relname AS name,
                               pg_relation_size(c.oid) AS table_bytes,
                               pg_indexes_size(c.oid) AS index_bytes,
                               pg_total_relation_size(c.oid) AS total_bytes
                               FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
                               WHERE n.nspname='public' AND c.relname=ANY(%s) AND c.relkind='r'
                               ORDER BY pg_total_relation_size(c.oid) DESC""",
                                     (["images", "image_search", "thumbnails", "image_previews", "faces", "face_groups", "people",
                                       "sort_proposals", "image_tags", "saved_views", "image_labels", "label_prototypes",
                                       "collections", "collection_images", "collection_links", "collection_settings", "place_lookup_cache"],)).fetchall()
            for relation in relations:
                relation["other_bytes"] = (relation["total_bytes"] - relation["table_bytes"]
                                           - relation["index_bytes"])
            info["relations"] = relations
            info["thumbnail_payload"] = conn.execute("""SELECT count(*) AS rows,
                               coalesce(sum(octet_length(data)),0) AS bytes FROM thumbnails""").fetchone()
            if conn.execute("SELECT to_regclass('public.image_previews') name").fetchone()['name']:
                info['preview_payload'] = conn.execute('SELECT count(*) rows,coalesce(sum(octet_length(data)),0) bytes FROM image_previews').fetchone()
            info["image_payload"] = conn.execute("""SELECT count(*) FILTER (WHERE embedding IS NOT NULL) AS embedding_rows,
                               coalesce(sum(pg_column_size(embedding)),0) AS embedding_bytes,
                               coalesce(sum(octet_length(ocr_text)),0) AS retained_ocr_text_bytes,
                               coalesce(sum(pg_column_size(exif)),0) AS exif_bytes,
                               coalesce(sum(pg_column_size(generation)),0) AS generation_bytes FROM images""").fetchone()
            info["face_embedding_bytes"] = conn.execute("""SELECT coalesce(sum(pg_column_size(embedding)),0) AS n
                                               FROM faces""").fetchone()["n"]
    except psycopg.Error as exc:
        emit({"ok": False, "error": str(exc).splitlines()[0]})
        raise typer.Exit(code=1) from exc
    from .catalogs import resolve
    catalog = resolve()
    emit({**info, 'catalog_id': catalog.id, 'catalog_name': catalog.name, 'readonly': catalog.readonly})


@app.command(hidden=True)
def scan(root: Path, prune: bool = False, limit: int | None = None):
    """Index metadata and hashes. --prune marks missing paths after a complete scan."""
    from .catalogs import require_path
    require_path(root)
    from .indexer import scan as run_scan
    initialize()
    with connect() as conn:
        emit(run_scan(conn, str(root), prune=prune, limit=limit))


@app.command(hidden=True)
def embed(limit: int | None = None, cpu: bool = False, retry_errors: bool = False):
    """Compute CLIP image vectors for semantic search."""
    from .embeddings import process_pending
    from .catalogs import resolve
    catalog = resolve()
    if not catalog.id or catalog.readonly or not catalog.roots:
        raise typer.BadParameter('Select a writable catalog with allowed folders')
    with connect() as conn:
        emit(process_pending(conn, limit=limit, cpu=cpu, retry_errors=retry_errors))


@app.command()
def search(query: str, limit: int = 30):
    """Search image paths and saved collection names."""
    with connect() as conn:
        rows = conn.execute("""SELECT i.id,i.path,i.width,i.height FROM images i
                              WHERE NOT i.missing AND (i.path ILIKE %s OR EXISTS(
                                SELECT 1 FROM collection_images m JOIN collections c ON c.id=m.collection_id
                                WHERE m.image_id=i.id AND m.source<>'excluded' AND c.status<>'dismissed'
                                AND coalesce(c.user_name,c.suggested_name) ILIKE %s)) ORDER BY i.id LIMIT %s""",
                            (f"%{query}%", f"%{query}%", limit)).fetchall()
    emit(rows)


@app.command("semantic-search")
def semantic_search(query: str, limit: int = 30, cpu: bool = False):
    """Find images whose visual embedding matches a text description."""
    from .embeddings import MODEL_ID, create_embedder
    encoder = create_embedder(cpu=cpu)
    try:
        vector = vector_literal(encoder.text(query))
    finally:
        if hasattr(encoder, "close"):
            encoder.close()
    with connect() as conn:
        rows = conn.execute("""SELECT id,path,1-(embedding <=> %s::halfvec) AS similarity
                              FROM images WHERE missing=false AND embedding_model=%s
                              ORDER BY embedding <=> %s::halfvec LIMIT %s""",
                            (vector, MODEL_ID, vector, limit)).fetchall()
    emit(rows)


@app.command()
def similar(image_id: int, limit: int = 20):
    """Find visually similar images by ID."""
    with connect() as conn:
        source = conn.execute("SELECT embedding::text AS embedding,embedding_model FROM images WHERE id=%s", (image_id,)).fetchone()
        if not source or not source["embedding"]:
            raise typer.BadParameter("Image has no embedding")
        rows = conn.execute("""SELECT id,path,1-(embedding <=> %s::halfvec) AS similarity
                              FROM images WHERE missing=false AND id<>%s AND embedding_model=%s
                              ORDER BY embedding <=> %s::halfvec LIMIT %s""",
                            (source["embedding"], image_id, source["embedding_model"], source["embedding"], limit)).fetchall()
    emit(rows)


@app.command('image-search')
def image_search(image: Path, limit: int = typer.Option(30, min=1, max=500), cpu: bool = False):
    """Find catalog images resembling a reference file, without indexing it."""
    from .image_query import encode_reference, search
    try:
        vector = encode_reference(image, cpu=cpu)
        with connect() as conn:
            emit(search(conn, vector, limit=limit))
    except (OSError, ValueError, RuntimeError) as exc:
        raise typer.BadParameter(str(exc)) from exc


@app.command()
def duplicates(near: bool = False, max_distance: int = 5, limit: int = 100):
    """List exact hash groups, or candidate near-duplicates by difference hash."""
    with connect() as conn:
        if not near:
            rows = conn.execute("""SELECT sha256,array_agg(id ORDER BY id) AS image_ids,count(*) AS count
                                  FROM images WHERE missing=false GROUP BY sha256 HAVING count(*)>1
                                  ORDER BY count(*) DESC LIMIT %s""", (limit,)).fetchall()
        else:
            # Prefix prefilter avoids a quadratic join on the whole library. These are candidates, not exhaustive.
            rows = conn.execute("""SELECT a.id AS first_id,b.id AS second_id,a.path AS first_path,b.path AS second_path,
                                         a.dhash AS first_hash,b.dhash AS second_hash
                                  FROM images a JOIN images b ON a.id<b.id AND left(a.dhash,4)=left(b.dhash,4)
                                  WHERE a.missing=false AND b.missing=false AND a.sha256<>b.sha256
                                  ORDER BY a.id,b.id LIMIT %s""", (max(limit * 50, 500),)).fetchall()
            rows = [{**row, "hamming_distance": (int(row["first_hash"], 16) ^ int(row["second_hash"], 16)).bit_count()}
                    for row in rows]
            rows = [row for row in rows if row["hamming_distance"] <= max_distance][:limit]
    emit(rows)


@app.command()
def show(image_id: int):
    """Return all indexed metadata for an image."""
    with connect() as conn:
        image = conn.execute("SELECT * FROM images WHERE id=%s", (image_id,)).fetchone()
        if not image:
            raise typer.BadParameter("Unknown image ID")
        image.pop("embedding", None)
        for key in ("ocr_text", "ocr_model", "ocr_status", "ocr_error", "ocr_at"):
            image.pop(key, None)
        image["faces"] = conn.execute("""SELECT f.id,f.x,f.y,f.width,f.height,f.group_id,f.confidence,
                                         p.name AS person,sp.name AS suggested_person
                                         FROM faces f LEFT JOIN people p ON f.person_id=p.id
                                         LEFT JOIN people sp ON f.suggested_person_id=sp.id
                                         WHERE f.image_id=%s""", (image_id,)).fetchall()
    emit(image)


@faces_app.command("detect", hidden=True)
def faces_detect(limit: int | None = None, retry_errors: bool = False):
    from .faces import process_pending
    from .catalogs import resolve
    catalog = resolve()
    if not catalog.id or catalog.readonly or not catalog.roots:
        raise typer.BadParameter('Select a writable catalog with allowed folders')
    with connect() as conn:
        emit(process_pending(conn, limit=limit, retry_errors=retry_errors))


@faces_app.command("groups")
def faces_groups(limit: int = 100):
    initialize()
    with connect() as conn:
        rows = conn.execute("""SELECT f.group_id,count(*) AS face_count,min(i.path) AS sample_path,
                                     max(p.name) AS person,max(p.id) AS person_id,
                                     max(sp.name) AS suggested_person,max(sp.id) AS suggested_person_id
                              FROM faces f JOIN images i ON i.id=f.image_id
                              LEFT JOIN people p ON p.id=f.person_id
                              LEFT JOIN people sp ON sp.id=f.suggested_person_id
                              WHERE i.missing=false AND NOT coalesce(p.ignored,false)
                              AND NOT coalesce(sp.ignored,false) GROUP BY f.group_id ORDER BY face_count DESC LIMIT %s""",
                            (limit,)).fetchall()
    emit(rows)


@faces_app.command("tag")
def faces_tag(group_id: int, name: str | None = None, person_id: int | None = None):
    from .faces import name_group
    initialize()
    with connect() as conn:
        emit(name_group(conn, group_id, name, person_id=person_id))


@faces_app.command("people")
def faces_people():
    initialize()
    with connect() as conn:
        emit(conn.execute("SELECT id,name FROM people WHERE NOT ignored ORDER BY lower(name),id").fetchall())


@faces_app.command("ignore")
def faces_ignore(group_id: int):
    from .faces import name_group
    initialize()
    with connect() as conn:
        emit(name_group(conn, group_id, ignore=True))


@sort_app.command("propose")
def sort_propose(image_id: int, destination: Path, reason: str):
    """Record a proposed destination. Never moves the source image."""
    with connect() as conn:
        source = conn.execute("SELECT id,path FROM images WHERE id=%s AND missing=false", (image_id,)).fetchone()
        if not source:
            raise typer.BadParameter("Unknown or missing image ID")
        row = conn.execute("""INSERT INTO sort_proposals(image_id,destination,reason)
                              VALUES (%s,%s,%s) ON CONFLICT(image_id,destination)
                              DO UPDATE SET reason=excluded.reason,status='pending'
                              RETURNING id,status""",
                           (image_id, normalized_path(destination), reason)).fetchone()
        conn.commit()
    emit({**row, "source": source["path"], "destination": normalized_path(destination)})


@sort_app.command("list")
def sort_list(limit: int = 100):
    with connect() as conn:
        rows = conn.execute("""SELECT p.id,i.path AS source,p.destination,p.reason,p.status,p.created_at
                              FROM sort_proposals p JOIN images i ON i.id=p.image_id
                              ORDER BY p.created_at DESC LIMIT %s""", (limit,)).fetchall()
    emit(rows)


@sort_app.command("review")
def sort_review(proposal_id: int, decision: str):
    """Mark a proposal approved or rejected; never moves a file."""
    if decision not in {"approved", "rejected"}:
        raise typer.BadParameter("Decision must be approved or rejected")
    with connect() as conn:
        row = conn.execute("UPDATE sort_proposals SET status=%s WHERE id=%s RETURNING id,status",
                           (decision, proposal_id)).fetchone()
        if not row:
            raise typer.BadParameter("Unknown proposal ID")
        conn.commit()
    emit(row)


@app.command()
def view():
    """Open the shared catalog viewer, initially showing this image database."""
    from .worker_env import model_python

    root = Path(__file__).resolve().parents[2]
    configured = os.environ.get("IMAGE_CLI_VIEWER_BIN")
    executable = Path(configured) if configured else root / "viewer" / "target" / "release" / "image-atlas-viewer.exe"
    updated = executable.with_name("image-atlas-viewer-updated.exe")
    if not configured and updated.is_file() and (not executable.is_file() or
                                                 updated.stat().st_mtime_ns > executable.stat().st_mtime_ns):
        executable = updated
    if not executable.is_file():
        raise typer.BadParameter("Build the viewer first: cargo build --release --manifest-path viewer/Cargo.toml")
    try:
        initialize()
    except psycopg.OperationalError:
        pass  # Offline viewer still opens with a visible Reconnect action.
    env = os.environ.copy()
    env['CLI_VIEWER_START_MODE'] = 'images'
    from .catalogs import resolve
    catalog = resolve()
    env['IMAGE_CLI_DSN'] = catalog.dsn
    from .db import DEFAULT_DSN
    env.setdefault('ASSET_CLI_VIEWER_DSN', os.environ.get('ASSET_CLI_DSN')
                   or DEFAULT_DSN.rsplit('/', 1)[0] + '/assetcli')
    env['IMAGE_CLI_CATALOG_ID'] = catalog.id or ''
    vision_python = model_python("IMAGE_CLI_VISION_PYTHON", ("torch", "open_clip")) or sys.executable
    env['IMAGE_CLI_VIEWER_CONTEXT'] = json.dumps({'id': catalog.id, 'name': catalog.name, 'roots': catalog.roots, 'readonly': catalog.readonly,
                                                 'vision_python': vision_python})
    env['IMAGE_CLI_ALLOWED_ROOTS'] = json.dumps(catalog.roots)
    env['IMAGE_CLI_READONLY'] = '1' if catalog.readonly else '0'
    env["IMAGE_CLI_MODULE_ROOT"] = str(root / "src")
    env["IMAGE_CLI_PYTHON"] = sys.executable
    env["IMAGE_CLI_VISION_PYTHON"] = vision_python
    subprocess.Popen([str(executable)], env=env,
                     creationflags=subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0)
    emit({"started": True, "viewer": str(executable)})


@app.command()
def gallery(host: str = "127.0.0.1", port: int = 18765, tls_cert: Path | None = None,
            tls_key: Path | None = None, allowed_host: list[str] | None = None):
    """Start the local image and face review gallery."""
    import uvicorn
    initialize()
    from .catalogs import resolve
    from .web import app as gallery_app
    gallery_app.state.initial_catalog_id = resolve().id
    from .web import configure_security
    import ipaddress
    try:
        local = ipaddress.ip_address(host).is_loopback
    except ValueError:
        local = host.lower() == 'localhost'
    if bool(tls_cert) != bool(tls_key) or (not local and not tls_cert):
        raise typer.BadParameter('Network gallery requires both --tls-cert and --tls-key; loopback does not')
    if tls_cert and (not tls_cert.is_file() or not tls_key.is_file()):
        raise typer.BadParameter('TLS certificate and key files must exist')
    configure_security(host, secure=bool(tls_cert), allowed_hosts=allowed_host or ())
    display_host = 'localhost' if host in {'0.0.0.0', '::'} else host
    if allowed_host:
        display_host = allowed_host[0]
    if ':' in display_host:
        display_host = '[' + display_host + ']'
    scheme = 'https' if tls_cert else 'http'
    print(f'Open gallery: {scheme}://{display_host}:{port}/#session={gallery_app.state.session_token}')
    uvicorn.run(gallery_app, host=host, port=port, access_log=False,
                ssl_certfile=str(tls_cert) if tls_cert else None, ssl_keyfile=str(tls_key) if tls_key else None)


previews_app = typer.Typer(help='Build and inspect cached 1024-pixel JPEG XL previews.')
app.add_typer(previews_app, name='previews')
remote_app = typer.Typer(help='Connect to a temporary remote image session over SSH.')
app.add_typer(remote_app, name='remote')


@previews_app.command('build')
def previews_build(limit: int | None = typer.Option(None, min=1)):
    from .previews import build
    from .jobs import MineJob
    initialize()
    job = MineJob('JPEG XL preview backfill')
    examined = 0
    try:
        with connect() as conn:
            def advance():
                nonlocal examined
                job.check()
                examined += 1
                job.update(preview_candidates=examined)
                if examined == 1 or examined % 25 == 0:
                    typer.echo(f'JPEG XL backfill: {examined} candidates examined', err=True)
            result = build(conn, limit=limit, progress=advance)
            emit(result)
            if result['previews_failed']:
                raise typer.Exit(code=1)
    finally:
        job.close()


@app.command('serve-images')
def serve_images(port: int = typer.Option(18766, min=1, max=65535), cpu: bool = False):
    """Serve the selected catalog until this command closes; start on the source desktop."""
    import uvicorn
    from .catalogs import resolve
    from .remote_server import create_app, connection_code
    from .process_scope import process_scope
    catalog = resolve()
    if not catalog.id:
        raise typer.BadParameter('Select a registered desktop catalog with --database')
    initialize()
    server = create_app(catalog, cpu=cpu)
    with connect() as conn:
        count = conn.execute('SELECT count(*) n FROM images').fetchone()['n']
    typer.echo(f'{catalog.name} [{catalog.id}]: {count} images')
    typer.echo('Connection code (private; expires when this command closes):')
    typer.echo(connection_code(port, server.state.token, catalog))
    typer.echo('On the other computer: image-atlas remote connect YOUR_SSH_HOST')
    with process_scope():
        uvicorn.run(server, host='127.0.0.1', port=port, access_log=False)


@remote_app.command('connect')
def remote_connect(host: str):
    from .remote_client import connect as start_session
    from .process_scope import process_scope
    code = typer.prompt('Connection code', hide_input=True)
    try:
        with process_scope():
            start_session(host, code)
    except KeyboardInterrupt:
        pass
    except (OSError, ValueError) as exc:
        raise typer.BadParameter(str(exc)) from None


@remote_app.command('search')
def remote_search(query: str = '', mode: str = 'combined', limit: int = typer.Option(100, min=1, max=500),
                  offset: int = typer.Option(0, min=0), min_pixels: int = typer.Option(0, min=0),
                  orientation: str = 'any', format: str = ''):
    from .remote_client import search
    try:
        emit(search(query, mode=mode, limit=limit, offset=offset, min_pixels=min_pixels,
                    orientation=orientation, format=format))
    except (OSError, ValueError) as exc:
        raise typer.BadParameter(str(exc)) from None


@remote_app.command('preview')
def remote_preview(image_id: int, destination: Path | None = None, thumbnail: bool = False):
    from .remote_client import download
    emit({'path': download(image_id, destination, kind='thumb' if thumbnail else 'preview')})


@remote_app.command('download')
def remote_download(image_id: int, destination: Path | None = None,
                    open_file: bool = typer.Option(False, '--open')):
    from .remote_client import download
    emit({'path': download(image_id, destination, open_file=open_file)})


@remote_app.command('disconnect')
def remote_disconnect():
    from .remote_client import request
    with request('/disconnect', data=b'{}'):
        pass
    emit({'disconnected': True})


@remote_app.command('view')
def remote_view():
    from .remote_client import active_session
    active_session()
    root = Path(__file__).resolve().parents[2]
    executable = Path(os.environ.get('IMAGE_CLI_VIEWER_BIN', root/'viewer/target/release/image-atlas-viewer.exe'))
    updated = executable.with_name('image-atlas-viewer-updated.exe')
    if not os.environ.get('IMAGE_CLI_VIEWER_BIN') and updated.exists() and (not executable.exists() or updated.stat().st_mtime_ns > executable.stat().st_mtime_ns):
        executable = updated
    if not executable.exists():
        raise typer.BadParameter('Build the native viewer first')
    env = os.environ.copy()
    for key in ('IMAGE_CLI_DSN', 'IMAGEATLAS_DB_PASSWORD', 'PGPASSWORD', 'IMAGE_CLI_VIEWER_CONTEXT'):
        env.pop(key, None)
    env.update(IMAGE_CLI_REMOTE='1', IMAGE_CLI_PYTHON=sys.executable, IMAGE_CLI_MODULE_ROOT=str(root/'src'))
    subprocess.Popen([str(executable)], env=env,
                     creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
    emit({'started': True, 'remote': True})


if __name__ == "__main__":
    app()
