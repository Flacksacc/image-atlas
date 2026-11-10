"""Private bounded preview protocol; no database imports or credentials needed."""
import argparse
import math
import sys
from contextlib import contextmanager
from pathlib import Path

from PIL import Image, ImageOps

from .documents import DOCUMENT_EXTENSIONS, document_image
from .image_io import RAW_EXTENSIONS, open_image


@contextmanager
def preview_source(path, edge, face, *, page=0):
    if path.suffix.lower() in DOCUMENT_EXTENSIONS:
        info, image = document_image(path, page=page)
        with image:
            yield image, info['width'], info['height']
    elif path.suffix.lower() in RAW_EXTENSIONS and not face:
        import rawpy
        with rawpy.imread(str(path)) as raw, Image.fromarray(raw.postprocess(
                half_size=edge < 3000, use_camera_wb=True, no_auto_bright=True)) as image:
            yield image, image.width, image.height
    else:
        with open_image(path) as (image, width, height, _):
            yield image, width, height


def pixels_from_source(source, original_width, original_height, edge, box=None):
    if not 1 <= edge <= 8192:
        raise ValueError('Preview edge must be between 1 and 8192')
    orientation = source.getexif().get(274, 1)
    if orientation in (5, 6, 7, 8):
        original_width, original_height = original_height, original_width
    source.draft('RGB', ((2048 if box else edge),) * 2)
    with ImageOps.exif_transpose(source) as image:
        if box:
            x, y, width, height = box
            sx, sy = image.width / original_width, image.height / original_height
            left = max(0, min(image.width - 1, math.floor(x * sx)))
            top = max(0, min(image.height - 1, math.floor(y * sy)))
            right = max(left + 1, min(image.width, math.ceil((x + max(1, width)) * sx)))
            bottom = max(top + 1, min(image.height, math.ceil((y + max(1, height)) * sy)))
            image = image.crop((left, top, right, bottom))
        image.thumbnail((edge, edge), Image.Resampling.LANCZOS)
        return image.convert('RGBA')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('path', type=Path)
    parser.add_argument('edge', type=int)
    parser.add_argument('box', nargs='*', type=int)
    parser.add_argument('--page', type=int, default=0)
    args = parser.parse_args()
    if len(args.box) not in (0, 4):
        parser.error('Face crop requires x y width height')
    if not 1 <= args.edge <= 8192:
        parser.error('Preview edge must be between 1 and 8192')
    box = tuple(args.box) or None
    with preview_source(args.path, args.edge, box is not None, page=args.page) as (source, width, height), \
            pixels_from_source(source, width, height, args.edge, box) as image:
        sys.stdout.buffer.write(f'{image.width} {image.height}\n'.encode('ascii'))
        sys.stdout.buffer.write(image.tobytes())


if __name__ == '__main__':
    main()
