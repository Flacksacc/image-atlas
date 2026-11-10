import subprocess
import sys

import pytest
from PIL import Image

from image_atlas.db import connect, initialize
from image_atlas.documents import document_info
from image_atlas.indexer import metadata, scan, select_images_for_mine


@pytest.fixture(autouse=True)
def document_cache(tmp_path, monkeypatch):
    monkeypatch.setenv('IMAGE_CLI_DOCUMENT_CACHE', str(tmp_path / 'previews'))


def write_ai(path, pages=1):
    import pypdfium2 as pdfium
    with pdfium.PdfDocument.new() as pdf:
        for index in range(pages):
            pdf.new_page(300 + index * 100, 200).close()
        pdf.save(path)


def test_native_ai_keeps_metadata_without_thumbnail_or_repeated_work(tmp_path):
    source = tmp_path / 'Illustration.AI'
    source.write_bytes(b'%!PS-Adobe-3.0\n%%Title: A sunset drawing\n%%Creator: Adobe Illustrator\n'
                       b'%%CreationDate: 2026-10-29\n%%BoundingBox: 0 0 640 480\n'
                       b'<x:xmpmeta xmlns:x="adobe:ns:meta/"><title>Sunset</title></x:xmpmeta>')
    initialize()
    with connect() as conn:
        counts = scan(conn, str(tmp_path))
        assert counts['new'] == counts['metadata_only'] == 1
        assert not counts['errors'] and counts['thumbnails_created'] == 0
        row = conn.execute('SELECT * FROM images').fetchone()
        assert row['format'] == 'AI' and row['sha256'] and row['size_bytes'] == source.stat().st_size
        assert (row['width'], row['height']) == (640, 480)
        doc = row['exif']['Document']
        assert row['generation'] == {}  # Adobe documents are not automatically AI-generated images.
        assert doc['Title'] == 'A sunset drawing' and 'Sunset' in doc['xmp']
        assert doc['pdf_compatible'] is doc['preview_available'] is False
        assert row['embedding_status'] == row['face_status'] == row['labels_status'] == 'skipped'
        assert conn.execute("SELECT search_text FROM image_search").fetchone()['search_text'].find('Sunset') >= 0
        assert scan(conn, str(tmp_path))['unchanged'] == 1
        assert select_images_for_mine(conn, tmp_path, ('embeddings', 'faces', 'labels'), 5)[0] == []
        moved = tmp_path / 'moved.ai'
        source.rename(moved)
        assert scan(conn, str(tmp_path))['moved'] == 1
        assert conn.execute('SELECT id FROM images').fetchone()['id'] == row['id']
        # Saving a PDF-compatible version makes visual work eligible again.
        moved.unlink()
        write_ai(moved)
        assert scan(conn, str(tmp_path))['updated'] == 1
        updated = conn.execute('SELECT * FROM images').fetchone()
        assert updated['exif']['Document']['preview_available'] is True
        assert updated['embedding_status'] == 'pending'
        assert conn.execute('SELECT count(*) AS n FROM thumbnails').fetchone()['n'] == 1


def test_metadata_only_ai_skips_all_model_stages(tmp_path, monkeypatch):
    from cli_helpers import JsonCliRunner

    from image_atlas import catalogs, embeddings, faces
    from image_atlas.cli import app
    from image_atlas.db import DEFAULT_DSN
    source = tmp_path / 'native.ai'
    source.write_bytes(b'%!PS-Adobe-3.0\n%%Title: No PDF preview\n')
    catalogs.register('Adobe test', DEFAULT_DSN.rsplit('/', 1)[0] + '/imagecli_test', roots=[str(tmp_path)])
    monkeypatch.setattr(embeddings, 'create_embedder', lambda **kwargs: pytest.fail('No embedding should be attempted'))
    monkeypatch.setattr(faces, 'FaceEngine', lambda *a, **k: pytest.fail('No face model should be started'))
    result = JsonCliRunner().invoke(app, ['mine', str(source)])
    assert result.exit_code == 0, result.stdout
    assert '"metadata_only": 1' in result.stdout


