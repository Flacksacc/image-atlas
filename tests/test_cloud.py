"""Cloud placeholders must not cause implicit downloads during mining."""
import json

import pytest
from cli_helpers import JsonCliRunner
from PIL import Image

from image_atlas import cloud
from image_atlas.cli import app
from image_atlas.db import connect, normalized_path


def test_cloud_availability_uses_file_state_on_google_drive(monkeypatch):
    from pathlib import Path
    image = Path('G:/Shared drives/Marketing/Marketing/image.psd')
    for flag in (cloud.OFFLINE, cloud.RECALL_ON_OPEN, cloud.RECALL_ON_DATA_ACCESS):
        monkeypatch.setattr(cloud, "file_attributes", lambda path, value=flag: value)
        assert cloud.unavailable_cloud_file(image)
    for attributes in (0x20, 0x80, 0x80000, 0x100000, None):
        # PINNED/UNPINNED describe policy, not whether content is downloaded.
        monkeypatch.setattr(cloud, "file_attributes", lambda path, value=attributes: value)
        assert not cloud.unavailable_cloud_file(image)
    for policy in (0x80000, 0x100000):
        monkeypatch.setattr(cloud, "file_attributes", lambda path, value=policy: value | cloud.RECALL_ON_DATA_ACCESS)
        assert cloud.unavailable_cloud_file(image)


def test_cloud_availability_rechecks_hydration_state(monkeypatch, tmp_path):
    image = tmp_path / 'changing.jpg'
    states = iter([cloud.OFFLINE, 0x80, cloud.RECALL_ON_DATA_ACCESS])
    monkeypatch.setattr(cloud, 'file_attributes', lambda path: next(states))
    assert cloud.unavailable_cloud_file(image)
    assert not cloud.unavailable_cloud_file(image)
    assert cloud.unavailable_cloud_file(image)


@pytest.mark.parametrize("limit", [None, 1])
def test_mine_skips_cloud_only_images_before_open(monkeypatch, tmp_path, limit):
    from image_atlas import indexer
    remote = tmp_path / "0-remote.png"
    local = tmp_path / "1-local.png"
    for image in (remote, local):
        Image.new("RGB", (24, 24), "blue").save(image)
    unavailable = lambda path: path.name == remote.name
    monkeypatch.setattr(cloud, "unavailable_cloud_file", unavailable)
    monkeypatch.setattr(indexer, "unavailable_cloud_file", unavailable)
    original = indexer.digest

    def guarded_digest(path):
        assert path != remote, "remote image data was opened"
        return original(path)

    monkeypatch.setattr(indexer, "digest", guarded_digest)
    args = ["mine", str(tmp_path), "--tags", "metadata"]
    if limit:
        args.extend(["--limit", str(limit)])
    response = JsonCliRunner().invoke(app, args)
    assert response.exit_code == 0, response.output
    result = json.loads(response.stdout)
    assert result["metrics"]["pictures_total"] == 1
    assert result["skipped_cloud"] == 1
    assert result["stages"]["metadata"]["new"] == 1
    with connect() as conn:
        paths = {row["path"] for row in conn.execute("SELECT path FROM images").fetchall()}
    assert paths == {normalized_path(local)}


def test_cloud_file_that_dehydrates_after_scan_keeps_pending_stages(monkeypatch, tmp_path):
    from image_atlas import embeddings, faces
    image = tmp_path / "local.png"
    Image.new("RGB", (24, 24), "red").save(image)
    monkeypatch.setattr(embeddings, "unavailable_cloud_file", lambda path: True)
    monkeypatch.setattr(faces, "unavailable_cloud_file", lambda path: True)
    monkeypatch.setattr(embeddings, "create_embedder", lambda **kwargs: pytest.fail("opened remote image"))
    monkeypatch.setattr(faces, "FaceEngine", lambda: pytest.fail("opened remote image"))
    response = JsonCliRunner().invoke(app, ["mine", str(image)])
    assert response.exit_code == 0, response.output
    result = json.loads(response.stdout)
    assert result["stages"]["embeddings"]["skipped_cloud"] == 1
    assert result["stages"]["faces"]["skipped_cloud"] == 1
    with connect() as conn:
        row = conn.execute("SELECT embedding_status,face_status FROM images").fetchone()
    assert row == {"embedding_status": "pending", "face_status": "pending"}
