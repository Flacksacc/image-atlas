"""Read a user-selected reference image without importing it into a catalog."""
import math
from contextlib import contextmanager
from pathlib import Path

from PIL import Image, ImageOps, UnidentifiedImageError

from .cloud import unavailable_cloud_file

MAX_BYTES = 32 * 1024 * 1024
MAX_PIXELS = 40_000_000


@contextmanager
def open_reference(path):
    path = Path(path).expanduser()
    if not path.is_file():
        raise ValueError('Choose an existing image file')
    if unavailable_cloud_file(path):
        raise ValueError('Make the reference image available locally before searching')
    if path.stat().st_size > MAX_BYTES:
        raise ValueError('Reference images must be 32 MiB or smaller')
    # Register the same optional Pillow decoders used by catalog images.
    from . import image_io  # noqa: F401
    try:
        with Image.open(path) as source:
            if source.width * source.height > MAX_PIXELS:
                raise ValueError('Reference images must be 40 megapixels or smaller')
            image = ImageOps.exif_transpose(source).convert('RGB')
            image.thumbnail((2048, 2048), Image.Resampling.LANCZOS)
            try:
                yield image
            finally:
                image.close()
    except (UnidentifiedImageError, Image.DecompressionBombError) as exc:
        raise ValueError('Unsupported or oversized reference image; try JPEG, PNG, or WebP') from exc


def validate_vector(values):
    if not isinstance(values, (list, tuple)) or len(values) != 512:
        raise ValueError('An image search needs a 512-dimensional OpenCLIP vector')
    if any(isinstance(value, bool) or not isinstance(value, (int, float))
           or not math.isfinite(value) or abs(value) > 1.0001 for value in values):
        raise ValueError('Invalid image search vector')
    if sum(value * value for value in values) <= 1e-12:
        raise ValueError('Image search vector cannot be zero')
    return [float(value) for value in values]


def encode_reference(path, *, cpu=False):
    # Validate and decode before loading a model, so invalid inputs fail promptly.
    with open_reference(path):
        pass
    from .embeddings import create_embedder
    encoder = create_embedder(cpu=cpu)
    try:
        return validate_vector(encoder.reference_image(Path(path)))
    finally:
        if hasattr(encoder, 'close'):
            encoder.close()


def search(conn, vector, *, limit=100, offset=0, include_missing=False):
    from .db import vector_literal
    from .embeddings import MODEL_ID
    if not 1 <= limit <= 500 or offset < 0:
        raise ValueError('Use a limit from 1 to 500 and a nonnegative offset')
    value = vector_literal(validate_vector(vector))
    return conn.execute('''SELECT id,path,sha256,width,height,format,size_bytes,
        capture_day,capture_at,camera_model,rating,review_state,
        1-(embedding <=> %s::halfvec) AS similarity,count(*) OVER() AS total
        FROM images WHERE (%s OR NOT missing) AND embedding IS NOT NULL AND embedding_model=%s
        ORDER BY embedding <=> %s::halfvec,id LIMIT %s OFFSET %s''',
        (value, include_missing, MODEL_ID, value, limit, offset)).fetchall()
