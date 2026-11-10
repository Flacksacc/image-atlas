"""The CLI launches the native viewer with the same database and model environment."""
import json
import subprocess
import sys

from cli_helpers import JsonCliRunner

from image_atlas.cli import app


def test_view_launches_native_binary(tmp_path, monkeypatch):
    binary = tmp_path / "image-atlas-viewer.exe"
    binary.write_bytes(b"test")
    monkeypatch.setenv("IMAGE_CLI_VIEWER_BIN", str(binary))
    calls = []

    def fake_popen(args, **kwargs):
        calls.append((args, kwargs))

    monkeypatch.setattr(subprocess, "Popen", fake_popen)
    result = JsonCliRunner().invoke(app, ["view"])
    assert result.exit_code == 0, result.output
    assert json.loads(result.output) == {"started": True, "viewer": str(binary)}
    assert calls[0][0] == [str(binary)]
    assert calls[0][1]["env"]["IMAGE_CLI_PYTHON"] == sys.executable
    assert calls[0][1]["env"]["CLI_VIEWER_START_MODE"] == "images"
    assert calls[0][1]["env"]["IMAGE_CLI_DSN"].endswith("/imagecli_test")
    context = json.loads(calls[0][1]["env"]["IMAGE_CLI_VIEWER_CONTEXT"])
    assert "dsn" not in context  # No second credential copy reaches helpers.
    assert context["name"]
    assert context["vision_python"] == calls[0][1]["env"]["IMAGE_CLI_VISION_PYTHON"]


def test_viewer_catalog_context_supplies_worker_for_cross_mode_switching():
    result = JsonCliRunner().invoke(app, ["db", "context"])
    assert result.exit_code == 0, result.output
    context = json.loads(result.output)
    assert context["vision_python"]
    assert context["dsn"]


def test_view_uses_newer_build_when_running_binary_is_locked(tmp_path, monkeypatch):
    import os
    import image_atlas.cli as cli

    monkeypatch.delenv("IMAGE_CLI_VIEWER_BIN", raising=False)
    monkeypatch.setattr(cli, "__file__", str(tmp_path / "src" / "image_atlas" / "cli.py"))
    release = tmp_path / "viewer" / "target" / "release"
    release.mkdir(parents=True)
    original = release / "image-atlas-viewer.exe"
    updated = release / "image-atlas-viewer-updated.exe"
    original.write_bytes(b"old")
    updated.write_bytes(b"updated")
    os.utime(original, (100, 100))
    os.utime(updated, (200, 200))
    calls = []
    monkeypatch.setattr(subprocess, "Popen", lambda args, **kwargs: calls.append(args))
    result = JsonCliRunner().invoke(app, ["view"])
    assert result.exit_code == 0, result.output
    assert calls == [[str(updated)]]
    os.utime(original, (300, 300))
    result = JsonCliRunner().invoke(app, ["view"])
    assert result.exit_code == 0, result.output
    assert calls[-1] == [str(original)]
