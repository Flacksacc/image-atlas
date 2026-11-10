import pytest
from PIL import Image

from image_atlas.db import connect
from image_atlas.indexer import scan
from image_atlas.viewer_export import export_sheet


def test_sheet_uses_cached_thumbnails_after_original_is_removed(tmp_path):
    original = tmp_path / '東京 · café.png'
    Image.new('RGB', (120, 30), 'red').save(original)
    with connect() as conn:
        scan(conn, str(tmp_path))
        iid = conn.execute('SELECT id FROM images').fetchone()['id']
        original.unlink()
        output = export_sheet(conn, [iid], tmp_path / 'sheet.png')
        with Image.open(output) as sheet:
            assert sheet.size == (240, 210)
            assert sheet.getpixel((120, 20))[0] > 200
        with pytest.raises(ValueError, match='already exists'):
            export_sheet(conn, [iid], output)


def test_sheet_rejects_wrong_scope_and_unbounded_output(tmp_path):
    with connect() as conn:
        with pytest.raises(ValueError, match='no longer in this catalog'):
            export_sheet(conn, [999999], tmp_path / 'sheet.png')
        with pytest.raises(ValueError, match='between 1 and 1000'):
            export_sheet(conn, list(range(1001)), tmp_path / 'large.png')
        with pytest.raises(ValueError, match='end in .png'):
            export_sheet(conn, [1], tmp_path / 'sheet.jpg')
