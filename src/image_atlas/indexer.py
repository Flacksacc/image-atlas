"""Cheap first-pass image inventory. No GPU or model download is needed."""
import hashlib
import io
import json
import logging
import math
import re
from datetime import UTC, date, datetime
from pathlib import Path

import numpy as np
from PIL import ExifTags, Image, ImageOps, UnidentifiedImageError
from psycopg import DataError
from psycopg.types.json import Jsonb

from .cloud import unavailable_cloud_file
from .db import normalized_path
from .documents import DOCUMENT_EXTENSIONS, document_info, is_document_cache
from .image_io import RAW_EXTENSIONS, open_image
from .labels import LABEL_VERSION

LOG = logging.getLogger(__name__)
EXTENSIONS = {".jpg", ".jpeg", ".png", ".webp", ".tif", ".tiff", ".bmp", ".gif", ".avif", ".heic", ".heif"} | RAW_EXTENSIONS | DOCUMENT_EXTENSIONS
GENERATION_KEYS = {"prompt", "workflow", "parameters", "negative_prompt", "generation_data", "software"}
THUMBNAIL_VERSION = 1
THUMBNAIL_EDGE = 160
THUMBNAIL_QUALITY = 45


def digest(path: Path) -> str:
    from .catalogs import resolve
    catalog = resolve()
    if catalog.id:
        catalog.require_path(path)
    sha = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            sha.update(block)
    return sha.hexdigest()


def difference_hash(image: Image.Image) -> str:
    pixels = np.asarray(image.convert("L").resize((9, 8), Image.Resampling.LANCZOS))
    bits = (pixels[:, 1:] > pixels[:, :-1]).flatten()
    return format(sum(int(bit) << idx for idx, bit in enumerate(bits)), "016x")


def signed_hash(value: str | None) -> int | None:
    if not value:
        return None
    number = int(value, 16)
    return number - (1 << 64) if number >= (1 << 63) else number


def natural_filename(path: str) -> str:
    parts = re.split(r"(\d+)", Path(path).name.casefold())
    return "".join(f"#{len(chunk.lstrip('0') or '0'):04d}:{chunk.lstrip('0') or '0'}"
                   if chunk.isdigit() else chunk for chunk in parts)


def _metadata_text(value: str) -> str:
    """PostgreSQL text/JSONB cannot store NUL or unpaired UTF-16 surrogates."""
    return re.sub(r"[\x00\ud800-\udfff]", "\ufffd", value)


def _jsonable(value):
    if isinstance(value, bytes):
        return _metadata_text(value.decode("utf-8", errors="replace"))
    if isinstance(value, str):
        return _metadata_text(value)
    if isinstance(value, float) and not math.isfinite(value):
        return str(value)
    if isinstance(value, (int, float, bool)) or value is None:
        return value
    if isinstance(value, (tuple, list)):
        return [_jsonable(item) for item in value]
    if isinstance(value, dict):
        return {_metadata_text(str(key)): _jsonable(item) for key, item in value.items()}
    return _metadata_text(str(value))


def _gps_coordinate(values, ref):
    try:
        degrees = sum(float(part) / divisor for part, divisor in zip(values, (1, 60, 3600)))
        return -degrees if ref in ("S", "W") else degrees
    except (TypeError, ValueError, ZeroDivisionError):
        return None


def _capture_at(exif):
    for field in ("DateTimeOriginal", "DateTimeDigitized", "DateTime"):
        value = exif.get(field)
        if isinstance(value, str):
            try:
                return datetime.strptime(value[:19], "%Y:%m:%d %H:%M:%S")
            except ValueError:
                pass
    return None


def _capture_offset(exif):
    raw = exif.get("OffsetTimeOriginal", exif.get("OffsetTime"))
    if isinstance(raw, str) and re.fullmatch(r"[+-]\d{2}:\d{2}", raw):
        hours, minutes = int(raw[1:3]), int(raw[4:6])
        if hours <= 23 and minutes < 60:
            return (hours * 60 + minutes) * (-1 if raw[0] == "-" else 1)
    return None


def _appearance(image):
    sample = ImageOps.exif_transpose(image).convert("RGB")
    sample.thumbnail((96, 96), Image.Resampling.BILINEAR)
    rgb = np.asarray(sample, dtype=np.float32)
    mean = rgb.mean(axis=(0, 1))
    gray = rgb.mean(axis=2)
    edges = np.diff(gray, axis=0)
    sharpness = float(np.var(edges)) if edges.size else 0.0
    color = tuple(int(round(channel)) for channel in mean)
    return (color[0] << 16) | (color[1] << 8) | color[2], float(gray.mean() / 255), sharpness


