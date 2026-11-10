from pathlib import Path

from image_atlas import worker_env


def test_worker_selection_prefers_override_then_local_packages_then_shared(monkeypatch, tmp_path):
    shared = tmp_path / "python.exe"
    shared.touch()
    monkeypatch.setattr(worker_env, "SHARED_PYTHON", shared)
    monkeypatch.setattr(worker_env.sys, "platform", "win32")
    monkeypatch.setattr(worker_env.importlib.util, "find_spec", lambda name: None)
    monkeypatch.delenv("IMAGE_CLI_VISION_PYTHON", raising=False)
    assert worker_env.model_python("IMAGE_CLI_VISION_PYTHON", ("torch", "open_clip")) == str(shared)
    monkeypatch.setenv("IMAGE_CLI_VISION_PYTHON", str(Path("X:/other/python.exe")))
    assert worker_env.model_python("IMAGE_CLI_VISION_PYTHON", ("torch",)) == str(Path("X:/other/python.exe"))
    monkeypatch.delenv("IMAGE_CLI_VISION_PYTHON")
    monkeypatch.setattr(worker_env.importlib.util, "find_spec", lambda name: object())
    assert worker_env.model_python("IMAGE_CLI_VISION_PYTHON", ("torch", "open_clip")) is None