@pytest.mark.parametrize('suffix', ['.psd', '.psb'])
def test_photoshop_preview_metadata_and_shared_viewer(tmp_path, suffix):
    from psd_tools import PSDImage
    source = tmp_path / ('layers' + suffix)
    psd = PSDImage.new('RGB', (120, 80), color=(.25, .5, .75))
    psd._record.header.version = 2 if suffix == '.psb' else 1
    psd.create_pixel_layer(Image.new('RGBA', (30, 20), 'red'), name='Red square')
    psd.save(source)
    info = metadata(source, include_thumbnail=True)
    doc = info['exif']['Document']
    assert (info['width'], info['height'], info['format']) == (120, 80, suffix[1:].upper())
    assert doc['preview_available'] and doc['layer_count'] == 1
    assert doc['layers'][0]['name'] == 'Red square'
    assert info['thumbnail'][0] and info['dhash']
    result = subprocess.run([sys.executable, '-m', 'image_atlas.viewer_decode', str(source), '64'],
                            capture_output=True, timeout=30, check=False)
    assert result.returncode == 0, result.stderr
    header, pixels = result.stdout.split(b'\n', 1)
    width, height = map(int, header.split())
    assert max(width, height) == 64 and len(pixels) == width * height * 4
    result = subprocess.run([sys.executable, '-m', 'image_atlas.viewer_decode', str(source), '32',
                             '0', '0', '30', '20', '--page', '0'],
                            capture_output=True, timeout=30, check=False)
    assert result.returncode == 0, result.stderr


def test_pdf_ai_artboards_cache(tmp_path, monkeypatch):
    source = tmp_path / 'artboards.ai'
    write_ai(source, pages=2)
    first, preview = document_info(source)
    assert first['preview_available'] and first['page_count'] == 2 and preview.is_file()
    second, _ = document_info(source, page=1)
    assert second['width'] == 400 and second['preview_page'] == 1
    result = subprocess.run([sys.executable, '-m', 'image_atlas.viewer_decode', str(source), '128', '--page', '1'],
                            capture_output=True, timeout=30, check=False)
    assert result.returncode == 0, result.stderr
    assert result.stdout.split(b'\n', 1)[0] == b'128 64'
    assert not document_info(source, page=2)[0]['preview_available']
    monkeypatch.setattr(subprocess, 'Popen', lambda *a, **k: pytest.fail('Cache should prevent another render'))
    assert document_info(source)[0] == first


def test_corrupt_document_keeps_inventory_metadata(tmp_path):
    source = tmp_path / 'bad.psd'
    source.write_bytes(b'8BPS\x00')
    info = metadata(source, include_thumbnail=True)
    assert not info['exif']['Document']['preview_available']
    assert info['format'] == 'PSD' and info['width'] is None


def test_decoder_timeout_retains_metadata_and_cleans_worker(tmp_path, monkeypatch):
    from image_atlas import documents
    source = tmp_path / 'slow.ai'
    write_ai(source)
    monkeypatch.setattr(documents, 'WORKER_TIMEOUT', 0)
    info, preview = document_info(source)
    assert preview is None and 'time limit' in info['preview_reason']
    assert info['pdf_compatible'] is True


def test_adobe_preview_is_rasterized_in_cli_for_shared_gpu_worker(tmp_path, monkeypatch):
    from image_atlas.embeddings import EmbeddingWorkerClient
    source = tmp_path / 'shared.ai'
    write_ai(source)
    client = object.__new__(EmbeddingWorkerClient)
    client.decoders = {}
    paths = []
    def request(operation, value):
        paths.append(value)
        assert operation == 'image' and value.endswith('.png')
        with Image.open(value) as image:
            assert image.size == (600, 400)
        return [1.] * 512
    monkeypatch.setattr(client, '_request', request)
    assert len(client.image(source)) == 512
    from pathlib import Path
    assert not Path(paths[0]).exists()