def searchable_text(path: str, exif: dict, generation: dict) -> str:
    """Bound indexed text while keeping full metadata in the image record."""
    fields = [path]
    for source in (exif, generation):
        for value in source.values():
            if isinstance(value, (str, int, float)):
                fields.append(str(value))
            elif isinstance(value, (dict, list)):
                fields.append(json.dumps(value, ensure_ascii=False))
    return _metadata_text(" ".join(fields))[:8192]


def store_search_text(conn, image_id: int, path: str, exif: dict, generation: dict):
    conn.execute("""INSERT INTO image_search(image_id,search_text) VALUES (%s,%s)
                    ON CONFLICT(image_id) DO UPDATE SET search_text=excluded.search_text""",
                 (image_id, searchable_text(path, exif, generation)))


def make_thumbnail(image: Image.Image) -> tuple[bytes, int, int]:
    """Create one compact, orientation-correct preview from the first frame."""
    preview = ImageOps.exif_transpose(image)
    preview.thumbnail((THUMBNAIL_EDGE, THUMBNAIL_EDGE), Image.Resampling.LANCZOS, reducing_gap=3.0)
    has_alpha = "A" in preview.getbands() or (preview.mode == "P" and "transparency" in preview.info)
    preview = preview.convert("RGBA" if has_alpha else "RGB")
    output = io.BytesIO()
    preview.save(output, format="WEBP", quality=THUMBNAIL_QUALITY, method=4)
    return output.getvalue(), preview.width, preview.height


def thumbnail_from_path(path: Path) -> tuple[bytes, int, int]:
    if path.suffix.lower() in DOCUMENT_EXTENSIONS and document_info(path)[1] is None:
        return None
    with open_image(path) as (image, _, _, _):
        return make_thumbnail(image)


def has_thumbnail(conn, sha256: str) -> bool:
    return conn.execute("SELECT 1 FROM thumbnails WHERE sha256=%s AND version=%s",
                        (sha256, THUMBNAIL_VERSION)).fetchone() is not None


def store_thumbnail(conn, sha256: str, thumbnail: tuple[bytes, int, int]) -> bool:
    if thumbnail is None:
        return False
    data, width, height = thumbnail
    return conn.execute("""INSERT INTO thumbnails(sha256,version,width,height,mime_type,data)
                           VALUES (%s,%s,%s,%s,'image/webp',%s)
                           ON CONFLICT DO NOTHING RETURNING sha256""",
                        (sha256, THUMBNAIL_VERSION, width, height, data)).fetchone() is not None


def moved_image_id(conn, sha256: str, new_path: str) -> int | None:
    """Find one vanished path with identical bytes; live copies keep separate records."""
    candidates = conn.execute("SELECT id,path FROM images WHERE sha256=%s AND path<>%s",
                              (sha256, new_path)).fetchall()
    vanished = []
    for candidate in candidates:
        try:
            Path(candidate["path"]).stat()
        except FileNotFoundError:
            vanished.append(candidate["id"])
            if len(vanished) > 1:
                return None
        except OSError:
            # An inaccessible drive or path is not evidence that the file moved.
            return None
    return vanished[0] if len(vanished) == 1 else None


