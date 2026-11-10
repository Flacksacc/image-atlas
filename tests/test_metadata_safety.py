import json
import warnings

from PIL import Image, PngImagePlugin

from image_atlas import indexer
from image_atlas.db import connect, normalized_path


def test_binary_exif_null_is_indexed_without_changing_photo(tmp_path):
    photo = tmp_path / "camera.jpg"
    exif = Image.Exif()
    exif[37121] = bytes([1, 2, 3, 0])  # ComponentsConfiguration from the reported crash.
    exif[272] = "Camera model"
    Image.new("RGB", (24, 20), "blue").save(photo, exif=exif)
    original = photo.read_bytes()

    with connect() as conn:
        result = indexer.scan(conn, str(tmp_path))
        assert result["new"] == 1
        assert result["errors"] == []
        row = conn.execute("SELECT exif,camera_model FROM images").fetchone()
        assert row["exif"]["ComponentsConfiguration"] == "\x01\x02\x03\ufffd"
        assert row["camera_model"] == "Camera model"
        assert "\x00" not in conn.execute("SELECT search_text FROM image_search").fetchone()["search_text"]
        assert indexer.scan(conn, str(tmp_path))["unchanged"] == 1
    assert photo.read_bytes() == original


def test_parsed_generation_json_is_safe_for_postgres(tmp_path):
    photo = tmp_path / "generated.png"
    prompt = {"key\x00": ["text\x00", {"nested": "\ud800", "unicode": "café 😀"}],
              "number": float("nan")}
    png = PngImagePlugin.PngInfo()
    png.add_text("prompt", json.dumps(prompt))
    Image.new("RGB", (24, 20), "red").save(photo, pnginfo=png)

    with connect() as conn:
        result = indexer.scan(conn, str(tmp_path))
        assert result["new"] == 1
        assert result["errors"] == []
        generation = conn.execute("SELECT generation FROM images").fetchone()["generation"]
        assert generation["prompt"] == {
            "key\ufffd": ["text\ufffd", {"nested": "\ufffd", "unicode": "café 😀"}],
            "number": "nan",
        }
        assert "café 😀" in conn.execute("SELECT search_text FROM image_search").fetchone()["search_text"]


def test_scan_rolls_back_bad_metadata_and_continues(tmp_path, monkeypatch):
    paths = [tmp_path / name for name in ("before.png", "bad.png", "after.png")]
    for path, color in zip(paths, ("red", "blue", "green")):
        Image.new("RGB", (24, 20), color).save(path)
    real_metadata = indexer.metadata

    def invalid_metadata(path, **kwargs):
        result = real_metadata(path, **kwargs)
        if path == paths[1]:
            result["width"] = "not-a-number"
        return result

    monkeypatch.setattr(indexer, "metadata", invalid_metadata)
    ids = []
    with connect() as conn:
        result = indexer.scan(conn, str(tmp_path), paths=paths, image_ids=ids)
        assert result["new"] == 2
        assert len(result["errors"]) == 1
        rows = conn.execute("SELECT id,path FROM images ORDER BY id").fetchall()
        assert [row["path"] for row in rows] == [normalized_path(paths[0]), normalized_path(paths[2])]
        assert [row["id"] for row in rows] == ids
        assert conn.execute("SELECT count(*) AS n FROM image_search").fetchone()["n"] == 2
        assert conn.execute("SELECT count(*) AS n FROM thumbnails").fetchone()["n"] == 2


def test_large_valid_image_is_counted_and_hard_limit_is_isolated(tmp_path, monkeypatch):
    # Small fixtures exercise exactly the same Pillow warning and error paths.
    monkeypatch.setattr(Image, 'MAX_IMAGE_PIXELS', 1000)
    large, enormous, normal = [tmp_path / name for name in ('large.png','enormous.png','normal.png')]
    for path, size in [(large,(40,30)),(enormous,(50,50)),(normal,(20,20))]:
        Image.new('RGB',size,'red').save(path)
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter('always')
        with connect() as conn:
            result=indexer.scan(conn,str(tmp_path),paths=[large,enormous,normal])
            assert result['new']==2
            assert result['large_images']==1
            assert len(result['errors'])==1
            assert 'hard pixel-size limit' in result['errors'][0]['error']
            assert conn.execute('SELECT count(*) AS n FROM images').fetchone()['n']==2
    assert not any(isinstance(w.message,Image.DecompressionBombWarning) for w in caught)
    assert Image.MAX_IMAGE_PIXELS==1000
