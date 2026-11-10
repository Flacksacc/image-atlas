import json

from fastapi.testclient import TestClient
from PIL import Image, PngImagePlugin

from image_atlas.db import connect, initialize, normalized_path, vector_literal
from image_atlas.indexer import scan
from image_atlas.web import app


def test_generation_metadata_gallery_and_group_tag(tmp_path):
    initialize()
    image_path = tmp_path / "prompt.png"
    png = PngImagePlugin.PngInfo()
    png.add_text("prompt", json.dumps({"3": {"class_type": "KSampler"}}))
    Image.new("RGB", (40, 40), "green").save(image_path, pnginfo=png)
    with connect() as conn:
        scan(conn, str(tmp_path))
        row = conn.execute("SELECT id,generation FROM images WHERE path=%s", (normalized_path(image_path),)).fetchone()
        assert row["generation"]["prompt"]["3"]["class_type"] == "KSampler"
        assert "KSampler" in conn.execute("SELECT search_text FROM image_search WHERE image_id=%s",
                                          (row["id"],)).fetchone()["search_text"]
        conn.execute("UPDATE images SET ocr_text='secretword',ocr_status='done' WHERE id=%s", (row["id"],))
        group_id = conn.execute("INSERT INTO face_groups DEFAULT VALUES RETURNING id").fetchone()["id"]
        conn.execute("""INSERT INTO faces(image_id,x,y,width,height,embedding,group_id,confidence)
                        VALUES (%s,1,1,20,20,%s::halfvec,%s,0.99)""",
                     (row["id"], vector_literal([0.0] * 127 + [1.0]), group_id))
        proposal_id = conn.execute("""INSERT INTO sort_proposals(image_id,destination,reason)
                                       VALUES (%s,%s,%s) RETURNING id""",
                                   (row["id"], str(tmp_path / "sorted" / "prompt.png"), "Test suggestion")).fetchone()["id"]
        conn.commit()
    client = TestClient(app, base_url="http://127.0.0.1", headers={"X-Image-CLI-Session": app.state.session_token})
    home = client.get("/")
    assert home.status_code == 200
    assert "OCR" not in home.text
    listed = client.get("/api/images").json()[0]
    assert listed["id"] == row["id"]
    assert "ocr_text" not in listed
    assert client.get("/api/images?q=secretword").json() == []
    assert client.get(f"/api/images/{row['id']}/thumb").headers["content-type"] == "image/webp"
    assert client.get("/api/groups").json()[0]["face_count"] == 1
    tagged = client.post(f"/api/groups/{group_id}/tag", json={"name": "Alice"}).json()
    assert tagged["faces_named"] == 1
    assert client.post(f"/api/proposals/{proposal_id}/review", json={"decision": "approved"}).json()["status"] == "approved"
    assert image_path.exists()  # review must not move the source
    image_path.unlink()
    with connect() as conn:
        scan(conn, str(tmp_path), prune=True)
    assert client.get("/api/images").json() == []
    assert client.get("/api/images?include_missing=true").json()[0]["missing"] is True
    assert client.get(f"/api/images/{row['id']}/thumb").headers["content-type"] == "image/webp"
    with connect() as conn:
        assert conn.execute("""SELECT p.name FROM faces f JOIN people p ON p.id=f.person_id
                               WHERE f.group_id=%s""", (group_id,)).fetchone()["name"] == "Alice"
