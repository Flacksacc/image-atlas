import json
from pathlib import Path
from unittest.mock import Mock

from fastapi.testclient import TestClient
from PIL import Image

from cli_helpers import JsonCliRunner
from image_atlas import catalogs, embeddings, image_query
from image_atlas.cli import app as cli
from image_atlas.db import connect, vector_literal
from image_atlas.indexer import scan
from image_atlas.remote_server import create_app
from image_atlas.web import app


def test_reference_search_cli_gallery_and_remote_rank_compatible_images_without_importing_sample(tmp_path, monkeypatch):
    owned = tmp_path / 'catalog'; owned.mkdir()
    record = catalogs.register('Image search', catalogs.resolve().dsn, [str(owned)])
    catalog = catalogs.resolve(record['id'])
    for name in ['near', 'far', 'other-model', 'missing']:
        Image.new('RGB', (20, 20), 'red').save(owned / (name + '.png'))
    vector = [1.0] + [0.0]*511
    with connect() as conn:
        scan(conn, str(owned))
        rows = conn.execute('SELECT id,path FROM images').fetchall()
        ids = {Path(row['path']).stem: row['id'] for row in rows}
        for name, iid in ids.items():
            value = [.8, .6] + [0.0]*510 if name == 'far' else vector
            model = 'incompatible' if name == 'other-model' else embeddings.MODEL_ID
            conn.execute('UPDATE images SET embedding=%s::halfvec,embedding_model=%s,missing=%s WHERE id=%s',
                         (vector_literal(value), model, name == 'missing', iid))
        conn.commit()
    reference = tmp_path / 'outside-the-catalog.png'; Image.new('RGB', (24, 18), 'blue').save(reference)
    original = reference.read_bytes()
    encoder = Mock(); encoder.reference_image.return_value = vector
    monkeypatch.setattr(embeddings, 'create_embedder', lambda **kwargs: encoder)
    result = JsonCliRunner().invoke(cli, ['--database', record['id'], 'image-search', str(reference), '--cpu'])
    assert result.exit_code == 0, result.output
    assert [row['id'] for row in json.loads(result.output)] == [ids['near'], ids['far']]

    client = TestClient(app, base_url='http://127.0.0.1', headers={'X-Image-Atlas-Session': app.state.session_token})
    prefix = '/api/catalogs/' + record['id']
    upload = client.post(prefix + '/search/image', content=original, headers={'Content-Type': 'image/png'})
    assert upload.status_code == 200, upload.text
    assert [row['id'] for row in upload.json()['items']] == [ids['near'], ids['far']]
    # The temporary upload is deleted and no source image is added to the catalog.
    assert not encoder.reference_image.call_args.args[0].exists()
    catalogs.edit_roots(record['id'], [])
    page = client.post(prefix + '/search/image/page', json={'vector': vector, 'offset': 1})
    assert page.status_code == 200, page.text
    assert [row['id'] for row in page.json()] == [ids['far']]
    missing = client.post(prefix + '/search/image/page', json={'vector': vector, 'include_missing': True})
    assert missing.status_code == 200, missing.text
    assert {row['id'] for row in missing.json()} == {ids['near'], ids['far'], ids['missing']}
    server = create_app(catalog, encoder_factory=Mock(side_effect=AssertionError('A supplied vector needs no server encoder')))
    remote = TestClient(server, base_url='http://127.0.0.1', headers={'Authorization': 'Bearer ' + server.state.token})
    response = remote.get('/v1/images', params={'image_vector': json.dumps(vector)})
    assert response.status_code == 200, response.text
    assert [row['id'] for row in response.json()['items']] == [ids['near'], ids['far']]
    assert remote.get('/v1/images', params={'image_vector': '[0]'}).status_code == 400
    assert reference.read_bytes() == original
    with connect() as conn:
        assert conn.execute('SELECT count(*) n FROM images').fetchone()['n'] == 4


def test_gallery_reference_upload_limits_and_authentication(tmp_path, monkeypatch):
    client = TestClient(app, base_url='http://127.0.0.1')
    assert client.post('/api/search/image', content=b'not an image').status_code == 401
    client.headers['X-Image-Atlas-Session'] = app.state.session_token
    monkeypatch.setattr(image_query, 'MAX_BYTES', 8)
    assert client.post('/api/search/image', content=b'oversized').status_code == 413
