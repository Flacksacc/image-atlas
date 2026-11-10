import io
import json
from unittest.mock import Mock
from urllib.parse import parse_qs, urlsplit

import pytest
from PIL import Image

from image_atlas import catalogs, embeddings, image_query, remote_client


def test_reference_is_oriented_and_readable_outside_catalog_roots(tmp_path):
    path = tmp_path / 'reference.png'
    exif = Image.Exif(); exif[274] = 6
    Image.new('RGB', (30, 20), 'red').save(path, exif=exif)
    original = path.read_bytes()
    token = catalogs.current.set(catalogs.Catalog('catalog', 'Test', '', (str(tmp_path / 'owned'),)))
    try:
        with image_query.open_reference(path) as image:
            assert image.size == (20, 30)
        assert path.read_bytes() == original
    finally:
        catalogs.current.reset(token)


def test_invalid_reference_fails_before_loading_a_model(tmp_path, monkeypatch):
    path = tmp_path / 'invalid.png'; path.write_text('not an image')
    factory = Mock(side_effect=AssertionError('Invalid files must not load a model'))
    monkeypatch.setattr(embeddings, 'create_embedder', factory)
    with pytest.raises(ValueError, match='Unsupported'):
        image_query.encode_reference(path)
    factory.assert_not_called()


def test_reference_size_limit_and_encoder_cleanup(tmp_path, monkeypatch):
    path = tmp_path / 'reference.png'; Image.new('RGB', (20, 20)).save(path)
    encoder = Mock(); encoder.reference_image.return_value = [1.0] + [0.0] * 511
    monkeypatch.setattr(embeddings, 'create_embedder', lambda **kwargs: encoder)
    assert image_query.encode_reference(path, cpu=True) == encoder.reference_image.return_value
    encoder.close.assert_called_once()
    monkeypatch.setattr(image_query, 'MAX_BYTES', 1)
    with pytest.raises(ValueError, match='32 MiB'):
        image_query.encode_reference(path)


@pytest.mark.parametrize('vector', [[0.0]*512, [1.0], [float('nan')]*512, [float('inf')]*512,
                                    [2.0]*512, [True]*512, ['0.1']*512, [1e-30]*512])
def test_invalid_query_vectors_are_rejected(vector):
    with pytest.raises(ValueError):
        image_query.validate_vector(vector)


def test_remote_search_sends_only_a_compact_vector_and_requires_server_support(monkeypatch):
    urls = []
    def get_json(url):
        urls.append(url)
        if url == '/v1/session': return {'image_search_model': embeddings.MODEL_ID}
        return {'items': []}
    monkeypatch.setattr(remote_client, 'get_json', get_json)
    monkeypatch.setattr(remote_client, 'request', lambda *args, **kwargs: io.BytesIO())
    vector = [1.0] + [0.0]*511
    remote_client.search(image_vector=vector)
    params = parse_qs(urlsplit(urls[-1]).query)
    assert json.loads(params['image_vector'][0]) == vector
    assert len(urls[-1]) < 12000
    assert 'image_path' not in params
    monkeypatch.setattr(remote_client, 'get_json', lambda url: {})
    with pytest.raises(ValueError, match='Update the remote'):
        remote_client.search(image_vector=vector)
