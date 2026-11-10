import json

from PIL import Image
from cli_helpers import JsonCliRunner

from image_atlas.cli import app
from image_atlas.db import connect, initialize, normalized_path
from image_atlas.indexer import scan


def test_mine_all_scopes_analysis_to_target_and_skips_finished(monkeypatch, tmp_path):
    from image_atlas import embeddings, faces, labels

    class FakeEmbedder:
        def image(self, path):
            return [1.0] + [0.0] * 511

        def text(self, query):
            return [1.0] + [0.0] * 511

    class FakeFaces:
        def detect(self, path):
            return []

    monkeypatch.setattr(embeddings, "create_embedder", lambda **kwargs: FakeEmbedder())
    monkeypatch.setattr(labels, "create_embedder", lambda **kwargs: FakeEmbedder())
    monkeypatch.setattr(faces, "FaceEngine", FakeFaces)
    first = tmp_path / "first"
    other = tmp_path / "other"
    first.mkdir()
    other.mkdir()
    Image.new("RGB", (32, 32), "green").save(first / "first.png")
    Image.new("RGB", (32, 32), "blue").save(other / "other.png")
    initialize()
    with connect() as conn:
        scan(conn, str(other))

    runner = JsonCliRunner()
    result = runner.invoke(app, ["mine", str(first)])
    assert result.exit_code == 0, result.output
    stages = json.loads(result.stdout)["stages"]
    assert json.loads(result.stdout)["metrics"]["pictures_total"] == 1
    assert list(stages) == ["metadata", "embeddings", "labels", "faces"]
    assert stages["metadata"]["new"] == 1
    assert stages["embeddings"]["processed"] == 1
    assert stages["labels"]["processed"] == 1
    assert stages["faces"]["images"] == 1
    with connect() as conn:
        target = conn.execute("SELECT embedding_status,face_status FROM images WHERE root_path=%s",
                              (normalized_path(first),)).fetchone()
        untouched = conn.execute("SELECT embedding_status,face_status FROM images WHERE root_path=%s",
                                 (normalized_path(other),)).fetchone()
    assert target == {"embedding_status": "done", "face_status": "done"}
    assert untouched == {"embedding_status": "pending", "face_status": "pending"}
    repeat = runner.invoke(app, ["mine", str(first)])
    assert repeat.exit_code == 0, repeat.output
    assert json.loads(repeat.stdout)["stages"]["metadata"]["unchanged"] == 1
    assert json.loads(repeat.stdout)["stages"]["embeddings"]["processed"] == 0


def test_mine_tags_select_stages_and_reject_unknown(tmp_path):
    image = tmp_path / "sample.png"
    Image.new("RGB", (24, 24), "red").save(image)
    runner = JsonCliRunner()
    invalid = runner.invoke(app, ["mine", str(image), "--tags", "unknown"])
    assert invalid.exit_code != 0
    assert "Use all, metadata, embeddings, labels, or faces" in invalid.output
    assert runner.invoke(app, ["mine", str(image), "--tags", "ocr"]).exit_code != 0
    assert runner.invoke(app, ["ocr", "--help"]).exit_code != 0
    assert runner.invoke(app, ["ocr-benchmark", "--help"]).exit_code != 0
    result = runner.invoke(app, ["mine", str(image), "--tags", "metadata"])
    assert result.exit_code == 0, result.output
    assert list(json.loads(result.stdout)["stages"]) == ["metadata"]
    with connect() as conn:
        row = conn.execute("SELECT embedding_status,face_status FROM images WHERE path=%s",
                           (normalized_path(image),)).fetchone()
    assert row == {"embedding_status": "pending", "face_status": "pending"}


def test_retained_ocr_data_is_not_exposed_or_searched(tmp_path):
    image = tmp_path / "portrait.png"
    Image.new("RGB", (24, 24), "red").save(image)
    runner = JsonCliRunner()
    assert runner.invoke(app, ["mine", str(image), "--tags", "metadata"]).exit_code == 0
    with connect() as conn:
        row = conn.execute("UPDATE images SET ocr_text='secretword',ocr_status='done' "
                           "WHERE path=%s RETURNING id", (normalized_path(image),)).fetchone()
        conn.commit()
    assert json.loads(runner.invoke(app, ["search", "secretword"]).stdout) == []
    assert "ocr_text" not in json.loads(runner.invoke(app, ["show", str(row["id"])]).stdout)


