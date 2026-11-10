"""The chosen identity ships with the app and remains behind gallery authentication."""
import io
import subprocess
from unittest.mock import Mock

from fastapi.testclient import TestClient
from PIL import Image

from image_atlas import launcher
from image_atlas.web import app


def test_gallery_brand_assets_and_about_keep_session_security():
    client = TestClient(app, base_url='http://127.0.0.1')
    assert client.get('/logo.png').status_code == 401
    client.headers['X-Image-Atlas-Session'] = app.state.session_token
    home = client.get('/')
    assert home.status_code == 200
    assert 'About ImageAtlas' in home.text
    assert '/logo.png' in home.text and '/icon.ico' in home.text
    assert '__IMAGEATLAS_VERSION__' not in home.text
    logo = client.get('/logo.png')
    assert logo.headers['content-type'] == 'image/png'
    with Image.open(io.BytesIO(logo.content)) as image:
        assert image.size == (1254, 1254)
        assert image.getchannel('A').getextrema() == (0, 255)
    icon = client.get('/icon.ico')
    assert icon.status_code == 200
    with Image.open(io.BytesIO(icon.content)) as image:
        assert {(16, 16), (32, 32), (48, 48), (256, 256)} <= image.ico.sizes()


def test_start_menu_launcher_invokes_normal_cli_without_a_console(monkeypatch):
    run = Mock(return_value=subprocess.CompletedProcess([], 0, '', ''))
    monkeypatch.setattr(launcher.subprocess, 'run', run)
    assert launcher.main() == 0
    assert run.call_args.args[0][-3:] == ['-m', 'image_atlas.cli', 'view']
    assert run.call_args.kwargs['creationflags'] == getattr(subprocess, 'CREATE_NO_WINDOW', 0)


def test_start_menu_launcher_reports_failure_without_a_traceback(monkeypatch, capsys):
    monkeypatch.setattr(launcher.sys, 'platform', 'linux')
    monkeypatch.setattr(launcher.subprocess, 'run', Mock(
        return_value=subprocess.CompletedProcess([], 1, '', 'Database unavailable')))
    assert launcher.main() == 1
    error = capsys.readouterr().err
    assert 'Database unavailable' in error
    assert 'Traceback' not in error
