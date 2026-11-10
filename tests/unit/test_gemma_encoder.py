"""Model loading and worker protocol checks without downloading weights."""
import io
import json
import sys
from contextlib import nullcontext
from types import SimpleNamespace
from unittest.mock import Mock

import numpy as np
import pytest
from PIL import Image

from image_atlas import embeddings, embedding_worker, image_query
from image_atlas.embedding_models import GEMMA, OPENCLIP, get_model


def test_model_identifiers_are_allowlisted():
    assert get_model(GEMMA.id) == GEMMA
    assert get_model('openclip') == OPENCLIP
    with pytest.raises(ValueError):
        get_model('embedding; DROP TABLE images')


@pytest.mark.parametrize('cpu,bf16,expected', [(True, True, 'fp32'), (False, True, 'bf16'), (False, False, 'fp32')])
def test_gemma_loads_only_text_and_images_and_normalizes_512d(tmp_path, monkeypatch, cpu, bf16, expected):
    torch = SimpleNamespace(cuda=SimpleNamespace(is_available=lambda: True, is_bf16_supported=lambda: bf16),
                            bfloat16='bf16', float32='fp32', inference_mode=nullcontext)
    model = Mock()
    model.encode.return_value = np.array([1.] + [0.]*511)
    constructor = Mock(return_value=model)
    monkeypatch.setitem(sys.modules, 'torch', torch)
    monkeypatch.setitem(sys.modules, 'sentence_transformers', SimpleNamespace(SentenceTransformer=constructor))
    encoder = embeddings.GemmaEmbedder(cpu=cpu)
    assert constructor.call_args.args == ('google/embeddinggemma-2',)
    assert constructor.call_args.kwargs['config_kwargs'] == {'audio_config': None}
    assert constructor.call_args.kwargs['model_kwargs']['torch_dtype'] == expected
    assert len(encoder.text('red car')) == 512
    assert model.encode.call_args.kwargs['prompt'] == 'task: search result | query: '
    photo = tmp_path / 'reference.png'
    Image.new('RGB', (24, 20), 'red').save(photo)
    assert len(encoder.reference_image(photo)) == 512
    assert isinstance(model.encode.call_args.args[0]['image'], Image.Image)
    assert model.encode.call_args.kwargs['prompt'] == ''
    assert model.encode.call_args.kwargs['normalize_embeddings'] is True
    assert model.encode.call_args.kwargs['truncate_dim'] == 512
    model.encode.return_value = np.full(512, np.nan)
    with pytest.raises(ValueError, match='Invalid'):
        encoder.text('bad output')


def test_worker_honors_model_flag_and_keeps_loader_output_out_of_protocol(monkeypatch):
    encoder = Mock()
    encoder.device = 'cpu'
    encoder.text.return_value = [1.] + [0.]*511
    def factory(**kwargs):
        assert kwargs == {'cpu': True, 'model': GEMMA.key}
        print('Loading model')
        return encoder
    monkeypatch.setattr(embedding_worker, 'local_embedder', factory)
    monkeypatch.setattr(sys, 'stdin', io.StringIO(json.dumps({'operation': 'text', 'value': 'photo'}) + '\n'))
    output = io.StringIO()
    monkeypatch.setattr(sys, 'stdout', output)
    embedding_worker.main(['--cpu', '--model', GEMMA.key])
    assert len(json.loads(output.getvalue())['embedding']) == 512
    encoder.close.assert_called_once()


def test_search_selects_only_the_requested_vector_space():
    conn = Mock()
    conn.execute.return_value.fetchall.return_value = []
    image_query.search(conn, [1.] + [0.]*511, model=GEMMA.key)
    sql, args = conn.execute.call_args.args
    assert 'gemma_embedding <=>' in sql
    assert 'gemma_embedding_model=%s' in sql
    assert GEMMA.id in args and OPENCLIP.id not in args
