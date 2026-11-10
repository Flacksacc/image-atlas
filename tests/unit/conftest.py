"""Unit tests isolate local settings without requiring a PostgreSQL server."""
import pytest

from image_atlas.catalogs import current


@pytest.fixture(autouse=True)
def isolated_database(monkeypatch, tmp_path):
    token = current.set(None)
    monkeypatch.setenv('IMAGE_CLI_CONFIG_DIR', str(tmp_path / 'config'))
    monkeypatch.setenv('IMAGE_CLI_JOB_DIR', str(tmp_path / 'jobs'))
    monkeypatch.delenv('IMAGE_CLI_DSN', raising=False)
    try:
        yield
    finally:
        current.reset(token)
