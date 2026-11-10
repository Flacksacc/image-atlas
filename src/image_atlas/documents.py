"""Read-only Adobe document inspection and bounded, cached preview workers."""
import hashlib
import json
import os
import re
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from xml.etree import ElementTree

import psutil
from PIL import Image

DOCUMENT_EXTENSIONS = {'.psd', '.psb', '.ai'}
PREVIEW_EDGE = 2048
CACHE_VERSION = 1
WORKER_TIMEOUT = 25
WORKER_MEMORY = 2 * 1024**3


class PreviewUnavailable(ValueError):
    pass


def basic_metadata(path):
    """Keep metadata independent of successful rasterization or optional decoders."""
    path = Path(path)
    with path.open('rb') as source:
        header = source.read(1024 * 1024)
        source.seek(max(0, path.stat().st_size - 1024 * 1024))
        tail = source.read(1024 * 1024)
    result = {'format': path.suffix[1:].upper(), 'preview_available': False}
    if path.suffix.lower() in {'.psd', '.psb'}:
        if len(header) >= 26 and header[:4] == b'8BPS':
            version, channels, height, width, depth, mode = struct.unpack('>H6xHIIHH', header[4:26])
            result.update(version=version, channels=channels, width=width, height=height,
                          bit_depth=depth, color_mode=mode)
        else:
            result['metadata_warning'] = 'Invalid or truncated Photoshop header'
    else:
        result['pdf_compatible'] = b'%PDF-' in header[:1024]
        text = header.decode('utf-8', errors='replace')
        result['dsc'] = {key: value[:8192] for key, value in
                         re.findall(r'^%%([A-Za-z][A-Za-z0-9]*):[ \t]*([^\r\n]*)', text, re.MULTILINE)}
        for name in ('Title', 'Creator', 'CreationDate', 'For', 'Pages', 'BoundingBox', 'HiResBoundingBox', 'DocumentData'):
            match = re.search(r'^%%' + name + r':\s*([^\r\n]*)', text, re.MULTILINE)
            if match:
                result[name] = match[1][:8192]
        box = result.get('HiResBoundingBox', result.get('BoundingBox', ''))
        try:
            left, bottom, right, top = map(float, box.split())
            if 0 < right-left < 2**31 and 0 < top-bottom < 2**31:
                result.update(width=round(right-left), height=round(top-bottom), dimension_unit='points')
        except (ValueError, OverflowError):
            pass
    # XMP is commonly in the header or trailer; keep the raw packet as well as fields.
    for block in (header, tail):
        match = re.search(rb'<(?:\w+:)?xmpmeta\b.*?</(?:\w+:)?xmpmeta>', block, re.DOTALL)
        if not match:
            continue
        packet = match[0]
        result['xmp'] = packet.decode('utf-8', errors='replace')
        try:
            tree = ElementTree.fromstring(packet)
            result['xmp_fields'] = {
                node.tag: {'text': ' '.join(node.itertext()).strip()[:8192], 'attributes': dict(node.attrib)}
                for node in tree.iter() if node.text or node.attrib
            }
        except ElementTree.ParseError:
            result['metadata_warning'] = 'Embedded XMP could not be parsed; original packet retained'
        break
    return result


def _cache_root():
    return Path(os.environ.get('IMAGE_CLI_DOCUMENT_CACHE',
                Path(os.environ.get('LOCALAPPDATA', Path.home() / '.cache')) / 'image-cli' / 'document-previews'))


def is_document_cache(path):
    return Path(path).resolve().is_relative_to(_cache_root().resolve())


def document_info(path: Path, *, page: int = 0):
    """Return document metadata plus an optional cached PNG path (zero-based page)."""
    path = Path(path).resolve()
    if page < 0:
        raise ValueError('Artboard number cannot be negative')
    stat = path.stat()
    key = hashlib.sha256(f'{CACHE_VERSION}:{path}:{stat.st_size}:{stat.st_mtime_ns}:{page}'.encode()).hexdigest()
    base = _cache_root()
    base.mkdir(parents=True, exist_ok=True)
    manifest = base / f'{key}.json'
    preview = base / f'{key}.png'
    try:
        info = json.loads(manifest.read_text(encoding='utf-8'))
        if not info['preview_available'] or preview.is_file():
            return info, preview if info['preview_available'] else None
    except (OSError, ValueError, KeyError):
        pass
    info = basic_metadata(path)
    if path.suffix.lower() == '.ai' and not info['pdf_compatible']:
        info['preview_reason'] = 'Illustrator file has no PDF-compatible content; metadata indexed without a thumbnail.'
    else:
        env = os.environ.copy()
        for name in tuple(env):
            if name.upper().startswith(('PG', 'IMAGE_CLI_CREDENTIAL')) or name in {
                'IMAGE_CLI_DSN', 'IMAGEATLAS_DB_PASSWORD', 'ASSET_CLI_DSN', 'ASSET_CLI_VIEWER_DSN', 'IMAGE_CLI_VIEWER_CONTEXT',
                'IMAGE_CLI_CATALOG_ID', 'IMAGE_CLI_ALLOWED_ROOTS', 'IMAGE_CLI_READONLY',
            }:
                env.pop(name, None)
        env['PYTHONPATH'] = str(Path(__file__).resolve().parents[1])
        flags = subprocess.CREATE_NO_WINDOW if sys.platform == 'win32' else 0
        # Each invocation owns its scratch directory. Atomic replacements allow concurrent readers.
        with tempfile.TemporaryDirectory(prefix='document-', dir=base) as temporary:
            output = Path(temporary)
            process = subprocess.Popen([sys.executable, '-m', 'image_atlas.document_worker', str(path),
                                        str(output), str(page)], env=env, creationflags=flags,
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            deadline = time.monotonic() + WORKER_TIMEOUT
            try:
                while process.poll() is None:
                    if time.monotonic() > deadline:
                        raise PreviewUnavailable('Document preview exceeded the 25-second time limit')
                    try:
                        if psutil.Process(process.pid).memory_info().rss > WORKER_MEMORY:
                            raise PreviewUnavailable('Document preview exceeded the 2 GiB memory limit')
                    except psutil.NoSuchProcess:
                        pass
                    time.sleep(.05)
                if (output / 'metadata.json').is_file():
                    info.update(json.loads((output / 'metadata.json').read_text(encoding='utf-8')))
                if process.returncode != 0:
                    raise PreviewUnavailable(info.get('preview_reason', 'Document decoder exited unexpectedly'))
                if info['preview_available']:
                    (output / 'preview.png').replace(preview)
            except PreviewUnavailable as exc:
                if process.poll() is None:
                    process.kill()
                process.wait()
                if (output / 'metadata.json').is_file():
                    info.update(json.loads((output / 'metadata.json').read_text(encoding='utf-8')))
                info.update(preview_available=False, preview_reason=str(exc))
            finally:
                if process.poll() is None:
                    process.kill()
                process.wait()
    with tempfile.NamedTemporaryFile(mode='w', encoding='utf-8', dir=base, delete=False) as target:
        json.dump(info, target)
        temporary_manifest = Path(target.name)
    temporary_manifest.replace(manifest)
    return info, preview if info['preview_available'] else None


def document_image(path, *, page=0):
    info, preview = document_info(path, page=page)
    if preview is None:
        raise PreviewUnavailable(info.get('preview_reason', 'No document preview available'))
    return info, Image.open(preview)
