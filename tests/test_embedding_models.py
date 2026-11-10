"""Two model indexes must survive switching, resumable mining, and source changes."""
import json
from pathlib import Path
from unittest.mock import Mock

from fastapi.testclient import TestClient
from PIL import Image

from cli_helpers import JsonCliRunner
from image_atlas import catalogs, embeddings, image_query
from image_atlas.cli import app as cli
from image_atlas.db import connect
from image_atlas.embedding_models import GEMMA, OPENCLIP
from image_atlas.indexer import scan
from image_atlas.remote_server import create_app
from image_atlas.web import app


def test_independent_indexes_resume_and_both_search_paths_select_the_model(tmp_path, monkeypatch):
    record = catalogs.register('Two models', catalogs.resolve().dsn, [str(tmp_path)])
    first = tmp_path / 'first.png'
    second = tmp_path / 'second.png'
    for path in (first, second):
        Image.new('RGB', (20, 20), 'red').save(path)
    near = [1.] + [0.]*511
    far = [.8, .6] + [0.]*510
    calls = []
    def factory(**kwargs):
        model = kwargs.get('model', OPENCLIP.key)
        encoder = Mock()
        encoder.device = 'cpu'
        def image(path):
            calls.append((model, path.name))
            return near if (path.name == 'first.png') == (model == OPENCLIP.key) else far
        encoder.image.side_effect = image
        encoder.reference_image.return_value = near
        encoder.text.return_value = near
        return encoder
    monkeypatch.setattr(embeddings, 'create_embedder', factory)
    runner = JsonCliRunner()
    base = ['--database', record['id']]
    result = runner.invoke(cli, base + ['mine', str(tmp_path), '--tags', 'embeddings'])
    assert result.exit_code == 0, result.output
    for _ in range(2):
        result = runner.invoke(cli, base + ['mine', str(tmp_path), '--tags', 'embeddings', '--embedding-model', GEMMA.key, '--limit', '1'])
        assert result.exit_code == 0, result.output
        assert json.loads(result.output)['stages']['embeddings']['processed'] == 1
        runtime = json.loads(result.output)['stages']['embeddings']
        assert runtime['model'] == 'EmbeddingGemma 2' and runtime['model_id'] == GEMMA.id
        assert runtime['device'] == 'cpu' and runtime['gpu_used'] is False
    assert len(calls) == 4
    result = runner.invoke(cli, base + ['mine', str(tmp_path), '--tags', 'embeddings', '--embedding-model', GEMMA.key, '--limit', '1'])
    assert result.exit_code == 0 and len(calls) == 4
    with connect() as conn:
        rows = conn.execute('SELECT id,path,embedding_model,gemma_embedding_model,embedding_status,gemma_embedding_status FROM images ORDER BY id').fetchall()
        ids = {Path(row['path']).name: row['id'] for row in rows}
        assert all(row['embedding_model'] == OPENCLIP.id and row['gemma_embedding_model'] == GEMMA.id for row in rows)
        assert all(row['embedding_status'] == row['gemma_embedding_status'] == 'done' for row in rows)
        assert image_query.search(conn, near)[0]['id'] == ids['first.png']
        assert image_query.search(conn, near, model=GEMMA.key)[0]['id'] == ids['second.png']
    for command in (['semantic-search', 'red photo'], ['image-search', str(first)], ['similar', str(ids['first.png'])]):
        result = runner.invoke(cli, base + command + ['--model', GEMMA.key])
        assert result.exit_code == 0, result.output
        assert json.loads(result.output)[0]['id'] == ids['second.png']
    client = TestClient(app, base_url='http://127.0.0.1', headers={'X-Image-Atlas-Session': app.state.session_token})
    prefix = '/api/catalogs/' + record['id']
    response = client.get(prefix + '/search/text', params={'q': 'photo', 'model': GEMMA.key})
    assert response.status_code == 200, response.text
    assert response.json()['items'][0]['id'] == ids['second.png']
    response = client.post(prefix + '/search/image', params={'model': GEMMA.key}, content=first.read_bytes())
    assert response.status_code == 200, response.text
    assert response.json()['items'][0]['id'] == ids['second.png']
    response = client.post(prefix + '/search/image/page', json={'vector': near, 'model': GEMMA.key, 'offset': 1})
    assert response.status_code == 200 and response.json()[0]['id'] == ids['first.png']
    server = create_app(catalogs.resolve(record['id']))
    with TestClient(server, base_url='http://127.0.0.1', headers={'Authorization': 'Bearer ' + server.state.token}) as remote:
        assert GEMMA.id in remote.get('/v1/session').json()['image_search_models']
        response = remote.get('/v1/images', params={'q': 'photo', 'mode': 'visual', 'model': GEMMA.key})
        assert response.status_code == 200, response.text
        assert response.json()['items'][0]['id'] == ids['second.png']
        response = remote.get('/v1/images', params={'image_vector': json.dumps(near), 'model': GEMMA.key})
        assert response.status_code == 200 and response.json()['items'][0]['id'] == ids['second.png']
        metadata = remote.get('/v1/images/' + str(ids['first.png'])).json()
        assert 'embedding' not in metadata and 'gemma_embedding' not in metadata
        assert remote.get('/v1/images', params={'model': 'invalid'}).status_code == 400
    # A changed original must invalidate both indexes, without retaining stale Gemma results.
    Image.new('RGB', (23, 20), 'blue').save(first)
    with connect() as conn:
        scan(conn, str(tmp_path))
        row = conn.execute('SELECT embedding,gemma_embedding,embedding_status,gemma_embedding_status FROM images WHERE id=%s', (ids['first.png'],)).fetchone()
        assert row['embedding'] is None and row['gemma_embedding'] is None
        assert row['embedding_status'] == row['gemma_embedding_status'] == 'pending'
