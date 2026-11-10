"""Export a contact sheet from stored thumbnails without reading originals."""
import argparse
import io
from pathlib import Path

from PIL import Image, ImageDraw

from .db import connect


def export_sheet(conn, image_ids, destination):
    ids = list(dict.fromkeys(image_ids))
    if not ids or len(ids) > 1000:
        raise ValueError('Select between 1 and 1000 images per contact sheet')
    destination = Path(destination).expanduser().resolve()
    if destination.suffix.lower() != '.png':
        raise ValueError('Contact sheet destination must end in .png')
    if destination.exists():
        raise ValueError('Destination already exists; choose a new filename')
    rows = conn.execute('''SELECT i.id,i.path,t.data FROM images i
        LEFT JOIN thumbnails t ON t.sha256=i.sha256 AND t.version=1
        WHERE i.id=ANY(%s)''', (ids,)).fetchall()
    by_id = {row['id']: row for row in rows}
    if len(by_id) != len(ids):
        raise ValueError('Some selected images are no longer in this catalog')
    columns = min(5, len(ids))
    sheet = Image.new('RGB', (columns * 240, ((len(ids) + columns - 1) // columns) * 210), 'white')
    draw = ImageDraw.Draw(sheet)
    for index, iid in enumerate(ids):
        row = by_id[iid]
        x, y = index % columns * 240, index // columns * 210
        if row['data']:
            with Image.open(io.BytesIO(bytes(row['data']))) as image:
                image.thumbnail((220, 170))
                sheet.paste(image.convert('RGB'), (x + (240-image.width)//2, y+5))
        else:
            draw.text((x+10,y+30), 'Thumbnail unavailable', fill='black')
        # Default Pillow font supports many Unicode names; IDs always identify the source.
        draw.text((x+10,y+178), f'#{iid} {Path(row["path"]).name[:28]}', fill='black')
    # Exclusive creation prevents accidental overwrite even if another process races us.
    with destination.open('xb') as handle:
        sheet.save(handle, format='PNG')
    return destination


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('destination')
    parser.add_argument('ids')
    args = parser.parse_args()
    with connect() as conn:
        path = export_sheet(conn, [int(value) for value in args.ids.split(',')], args.destination)
    print(f'Contact sheet saved: {path}')


if __name__ == '__main__':
    main()