def metadata(path: Path, *, include_thumbnail: bool = False) -> dict:
    document = None
    if path.suffix.lower() in DOCUMENT_EXTENSIONS:
        document, preview = document_info(path)
        if preview is None:
            # An opaque Illustrator document is still a valid inventory record.
            return {'width': document.get('width'), 'height': document.get('height'),
                    'format': document['format'], 'exif': {'Document': _jsonable(document)}, 'generation': {},
                    'dhash': None, 'capture_at': None, 'capture_offset_minutes': None,
                    'camera_make': None, 'camera_model': None, 'lens': None, 'gps_lat': None, 'gps_lon': None,
                    'has_alpha': None, 'animated': False, 'is_raw': False,
                    'high_bit_depth': document.get('bit_depth', 8) > 8,
                    'dominant_rgb': None, 'brightness': None, 'sharpness': None}
    with open_image(path) as (image, original_width, original_height, original_format):
        raw_exif = image.getexif()
        exif = {ExifTags.TAGS.get(key, str(key)): _jsonable(value) for key, value in raw_exif.items()}
        try:
            camera_ifd = image.getexif().get_ifd(ExifTags.IFD.Exif)
            exif.update({ExifTags.TAGS.get(key, str(key)): _jsonable(value)
                         for key, value in camera_ifd.items()})
        except (OSError, ValueError, KeyError, TypeError):
            pass
        gps = {}
        try:
            gps = {ExifTags.GPSTAGS.get(key, str(key)): value
                   for key, value in raw_exif.get_ifd(ExifTags.IFD.GPSInfo).items()}
        except (OSError, ValueError, KeyError, TypeError):
            pass
        lat = _gps_coordinate(gps.get("GPSLatitude"), gps.get("GPSLatitudeRef"))
        lon = _gps_coordinate(gps.get("GPSLongitude"), gps.get("GPSLongitudeRef"))
        if lat is not None and not -90 <= lat <= 90:
            lat = None
        if lon is not None and not -180 <= lon <= 180:
            lon = None
        generation = {}
        for key, value in image.info.items():
            if key.lower() in GENERATION_KEYS:
                decoded = _jsonable(value)
                if isinstance(decoded, str) and decoded[:1] in ("{", "["):
                    try:
                        decoded = json.loads(decoded)
                    except json.JSONDecodeError:
                        pass
                # JSON escapes can introduce unsupported characters after parsing.
                generation[_metadata_text(key)] = _jsonable(decoded)
        if raw_exif.get(274, 1) in (5, 6, 7, 8):
            original_width, original_height = original_height, original_width
        color, brightness, sharpness = _appearance(image)
        result = {"width": original_width, "height": original_height, "format": original_format,
                  "large_image": bool(Image.MAX_IMAGE_PIXELS and original_width*original_height > Image.MAX_IMAGE_PIXELS),
                  "exif": exif, "generation": generation, "dhash": difference_hash(image),
                  "capture_at": _capture_at(exif), "capture_offset_minutes": _capture_offset(exif), "camera_make": exif.get("Make"),
                  "camera_model": exif.get("Model"), "lens": exif.get("LensModel"),
                  "gps_lat": lat, "gps_lon": lon,
                  "has_alpha": "A" in image.getbands() or "transparency" in image.info,
                  "animated": getattr(image, "n_frames", 1) > 1,
                  "is_raw": path.suffix.lower() in RAW_EXTENSIONS,
                  "high_bit_depth": path.suffix.lower() in RAW_EXTENSIONS or image.mode in {"I;16", "I", "F"} or (isinstance(image.info.get("bits"), int) and image.info["bits"] > 8),
                  "dominant_rgb": color, "brightness": brightness, "sharpness": sharpness}
        if include_thumbnail:
            result["thumbnail"] = make_thumbnail(image)
        if document is not None:
            result['exif']['Document'] = _jsonable(document)
            result['high_bit_depth'] = document.get('bit_depth', 8) > 8
        return result


def _preview_unavailable(exif):
    return exif.get('Document', {}).get('preview_available') is False


def photo_day(exif: dict, mtime_ns: int) -> tuple[str, str]:
    """Use the camera's calendar day, falling back to the local file date."""
    for field in ("DateTimeOriginal", "DateTimeDigitized", "DateTime"):
        value = exif.get(field)
        if not isinstance(value, str):
            continue
        try:
            return date.fromisoformat(value[:10].replace(":", "-")).isoformat(), "exif"
        except ValueError:
            continue
    return datetime.fromtimestamp(mtime_ns / 1_000_000_000, tz=UTC).astimezone().date().isoformat(), "mtime"


def iter_images(root: Path):
    from .catalogs import resolve, under
    catalog = resolve()
    if catalog.id:
        catalog.require_path(root)
    if root.is_file():
        if root.suffix.lower() in EXTENSIONS and not is_document_cache(root):
            yield root
        return
    for path in root.rglob("*"):
        if catalog.id and not any(under(path, p) for p in catalog.roots):
            continue
        if path.is_file() and path.suffix.lower() in EXTENSIONS and not is_document_cache(path):
            yield path


