import json

from PIL import Image
from cli_helpers import JsonCliRunner

from image_atlas.cli import app
from image_atlas.db import connect
from image_atlas.indexer import scan


def test_database_check_and_storage_report(tmp_path):
    image = tmp_path / "sample.png"
    Image.new("RGB", (48, 36), "blue").save(image)
    with connect() as conn:
        scan(conn, str(tmp_path))
        expected_thumb_bytes = conn.execute("SELECT octet_length(data) AS n FROM thumbnails").fetchone()["n"]
        assert conn.execute("""SELECT 1 FROM information_schema.columns
                               WHERE table_name='images' AND column_name='search_vector'""").fetchone() is None
    runner = JsonCliRunner()
    check = runner.invoke(app, ["db", "check"])
    assert check.exit_code == 0, check.output
    checked = json.loads(check.stdout)
    assert checked["ok"] is True
    assert checked["images"] == 1
    assert "vector" in checked["extensions"]

    storage = runner.invoke(app, ["db", "storage"])
    assert storage.exit_code == 0, storage.output
    sizes = json.loads(storage.stdout)
    assert sizes["database_bytes"] > 0
    assert sizes["thumbnail_payload"] == {"rows": 1, "bytes": expected_thumb_bytes}
    assert {item["name"] for item in sizes["relations"]} >= {"images", "thumbnails"}
    assert all(item["total_bytes"] >= item["table_bytes"] + item["index_bytes"]
               for item in sizes["relations"])
