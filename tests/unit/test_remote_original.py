"""Repeated native-viewer opens preserve existing downloads and use the default app."""
import hashlib
import io
from pathlib import Path
from unittest.mock import Mock

from image_atlas import remote_client


def test_original_open_downloads_once_and_reopens_verified_file(tmp_path, monkeypatch):
    content = b'original image bytes'
    row = {'path': r'G:\Photos\photo.jpg', 'sha256': hashlib.sha256(content).hexdigest()}
    monkeypatch.setattr(Path, 'home', classmethod(lambda cls: tmp_path))
    monkeypatch.setattr(remote_client, 'get_json', lambda route: row)
    requests = []

    def request(route):
        requests.append(route)
        response = io.BytesIO(content)
        response.headers = {'Content-Length': str(len(content))}
        return response

    monkeypatch.setattr(remote_client, 'request', request)
    open_file = Mock()
    monkeypatch.setattr(remote_client, '_open_file', open_file)
    first = remote_client.open_original(1)
    assert Path(first).read_bytes() == content
    assert remote_client.open_original(1) == first
    assert requests == ['/v1/images/1/original']
    assert open_file.call_count == 2
    open_file.assert_called_with(Path(first))


def test_original_open_preserves_unrelated_existing_file(tmp_path, monkeypatch):
    downloads = tmp_path / 'Downloads'
    downloads.mkdir()
    existing = downloads / 'photo.jpg'
    existing.write_bytes(b'user file')
    monkeypatch.setattr(Path, 'home', classmethod(lambda cls: tmp_path))
    monkeypatch.setattr(remote_client, 'get_json', lambda route: {
        'path': r'G:\Photos\photo.jpg', 'sha256': hashlib.sha256(b'new image').hexdigest()})
    download = Mock(return_value='new copy')
    monkeypatch.setattr(remote_client, 'download', download)
    assert remote_client.open_original(1) == 'new copy'
    assert existing.read_bytes() == b'user file'
    target = download.call_args.args[1]
    assert target.parent == downloads
    assert target.name != existing.name
    assert target.suffix == '.jpg'
    assert download.call_args.kwargs == {'open_file': True}