def select_images_for_mine(conn, root: Path, tags: tuple[str, ...], limit: int, *,
                           retry_errors: bool = False, progress=None, selected_progress=None,
                           candidates=None) -> tuple[list[Path], int, int]:
    """Choose the next images needing requested work without decoding earlier completed files."""
    from .previews import VERSION as PREVIEW_VERSION
    selected = []
    examined = 0
    skipped_cloud = 0
    allowed = {"pending", "error"} if retry_errors else {"pending"}
    for path in candidates if candidates is not None else iter_images(root):
        from .catalogs import resolve
        catalog = resolve()
        if catalog.id:
            catalog.require_path(path)
        examined += 1
        if progress is not None:
            progress()
        if unavailable_cloud_file(path):
            skipped_cloud += 1
            continue
        key = normalized_path(path)
        row = conn.execute("""SELECT i.size_bytes,i.mtime_ns,i.missing,i.metadata_version,
                            i.embedding_status,i.face_status,i.labels_status,i.labels_version,i.exif,
                            (t.sha256 IS NOT NULL) AS has_thumbnail,
                            EXISTS(SELECT 1 FROM image_previews p WHERE p.sha256=i.sha256
                              AND p.version=%s AND p.status IN ('done','unsupported')) AS has_preview
                            FROM images i LEFT JOIN thumbnails t ON t.sha256=i.sha256 AND t.version=%s
                            WHERE i.path=%s""", (PREVIEW_VERSION, THUMBNAIL_VERSION, key)).fetchone()
        try:
            stat = path.stat()
        except OSError:
            selected.append(path)
            if selected_progress is not None:
                selected_progress()
            if len(selected) >= limit:
                break
            continue
        needs_scan = (not row or row["missing"] or row["size_bytes"] != stat.st_size
                      or row["mtime_ns"] != stat.st_mtime_ns
                      or (not row["has_thumbnail"] and not _preview_unavailable(row['exif']))
                      or row["metadata_version"] < 5 or not row["has_preview"])
        needs_stage = any(row and (
            (tag == "labels" and (row["embedding_status"] == "done" or "embeddings" in tags)
             and (row["labels_status"] in allowed or
                  (row["labels_status"] == "done" and row["labels_version"] < LABEL_VERSION)))
            or (tag != "labels" and row[{"embeddings": "embedding_status",
                                               "faces": "face_status"}[tag]] in allowed))
                          for tag in tags)
        if needs_scan or needs_stage:
            selected.append(path)
            if selected_progress is not None:
                selected_progress()
            if len(selected) >= limit:
                break
    return selected, examined, skipped_cloud


