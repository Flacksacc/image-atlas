from fastapi.testclient import TestClient
from PIL import Image

from image_atlas.db import connect, normalized_path, vector_literal
from image_atlas.gallery_sort import GROUP_FIELDS, SORT_FIELDS
from image_atlas.indexer import scan
from image_atlas.web import app


def seed_images(tmp_path):
    for n in range(8):
        Image.new("RGB", (30 + n, 32), "blue").save(tmp_path / f"image-{n}.png")
    with connect() as conn:
        scan(conn, str(tmp_path))
        rows = conn.execute("SELECT id FROM images WHERE root_path=%s ORDER BY id",
                            (normalized_path(tmp_path),)).fetchall()
        person = conn.execute("INSERT INTO people(name) VALUES ('Alice') RETURNING id").fetchone()["id"]
        group = conn.execute("INSERT INTO face_groups DEFAULT VALUES RETURNING id").fetchone()["id"]
        for n, row in enumerate(rows):
            conn.execute("""UPDATE images SET size_bytes=%s,capture_day=date '2026-01-01'+%s,
                            capture_at=timestamp '2026-01-01'+%s*interval '1 day',rating=%s,
                            camera_model=%s WHERE id=%s""",
                         (1000 + n, n % 3, n % 3, n % 6, "Camera A" if n % 2 else None, row["id"]))
            if n % 2:
                conn.execute("""INSERT INTO faces(image_id,x,y,width,height,embedding,group_id,person_id)
                                VALUES (%s,0,0,10,10,%s::halfvec,%s,%s)""",
                             (row["id"], vector_literal([1.0] + [0.0] * 127), group, person))
        conn.commit()
    return {row["id"] for row in rows}, person


def test_gallery_sorting_grouping_and_pages(tmp_path):
    expected, _ = seed_images(tmp_path)
    client = TestClient(app, base_url="http://127.0.0.1", headers={"X-Image-CLI-Session": app.state.session_token})
    for key in SORT_FIELDS:
        for direction in (True, False):
            params = {"sort": key, "descending": direction, "limit": 4}
            first = client.get("/api/images", params=params)
            assert first.status_code == 200, first.text
            second = client.get("/api/images", params={**params, "offset": 4}).json()
            rows = first.json() + second
            assert len(rows) == 8 and {row["id"] for row in rows} == expected
            if key == "Bytes":
                sizes = [row["size_bytes"] for row in rows]
                assert sizes == sorted(sizes, reverse=direction)
    for grouping in GROUP_FIELDS:
        params = {"group_by": grouping, "sort": "Bytes", "descending": False, "limit": 4}
        first = client.get("/api/images", params=params)
        assert first.status_code == 200, first.text
        rows = first.json() + client.get("/api/images", params={**params, "offset": 4}).json()
        assert len(rows) == 8 and {row["id"] for row in rows} == expected
        groups = [row["image_group"] for row in rows]
        assert groups == sorted(groups)
        for a, b in zip(rows, rows[1:]):
            if a["image_group"] == b["image_group"]:
                assert a["size_bytes"] <= b["size_bytes"]


def test_gallery_people_groups_use_identity_ids(tmp_path):
    _, person_id = seed_images(tmp_path)
    client = TestClient(app, base_url="http://127.0.0.1", headers={"X-Image-CLI-Session": app.state.session_token})
    rows = client.get("/api/images?group_by=People").json()
    assert {row["image_group"] for row in rows} == {"No faces", f"Alice (#{person_id})"}
    with connect() as conn:
        conn.execute("UPDATE people SET ignored=true WHERE id=%s", (person_id,))
        conn.commit()
    rows = client.get("/api/images?group_by=People").json()
    assert {row["image_group"] for row in rows} == {"No faces", "Ignored people"}


def test_gallery_rejects_unknown_sort_expressions(tmp_path):
    seed_images(tmp_path)
    client = TestClient(app, base_url="http://127.0.0.1", headers={"X-Image-CLI-Session": app.state.session_token})
    for key in ("sort", "secondary", "group_by"):
        assert client.get("/api/images", params={key: "id; DROP TABLE people"}).status_code == 400
    assert client.get("/api/images", params={"q": "' OR true --"}).json() == []