def test_mine_reports_model_startup_failure_without_poisoning_backlog(monkeypatch, tmp_path):
    from image_atlas import embeddings, faces

    class FakeFaces:
        def detect(self, path):
            return []

    def unavailable(**kwargs):
        raise RuntimeError("embedding model unavailable")

    monkeypatch.setattr(embeddings, "create_embedder", unavailable)
    monkeypatch.setattr(faces, "FaceEngine", FakeFaces)
    image = tmp_path / "sample.png"
    Image.new("RGB", (24, 24), "red").save(image)
    result = JsonCliRunner().invoke(app, ["mine", str(tmp_path), "--tags", "embeddings,faces"])
    assert result.exit_code == 1, result.output
    stages = json.loads(result.stdout)["stages"]
    assert stages["embeddings"]["error"] == "embedding model unavailable"
    assert stages["faces"]["images"] == 1
    with connect() as conn:
        row = conn.execute("SELECT embedding_status,face_status FROM images WHERE path=%s",
                           (normalized_path(image),)).fetchone()
    assert row == {"embedding_status": "pending", "face_status": "done"}


def test_mine_counts_failed_face_image_in_stage_average(monkeypatch, tmp_path):
    from image_atlas import faces

    class BrokenFaces:
        def detect(self, path):
            raise ValueError("bad face image")

    monkeypatch.setattr(faces, "FaceEngine", BrokenFaces)
    image = tmp_path / "sample.png"
    Image.new("RGB", (24, 24), "red").save(image)
    result = JsonCliRunner().invoke(app, ["mine", str(image), "--tags", "faces"])
    assert result.exit_code == 1
    stage = json.loads(result.stdout)["stages"]["faces"]
    assert len(stage["errors"]) == 1
    assert "images_per_second" in stage


def test_mine_limit_caps_whole_pipeline_and_next_run_advances(monkeypatch, tmp_path):
    from image_atlas import embeddings, faces, labels

    class FakeEmbedder:
        def image(self, path):
            return [1.0] + [0.0] * 511

        def text(self, query):
            return [1.0] + [0.0] * 511

    class FakeFaces:
        def detect(self, path):
            return []

    monkeypatch.setattr(embeddings, "create_embedder", lambda **kwargs: FakeEmbedder())
    monkeypatch.setattr(labels, "create_embedder", lambda **kwargs: FakeEmbedder())
    monkeypatch.setattr(faces, "FaceEngine", FakeFaces)
    for number in range(4):
        Image.new("RGB", (24, 24), (number * 50, 0, 0)).save(tmp_path / f"{number}.png")
    runner = JsonCliRunner()
    first = runner.invoke(app, ["mine", str(tmp_path), "--limit", "2"])
    assert first.exit_code == 0, first.output
    first_result = json.loads(first.stdout)
    assert first_result["metrics"]["pictures_total"] == 2
    assert first_result["stages"]["metadata"]["new"] == 2
    assert [first_result["stages"][tag][key] for tag, key in
            (("embeddings", "processed"), ("faces", "images"))] == [2, 2]
    with connect() as conn:
        first_ids = {row["id"] for row in conn.execute("SELECT id FROM images").fetchall()}
    assert len(first_ids) == 2

    second = runner.invoke(app, ["mine", str(tmp_path), "--limit", "2"])
    assert second.exit_code == 0, second.output
    second_result = json.loads(second.stdout)
    assert second_result["stages"]["metadata"]["new"] == 2
    with connect() as conn:
        rows = conn.execute("SELECT id,embedding_status,face_status FROM images").fetchall()
    assert len(rows) == 4
    assert all((row["embedding_status"], row["face_status"]) == ("done", "done") for row in rows)
    assert len({row["id"] for row in rows} - first_ids) == 2
    third = runner.invoke(app, ["mine", str(tmp_path), "--limit", "2"])
    assert third.exit_code == 0, third.output
    assert json.loads(third.stdout)["metrics"]["pictures_total"] == 0
    assert runner.invoke(app, ["mine", str(tmp_path), "--limit", "2", "--prune"]).exit_code != 0


def test_everything_source_preserves_folder_scope_and_limit(monkeypatch, tmp_path):
    from image_atlas import everything
    folder = tmp_path / "photos"
    folder.mkdir()
    images = [folder / f"{number}.png" for number in range(3)]
    for image in images:
        Image.new("RGB", (24, 24), "blue").save(image)
    monkeypatch.setattr(everything, "iter_images", lambda **kwargs: iter(images))
    runner = JsonCliRunner()
    first = runner.invoke(app, ["mine", "searcheverything", "--tags", "metadata", "--limit", "2",
                                "--include-system"])
    assert first.exit_code == 0, first.output
    assert json.loads(first.stdout)["stages"]["metadata"]["new"] == 2
    second = runner.invoke(app, ["mine", "searcheverything", "--tags", "metadata", "--limit", "2",
                                 "--include-system"])
    assert second.exit_code == 0, second.output
    assert json.loads(second.stdout)["stages"]["metadata"]["new"] == 1
    with connect() as conn:
        roots = {row["root_path"] for row in conn.execute("SELECT root_path FROM images").fetchall()}
    assert roots == {normalized_path(folder)}
    assert runner.invoke(app, ["mine", "searcheverything", "--prune"]).exit_code != 0