def scan(conn, root: str, *, prune: bool = False, limit: int | None = None, progress=None,
         paths=None, image_ids: list[int] | None = None, virtual_source: bool = False,
         counts: dict | None = None) -> dict:
    from . import previews
    root_path = Path(normalized_path(root))
    if not virtual_source and not root_path.exists():
        raise FileNotFoundError(root_path)
    root_key = str(root_path)
    counts = counts if counts is not None else {}
    counts.update({"new": 0, "moved": 0, "updated": 0, "unchanged": 0,
                   "thumbnails_created": 0, "skipped_cloud": 0, "large_images": 0,
                   "metadata_only": 0, "preview_unavailable": [], "errors": []})
    counts.update(previews.counters())
    seen = set()
    for number, path in enumerate(paths if paths is not None else iter_images(root_path)):
        from .catalogs import resolve
        catalog = resolve()
        if catalog.id:
            catalog.require_path(path)
        if limit is not None and number >= limit:
            break
        key = normalized_path(path)
        if virtual_source:
            root_key = str(Path(key).parent)
        seen.add(key)
        if unavailable_cloud_file(path):
            counts["skipped_cloud"] += 1
            counts["previews_pending"] += 1
            continue
        completed = False
        try:
            stat = path.stat()
            old = conn.execute("SELECT id,root_path,size_bytes,mtime_ns,missing,sha256,dhash,metadata_version,exif FROM images WHERE path=%s",
                               (key,)).fetchone()
            if virtual_source and old:
                root_key = old["root_path"]
            sha = None
            if old and old["size_bytes"] == stat.st_size and old["mtime_ns"] == stat.st_mtime_ns and not old["missing"]:
                if not old["sha256"] or has_thumbnail(conn, old["sha256"]) or _preview_unavailable(old['exif']) or old['metadata_version'] < 5:
                    if old["metadata_version"] < 2:
                        enriched = metadata(path)
                        conn.execute("""UPDATE images SET capture_at=%s,camera_make=%s,camera_model=%s,
                            lens=%s,gps_lat=%s,gps_lon=%s,has_alpha=%s,animated=%s,
                            is_raw=%s,high_bit_depth=%s,dominant_rgb=%s,brightness=%s,sharpness=%s,
                            dhash_num=%s,filename_sort=%s,metadata_version=5 WHERE id=%s""",
                            tuple(enriched[field] for field in ("capture_at", "camera_make", "camera_model",
                              "lens", "gps_lat", "gps_lon", "has_alpha", "animated", "is_raw", "high_bit_depth", "dominant_rgb",
                              "brightness", "sharpness")) + (signed_hash(enriched["dhash"]), natural_filename(key), old["id"]),)
                    elif old["metadata_version"] < 3:
                        conn.execute("UPDATE images SET dhash_num=%s,filename_sort=%s,metadata_version=5 WHERE id=%s",
                                     (signed_hash(old["dhash"]), natural_filename(key), old["id"]))
                    elif old["metadata_version"] < 5:
                        conn.execute("UPDATE images SET filename_sort=%s,metadata_version=5 WHERE id=%s",
                                     (natural_filename(key), old["id"]))
                    if old['sha256'] and not has_thumbnail(conn, old['sha256']) and not _preview_unavailable(old['exif']):
                        if store_thumbnail(conn, old['sha256'], thumbnail_from_path(path)):
                            counts['thumbnails_created'] += 1
                    if old["root_path"] != root_key:
                        conn.execute("UPDATE images SET root_path=%s WHERE id=%s", (root_key, old["id"]))
                        conn.commit()
                    if image_ids is not None:
                        image_ids.append(old["id"])
                    counts["unchanged"] += 1
                    completed = True
                    continue
                sha = digest(path)
                if sha == old["sha256"]:
                    if store_thumbnail(conn, sha, thumbnail_from_path(path)):
                        counts["thumbnails_created"] += 1
                    if old["root_path"] != root_key:
                        conn.execute("UPDATE images SET root_path=%s WHERE id=%s", (root_key, old["id"]))
                    conn.commit()
                    if image_ids is not None:
                        image_ids.append(old["id"])
                    counts["unchanged"] += 1
                    completed = True
                    continue
            if sha is None:
                sha = digest(path)
            if old is None:
                moved_id = moved_image_id(conn, sha, key)
                if moved_id is not None:
                    if not has_thumbnail(conn, sha) and store_thumbnail(conn, sha, thumbnail_from_path(path)):
                        counts["thumbnails_created"] += 1
                    conn.execute("""UPDATE images SET path=%s,root_path=%s,folder_path=%s,
                                    size_bytes=%s,mtime_ns=%s,filename_sort=%s,indexed_at=now(),missing=false
                                    WHERE id=%s""",
                                 (key, root_key, str(Path(key).parent), stat.st_size, stat.st_mtime_ns,
                                  natural_filename(key), moved_id))
                    old_info = conn.execute("SELECT exif,generation FROM images WHERE id=%s",
                                            (moved_id,)).fetchone()
                    day, source = photo_day(old_info["exif"], stat.st_mtime_ns)
                    conn.execute("UPDATE images SET capture_day=%s,date_source=%s WHERE id=%s",
                                 (day, source, moved_id))
                    store_search_text(conn, moved_id, key, old_info["exif"], old_info["generation"])
                    conn.commit()
                    if image_ids is not None:
                        image_ids.append(moved_id)
                    counts["moved"] += 1
                    completed = True
                    continue
            needs_thumbnail = not has_thumbnail(conn, sha)
            info = metadata(path, include_thumbnail=needs_thumbnail)
            if info.get('large_image'):
                counts['large_images'] += 1
            indexed = conn.execute("""
                INSERT INTO images(path,root_path,folder_path,capture_day,date_source,size_bytes,mtime_ns,sha256,dhash,dhash_num,filename_sort,width,height,format,exif,generation,
                                   capture_at,camera_make,camera_model,lens,gps_lat,gps_lon,has_alpha,animated,is_raw,high_bit_depth,dominant_rgb,brightness,sharpness,metadata_version,
                                   ocr_status,embedding_status,face_status,labels_status)
                VALUES (%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,5,'pending','pending','pending','pending')
                ON CONFLICT(path) DO UPDATE SET root_path=excluded.root_path,folder_path=excluded.folder_path,
                    capture_day=excluded.capture_day,date_source=excluded.date_source,size_bytes=excluded.size_bytes,
                    mtime_ns=excluded.mtime_ns,sha256=excluded.sha256,dhash=excluded.dhash,
                    dhash_num=excluded.dhash_num,
                    filename_sort=excluded.filename_sort,
                    width=excluded.width,height=excluded.height,format=excluded.format,
                    exif=excluded.exif,generation=excluded.generation,indexed_at=now(),missing=false,
                    capture_at=excluded.capture_at,camera_make=excluded.camera_make,
                    camera_model=excluded.camera_model,lens=excluded.lens,
                    gps_lat=excluded.gps_lat,gps_lon=excluded.gps_lon,
                    has_alpha=excluded.has_alpha,animated=excluded.animated,
                    is_raw=excluded.is_raw,high_bit_depth=excluded.high_bit_depth,
                    dominant_rgb=excluded.dominant_rgb,brightness=excluded.brightness,
                    sharpness=excluded.sharpness,metadata_version=5,
                    ocr_text=NULL,ocr_model=NULL,ocr_status='pending',ocr_error=NULL,ocr_at=NULL,
                    embedding=NULL,embedding_model=NULL,embedding_status='pending',embedding_error=NULL,
                    face_status='pending',face_error=NULL,labels_status='pending',labels_version=0
                RETURNING id
            """, (key, root_key, str(Path(key).parent), *photo_day(info["exif"], stat.st_mtime_ns),
                  stat.st_size, stat.st_mtime_ns, sha, info["dhash"], signed_hash(info["dhash"]),
                  natural_filename(key), info["width"],
                  info["height"], info["format"], Jsonb(info["exif"]), Jsonb(info["generation"]),
                  info["capture_at"], info["camera_make"], info["camera_model"], info["lens"],
                  info["gps_lat"], info["gps_lon"], info["has_alpha"], info["animated"],
                  info["is_raw"], info["high_bit_depth"],
                  info["dominant_rgb"], info["brightness"], info["sharpness"])).fetchone()
            conn.execute("UPDATE images SET capture_offset_minutes=%s WHERE id=%s",
                         (info["capture_offset_minutes"], indexed["id"]))
            if _preview_unavailable(info['exif']):
                reason = info['exif']['Document'].get('preview_reason', 'No document preview available')
                conn.execute("""UPDATE images SET embedding_status='skipped',embedding_error=%s,
                                face_status='skipped',face_error=%s,labels_status='skipped' WHERE id=%s""",
                             (reason, reason, indexed['id']))
                counts['metadata_only'] += 1
                counts['preview_unavailable'].append({'path': key, 'reason': reason})
            if needs_thumbnail and store_thumbnail(conn, sha, info.get("thumbnail")):
                counts["thumbnails_created"] += 1
            store_search_text(conn, indexed["id"], key, info["exif"], info["generation"])
            if old:
                conn.execute("DELETE FROM faces WHERE image_id=%s", (old["id"],))
                conn.execute("DELETE FROM image_labels WHERE image_id=%s", (old["id"],))
                if old["sha256"] != sha:
                    conn.execute("""DELETE FROM thumbnails t WHERE t.sha256=%s AND NOT EXISTS
                                    (SELECT 1 FROM images i WHERE i.sha256=t.sha256)""", (old["sha256"],))
            conn.commit()
            if image_ids is not None:
                image_ids.append(indexed["id"])
            counts["updated" if old else "new"] += 1
            completed = True
        except (OSError, UnidentifiedImageError, ValueError, RuntimeError, DataError) as exc:
            conn.rollback()
            if progress is None:
                LOG.warning("Could not index %s: %s", path, exc)
            counts["errors"].append({"path": key, "error": str(exc)})
        finally:
            if completed:
                conn.commit()
                preview_row = conn.execute('SELECT sha256 FROM images WHERE path=%s', (key,)).fetchone()
                if preview_row and preview_row['sha256']:
                    previews.ensure(conn, preview_row['sha256'], path, counts)
            if progress is not None:
                progress()
    if prune and limit is None and paths is None:
        rows = conn.execute("SELECT id,path FROM images WHERE root_path=%s AND missing=false", (root_key,)).fetchall()
        for row in rows:
            if row["path"] not in seen:
                conn.execute("UPDATE images SET missing=true WHERE id=%s", (row["id"],))
        conn.commit()
    return counts
