"""Image decoding shared by indexing and vision workers; no database dependencies."""
import io
import sys
import warnings
from contextlib import contextmanager
from pathlib import Path

from PIL import Image

from .documents import DOCUMENT_EXTENSIONS, document_image

HEIF_AVAILABLE = False
try:
    from pillow_heif import register_heif_opener
    register_heif_opener()
    HEIF_AVAILABLE = True
except ImportError:
    pass

RAW_EXTENSIONS = {'.dng', '.cr2', '.nef', '.arw', '.raf', '.rw2', '.orf', '.pef'}
HEIF_EXTENSIONS = {'.heic', '.heif'}


@contextmanager
def _checked_image(source):
    with warnings.catch_warnings():
        warnings.simplefilter('ignore', Image.DecompressionBombWarning)
        try:
            image = Image.open(source)
        except Image.DecompressionBombError as exc:
            raise ValueError('Image exceeds the hard pixel-size limit; skipped to avoid excessive memory use') from exc
    with image:
        yield image


@contextmanager
def open_image(path: Path):
    """Yield a PIL image, original dimensions, and format; RAW uses its preview."""
    # Model workers deliberately never import the database/registry modules.
    catalogs = sys.modules.get('image_atlas.catalogs')
    if catalogs is not None:
        catalog = catalogs.resolve()
        if catalog.id:
            catalog.require_path(path)
    if path.suffix.lower() in DOCUMENT_EXTENSIONS:
        info, image = document_image(path)
        with image:
            yield image, info['width'], info['height'], info['format']
        return
    if path.suffix.lower() not in RAW_EXTENSIONS:
        with _checked_image(path) as image:
            yield image, image.width, image.height, image.format
        return
    try:
        import rawpy
    except ImportError as exc:
        raise RuntimeError("RAW support requires: pip install -e '.[raw]'") from exc
    try:
        source = rawpy.imread(str(path))
    except rawpy.LibRawError as exc:
        raise ValueError(f'Could not decode RAW image: {exc}') from exc
    with source as raw:
        try:
            thumb = raw.extract_thumb()
        except rawpy.LibRawNoThumbnailError:
            with Image.fromarray(raw.postprocess(half_size=True)) as image:
                yield image, raw.sizes.width, raw.sizes.height, path.suffix[1:].upper()
        else:
            if thumb.format == rawpy.ThumbFormat.JPEG:
                with _checked_image(io.BytesIO(thumb.data)) as image:
                    yield image, raw.sizes.width, raw.sizes.height, path.suffix[1:].upper()
            else:
                with Image.fromarray(thumb.data) as image:
                    yield image, raw.sizes.width, raw.sizes.height, path.suffix[1:].upper()
