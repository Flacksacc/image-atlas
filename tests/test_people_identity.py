import json

from fastapi.testclient import TestClient
from PIL import Image
from cli_helpers import JsonCliRunner

from image_atlas.cli import app as cli
from image_atlas.db import connect, normalized_path, vector_literal
from image_atlas.faces import _group_for, process_pending
from image_atlas.indexer import scan
from image_atlas.web import app


def seed_groups(tmp_path, count=3):
    path = tmp_path / "faces.png"
    Image.new("RGB", (80, 80), "green").save(path)
    with connect() as conn:
        scan(conn, str(tmp_path))
        image_id = conn.execute("SELECT id FROM images WHERE path=%s", (normalized_path(path),)).fetchone()["id"]
        groups = []
        for index in range(count):
            group = conn.execute("INSERT INTO face_groups DEFAULT VALUES RETURNING id").fetchone()["id"]
            conn.execute("""INSERT INTO faces(image_id,x,y,width,height,embedding,group_id)
                            VALUES (%s,%s,0,10,10,%s::halfvec,%s)""",
                         (image_id, index * 10, vector_literal([1.0] + [0.0] * 127), group))
            groups.append(group)
        conn.commit()
    return image_id, groups


def test_existing_person_selection_and_validation(tmp_path):
    _, groups = seed_groups(tmp_path)
    client = TestClient(app, base_url="http://127.0.0.1", headers={"X-Image-CLI-Session": app.state.session_token})
    first = client.post(f"/api/groups/{groups[0]}/tag", json={"name": "Alice"}).json()
    person_id = first["person_id"]
    assert client.get("/api/people").json() == [{"id": person_id, "name": "Alice"}]
    second = client.post(f"/api/groups/{groups[1]}/tag", json={"person_id": person_id}).json()
    assert second["person_id"] == person_id
    runner = JsonCliRunner()
    assert json.loads(runner.invoke(cli, ["faces", "people"]).output)[0]["id"] == person_id
    result = runner.invoke(cli, ["faces", "tag", str(groups[2]), "--person-id", str(person_id)])
    assert result.exit_code == 0, result.output
    assert json.loads(result.output)["person_id"] == person_id
    for payload in ({}, {"name": " "}, {"person_id": 999999}, {"name": "Bob", "person_id": person_id}):
        assert client.post(f"/api/groups/{groups[1]}/tag", json=payload).status_code == 400
    assert client.post("/api/groups/999999/tag", json={"name": "Bob"}).status_code == 400
    with connect() as conn:
        assert conn.execute("SELECT count(*) AS n FROM people").fetchone()["n"] == 1
        assert conn.execute("SELECT count(*) AS n FROM faces WHERE person_id=%s", (person_id,)).fetchone()["n"] == 3


def test_ignored_named_person_hides_assignments_and_suggestions(tmp_path):
    _, groups = seed_groups(tmp_path)
    client = TestClient(app, base_url="http://127.0.0.1", headers={"X-Image-CLI-Session": app.state.session_token})
    person_id = client.post(f"/api/groups/{groups[0]}/tag", json={"name": "Alice"}).json()["person_id"]
    client.post(f"/api/groups/{groups[1]}/tag", json={"person_id": person_id})
    with connect() as conn:
        conn.execute("UPDATE faces SET suggested_person_id=%s WHERE group_id=%s", (person_id, groups[2]))
        conn.commit()
    assert client.post(f"/api/groups/{groups[0]}/ignore").json()["person_id"] == person_id
    assert client.get("/api/groups").json() == []
    assert client.get("/api/people").json() == []
    assert json.loads(JsonCliRunner().invoke(cli, ["faces", "groups"]).output) == []
    assert client.post(f"/api/groups/{groups[1]}/tag", json={"person_id": person_id}).status_code == 400
    same_name = client.post(f"/api/groups/{groups[1]}/tag", json={"name": "Alice"})
    assert same_name.status_code == 200
    assert same_name.json()["person_id"] != person_id
    with connect() as conn:
        assert conn.execute("SELECT count(*) AS n FROM faces WHERE person_id=%s", (person_id,)).fetchone()["n"] == 2
        assert conn.execute("SELECT count(*) AS n FROM faces WHERE person_id=%s", (same_name.json()["person_id"],)).fetchone()["n"] == 1


def test_ignore_sample_set_recognizes_future_faces(tmp_path, monkeypatch):
    image_id, groups = seed_groups(tmp_path, count=1)
    client = TestClient(app, base_url="http://127.0.0.1", headers={"X-Image-CLI-Session": app.state.session_token})
    ignored_id = client.post(f"/api/groups/{groups[0]}/ignore").json()["person_id"]
    with connect() as conn:
        # A confident match inherits the ignored identity; an unrelated face does not.
        _, suggestion = _group_for(conn, [1.0] + [0.0] * 127)
        assert suggestion == ignored_id
        _, suggestion = _group_for(conn, [0.0, 1.0] + [0.0] * 126)
        assert suggestion is None
        conn.rollback()

    class Engine:
        def detect(self, path):
            yield {"box": (40, 0, 10, 10), "embedding": [1.0] + [0.0] * 127, "confidence": 0.99}
            yield {"box": (60, 0, 10, 10), "embedding": [0.0, 1.0] + [0.0] * 126, "confidence": 0.99}

    monkeypatch.setattr("image_atlas.faces.FaceEngine", Engine)
    with connect() as conn:
        result = process_pending(conn, image_ids=[image_id])
        assert result["faces"] == 2 and result["errors"] == []
        assert conn.execute("SELECT person_id FROM faces WHERE x=40").fetchone()["person_id"] == ignored_id
        assert conn.execute("SELECT person_id FROM faces WHERE x=60").fetchone()["person_id"] is None
    assert len(client.get("/api/groups").json()) == 1


def test_suggestion_survives_additional_samples(tmp_path):
    _, groups = seed_groups(tmp_path, count=1)
    with connect() as conn:
        person_id = conn.execute("INSERT INTO people(name) VALUES ('Alice') RETURNING id").fetchone()["id"]
        conn.execute("UPDATE faces SET suggested_person_id=%s", (person_id,))
        group_id, suggestion = _group_for(conn, [1.0] + [0.0] * 127)
        assert group_id == groups[0] and suggestion == person_id