def test_everything_custom_exclusion_applies_before_limit(monkeypatch, tmp_path):
    from image_atlas import everything
    blocked = tmp_path / "blocked"
    allowed = tmp_path / "allowed"
    blocked.mkdir()
    allowed.mkdir()
    images = [blocked / "first.png", allowed / "second.png"]
    for image in images:
        Image.new("RGB", (24, 24), "green").save(image)
    monkeypatch.setattr(everything, "_query_page", lambda search, offset: (images, len(images)))
    result = JsonCliRunner().invoke(app, ["mine", "searcheverything", "--tags", "metadata",
                                      "--limit", "1", "--include-system", "--exclude-dir", str(blocked)])
    assert result.exit_code == 0, result.output
    assert json.loads(result.stdout)["stages"]["metadata"]["new"] == 1
    with connect() as conn:
        paths = {row["path"] for row in conn.execute("SELECT path FROM images").fetchall()}
    assert paths == {normalized_path(images[1])}


def test_everything_unlimited_scopes_enrichment_to_discovered_images(monkeypatch, tmp_path):
    from image_atlas import embeddings, everything, faces, labels

    class FakeEmbedder:
        def image(self, path):
            return [1.0] + [0.0] * 511

        def text(self, query):
            return [1.0] + [0.0] * 511

    class FakeFaces:
        def detect(self, path):
            return []

    monkeypatch.setattr(embeddings, "create_embedder", lambda **kwargs: FakeEmbedder())
    monkeypatch.setattr(labels, "create_embedder", lambda **kwargs: FakeEmbedder())
    monkeypatch.setattr(faces, "FaceEngine", FakeFaces)
    first = tmp_path / "first.png"
    other = tmp_path / "other.png"
    Image.new("RGB", (24, 24), "red").save(first)
    Image.new("RGB", (24, 24), "blue").save(other)
    initialize()
    with connect() as conn:
        scan(conn, str(other))
    monkeypatch.setattr(everything, "iter_images", lambda **kwargs: iter([first]))
    result = JsonCliRunner().invoke(app, ["mine", "searcheverything", "--include-system"])
    assert result.exit_code == 0, result.output
    stages = json.loads(result.stdout)["stages"]
    assert stages["embeddings"]["processed"] == 1
    assert stages["faces"]["images"] == 1
    with connect() as conn:
        untouched = conn.execute("SELECT embedding_status,face_status FROM images WHERE path=%s",
                                 (normalized_path(other),)).fetchone()
    assert untouched == {"embedding_status": "pending", "face_status": "pending"}


def test_mine_can_cancel_and_preserve_completed_images(monkeypatch, tmp_path):
    from image_atlas import indexer
    from image_atlas.jobs import job_directory
    for number in range(3):
        Image.new("RGB", (24, 24), "red").save(tmp_path / f"{number}.png")
    original = indexer.metadata
    calls = 0

    def cancel_after_first(path, **kwargs):
        nonlocal calls
        value = original(path, **kwargs)
        calls += 1
        if calls == 1:
            from image_atlas.jobs import active_jobs
            (job_directory() / f"{active_jobs()[0]['id']}.cancel").touch()
        return value

    monkeypatch.setattr(indexer, "metadata", cancel_after_first)
    result = JsonCliRunner().invoke(app, ["mine", str(tmp_path), "--tags", "metadata"])
    assert result.exit_code == 130, result.output
    assert json.loads(result.stdout)["cancelled"] is True
    assert json.loads(result.stdout)['stages']['metadata']['new'] == 1
    assert json.loads(result.stdout)['metrics']['images_processed'] == 1
    with connect() as conn:
        assert conn.execute("SELECT count(*) AS n FROM images").fetchone()["n"] == 1


def test_jobs_and_cancel_command(monkeypatch, tmp_path):
    from image_atlas.jobs import MineJob
    monkeypatch.setenv("IMAGE_CLI_JOB_DIR", str(tmp_path / "jobs"))
    job = MineJob("sample")
    try:
        runner = JsonCliRunner()
        listed = json.loads(runner.invoke(app, ["jobs"]).stdout)
        assert [item["id"] for item in listed] == [job.id]
        requested = runner.invoke(app, ["cancel"])
        assert requested.exit_code == 0, requested.output
        assert json.loads(requested.stdout)["cancel_requested"] == job.id
        assert json.loads(runner.invoke(app, ["cancel", job.id]).stdout)["cancel_requested"] == job.id
        assert json.loads(runner.invoke(app, ["jobs"]).stdout)[0]["cancel_requested"] is True
    finally:
        job.close()
