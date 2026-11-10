
from PIL import Image
from cli_helpers import JsonCliRunner

from image_atlas.cli import app
from image_atlas.db import connect, initialize, normalized_path, vector_literal
from image_atlas.indexer import scan


def test_scan_change_and_prune(tmp_path):
    initialize()
    photo = tmp_path / "red-car.png"
    Image.new("RGB", (24, 20), "blue").save(photo)
    with connect() as conn:
        first = scan(conn, str(tmp_path), prune=True)
        assert first["new"] == 1
        assert first["thumbnails_created"] == 1
        row = conn.execute("SELECT * FROM images WHERE path=%s", (normalized_path(photo),)).fetchone()
        original_sha = row["sha256"]
        thumb = conn.execute("SELECT width,height,mime_type,data FROM thumbnails WHERE sha256=%s",
                             (original_sha,)).fetchone()
        assert (thumb["width"], thumb["height"], thumb["mime_type"]) == (24, 20, "image/webp")
        assert bytes(thumb["data"])[:4] == b"RIFF"
        assert (row["width"], row["height"], row["ocr_status"]) == (24, 20, "pending")
        assert str(row["id"]) in JsonCliRunner().invoke(app, ["search", "red"]).stdout
        conn.execute("UPDATE images SET ocr_text='old text',ocr_status='done' WHERE id=%s", (row["id"],))
        conn.commit()
        repeat = scan(conn, str(tmp_path))
        assert (repeat["unchanged"], repeat["thumbnails_created"]) == (1, 0)
        Image.new("RGB", (26, 22), "red").save(photo)
        updated = scan(conn, str(tmp_path))
        assert (updated["updated"], updated["thumbnails_created"]) == (1, 1)
        assert conn.execute("SELECT count(*) AS n FROM thumbnails").fetchone()["n"] == 1
        assert not conn.execute("SELECT 1 FROM thumbnails WHERE sha256=%s", (original_sha,)).fetchone()
        changed = conn.execute("SELECT ocr_text,ocr_status,width FROM images WHERE id=%s", (row["id"],)).fetchone()
        assert changed == {"ocr_text": None, "ocr_status": "pending", "width": 26}
        photo.unlink()
        scan(conn, str(tmp_path), prune=True)
        assert conn.execute("SELECT missing FROM images WHERE id=%s", (row["id"],)).fetchone()["missing"]


def test_duplicates_share_thumbnail_and_rescan_backfills(tmp_path):
    initialize()
    one = tmp_path / "one.png"
    two = tmp_path / "two.png"
    Image.new("RGB", (600, 450), "orange").save(one)
    two.write_bytes(one.read_bytes())
    with connect() as conn:
        result = scan(conn, str(tmp_path))
        assert result["thumbnails_created"] == 1
        assert conn.execute("SELECT count(*) AS n FROM thumbnails").fetchone()["n"] == 1
        thumb = conn.execute("SELECT width,height FROM thumbnails").fetchone()
        assert (thumb["width"], thumb["height"]) == (160, 120)
        conn.execute("DELETE FROM thumbnails")
        conn.commit()
        result = scan(conn, str(tmp_path))
        assert (result["unchanged"], result["thumbnails_created"]) == (2, 1)


def test_moved_image_keeps_analysis_and_people_across_roots(tmp_path):
    initialize()
    first_root = tmp_path / "first"
    second_root = tmp_path / "second"
    first_root.mkdir()
    second_root.mkdir()
    original = first_root / "portrait.png"
    moved = second_root / "portrait.png"
    Image.new("RGB", (24, 20), "purple").save(original)
    with connect() as conn:
        assert scan(conn, str(first_root))["new"] == 1
        image_id = conn.execute("SELECT id FROM images WHERE path=%s",
                                (normalized_path(original),)).fetchone()["id"]
        person_id = conn.execute("INSERT INTO people(name) VALUES ('Alice') RETURNING id").fetchone()["id"]
        group_id = conn.execute("INSERT INTO face_groups DEFAULT VALUES RETURNING id").fetchone()["id"]
        face_id = conn.execute("""INSERT INTO faces(image_id,x,y,width,height,embedding,group_id,person_id)
                                  VALUES (%s,1,2,3,4,%s,%s,%s) RETURNING id""",
                               (image_id, vector_literal([0.0] * 127 + [1.0]), group_id,
                                person_id)).fetchone()["id"]
        proposal_id = conn.execute("""INSERT INTO sort_proposals(image_id,destination,reason)
                                      VALUES (%s,'portraits','Alice') RETURNING id""",
                                   (image_id,)).fetchone()["id"]
        conn.execute("""UPDATE images SET ocr_text='Alice',ocr_status='done',
                      embedding=%s,embedding_status='done',face_status='done' WHERE id=%s""",
                     (vector_literal([0.0] * 511 + [1.0]), image_id))
        conn.commit()
        original.rename(moved)
        ids = []
        result = scan(conn, str(second_root), image_ids=ids)
        assert (result["moved"], result["new"], result["thumbnails_created"]) == (1, 0, 0)
        assert ids == [image_id]
        assert conn.execute("SELECT count(*) AS n FROM images").fetchone()["n"] == 1
        assert conn.execute("SELECT count(*) AS n FROM thumbnails").fetchone()["n"] == 1
        assert conn.execute("SELECT search_text FROM image_search WHERE image_id=%s",
                            (image_id,)).fetchone()["search_text"].startswith(normalized_path(moved))
        row = conn.execute("""SELECT path,root_path,missing,ocr_text,ocr_status,
                            embedding_status,face_status,embedding IS NOT NULL AS has_embedding
                            FROM images WHERE id=%s""", (image_id,)).fetchone()
        assert row == {"path": normalized_path(moved), "root_path": normalized_path(second_root),
                       "missing": False, "ocr_text": "Alice", "ocr_status": "done",
                       "embedding_status": "done", "face_status": "done", "has_embedding": True}
        assert conn.execute("SELECT id,person_id FROM faces WHERE image_id=%s",
                            (image_id,)).fetchone() == {"id": face_id, "person_id": person_id}
        assert conn.execute("SELECT id FROM sort_proposals WHERE image_id=%s",
                            (image_id,)).fetchone()["id"] == proposal_id


def test_live_copy_gets_separate_record_and_shared_thumbnail(tmp_path):
    initialize()
    first_root = tmp_path / "first"
    second_root = tmp_path / "second"
    first_root.mkdir()
    second_root.mkdir()
    original = first_root / "copy.png"
    copy = second_root / "copy.png"
    Image.new("RGB", (24, 20), "green").save(original)
    with connect() as conn:
        scan(conn, str(first_root))
        copy.write_bytes(original.read_bytes())
        result = scan(conn, str(second_root))
        assert (result["new"], result["moved"], result["thumbnails_created"]) == (1, 0, 0)
        assert conn.execute("SELECT count(*) AS n FROM images").fetchone()["n"] == 2
        assert conn.execute("SELECT count(*) AS n FROM thumbnails").fetchone()["n"] == 1


def test_photo_day_prefers_valid_exif_and_falls_back_to_file_date():
    from datetime import UTC, datetime

    from image_atlas.indexer import photo_day

    timestamp = int(datetime(2023, 6, 7, 12, 0, 0, tzinfo=UTC).timestamp() * 1_000_000_000)
    assert photo_day({"DateTimeOriginal": "2024:05:06 08:30:00"}, timestamp) == ("2024-05-06", "exif")
    assert photo_day({"DateTimeOriginal": "invalid"}, timestamp) == ("2023-06-07", "mtime")
