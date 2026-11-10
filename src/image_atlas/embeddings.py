"""Local image and text vectors for semantic search and visual similarity."""
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

from .cloud import unavailable_cloud_file
from .documents import DOCUMENT_EXTENSIONS
from .image_io import HEIF_EXTENSIONS, RAW_EXTENSIONS, open_image
from .worker_env import model_python
from .embedding_models import get_model, GEMMA

MODEL_NAME = "ViT-B-32"
PRETRAINED = "laion2b_s34b_b79k"
MODEL_ID = f"open_clip:{MODEL_NAME}:{PRETRAINED}"


class EmbeddingSetupError(RuntimeError):
    """A broken worker setup must stop the stage without marking images failed."""


class Embedder:
    def __init__(self, *, cpu: bool = False):
        try:
            import open_clip
            import torch
        except ImportError as exc:
            raise RuntimeError("Install the vision extra: pip install -e '.[vision]'") from exc
        self.torch = torch
        self.device = "cpu" if cpu or not torch.cuda.is_available() else "cuda"
        self.model, _, self.preprocess = open_clip.create_model_and_transforms(
            MODEL_NAME, pretrained=PRETRAINED, device=self.device
        )
        self.model.eval()
        self.tokenizer = open_clip.get_tokenizer(MODEL_NAME)

    def image(self, path: Path):
        with open_image(path) as (source, _, _, _):
            pixels = self.preprocess(source.convert("RGB")).unsqueeze(0).to(self.device)
        with self.torch.inference_mode():
            vector = self.model.encode_image(pixels)
            vector = vector / vector.norm(dim=-1, keepdim=True)
        return vector[0].float().cpu().tolist()

    def text(self, query: str):
        tokens = self.tokenizer([query]).to(self.device)
        with self.torch.inference_mode():
            vector = self.model.encode_text(tokens)
            vector = vector / vector.norm(dim=-1, keepdim=True)
        return vector[0].float().cpu().tolist()

    def reference_image(self, path: Path):
        from .image_query import open_reference
        with open_reference(path) as source:
            pixels = self.preprocess(source).unsqueeze(0).to(self.device)
        with self.torch.inference_mode():
            vector = self.model.encode_image(pixels)
            vector = vector / vector.norm(dim=-1, keepdim=True)
        return vector[0].float().cpu().tolist()

    def close(self):
        del self.model
        if self.device == "cuda":
            self.torch.cuda.empty_cache()


class GemmaEmbedder(Embedder):
    """Local EmbeddingGemma 2, text and vision only, normalized to 512 dimensions."""
    def __init__(self, *, cpu=False):
        try:
            import torch
            from sentence_transformers import SentenceTransformer
        except ImportError as exc:
            raise EmbeddingSetupError('Install EmbeddingGemma support: pip install -e ".[gemma]"') from exc
        self.torch = torch
        self.device = 'cpu' if cpu or not torch.cuda.is_available() else 'cuda'
        dtype = torch.bfloat16 if self.device == 'cuda' and torch.cuda.is_bf16_supported() else torch.float32
        try:
            self.model = SentenceTransformer(
                'google/embeddinggemma-2', device=self.device,
                config_kwargs={'audio_config': None}, model_kwargs={'torch_dtype': dtype},
                truncate_dim=512,
            )
            self.model.eval()
        except Exception as exc:
            raise EmbeddingSetupError(
                'Could not load EmbeddingGemma 2. Install the gemma extra and check access to '
                'google/embeddinggemma-2 on Hugging Face. ' + str(exc)
            ) from exc

    def _encode(self, value, *, prompt=''):
        from .image_query import validate_vector
        with self.torch.inference_mode():
            vector = self.model.encode(value, prompt=prompt, normalize_embeddings=True,
                                       truncate_dim=512, show_progress_bar=False)
        return validate_vector(vector.reshape(-1).tolist())

    def image(self, path: Path):
        from PIL import ImageOps
        with open_image(path) as (source, _, _, _):
            with ImageOps.exif_transpose(source) as oriented:
                with oriented.convert('RGB') as pixels:
                    return self._encode({'image': pixels})

    def reference_image(self, path: Path):
        from .image_query import open_reference
        with open_reference(path) as source:
            return self._encode({'image': source})

    def text(self, query: str):
        return self._encode(query, prompt='task: search result | query: ')


def local_embedder(*, cpu=False, model='openclip'):
    return GemmaEmbedder(cpu=cpu) if get_model(model) == GEMMA else Embedder(cpu=cpu)


class EmbeddingWorkerClient:
    def __init__(self, python: str, *, cpu: bool = False, model='openclip'):
        spec = get_model(model)
        env = os.environ.copy()
        env.pop('IMAGE_CLI_DSN', None)
        env.pop('IMAGEATLAS_DB_PASSWORD', None)
        env.pop('PGPASSWORD', None)
        env.pop('PGPASSFILE', None)
        env["PYTHONPATH"] = os.pathsep.join(filter(None, [str(Path(__file__).resolve().parents[1]), env.get("PYTHONPATH", "")]))
        flags = subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0
        self.process = subprocess.Popen(
            [python, "-m", "image_atlas.embedding_worker", '--model', spec.key, *( ["--cpu"] if cpu else [] )],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            text=True, encoding="utf-8", env=env, creationflags=flags,
        )
        try:
            ready = self._exchange('check', '')
            if ready.get('ready') is not True:
                raise EmbeddingSetupError('Embedding worker did not confirm startup')
            self.decoders = ready.get('decoders', {})
            self.device = ready.get('device', 'unknown')
        except BaseException:
            self.abort()
            raise

    def _exchange(self, operation: str, value: str):
        try:
            self.process.stdin.write(json.dumps({"operation": operation, "value": value}) + "\n")
            self.process.stdin.flush()
        except (OSError, ValueError) as exc:
            raise EmbeddingSetupError('Embedding worker is unavailable') from exc
        line = self.process.stdout.readline()
        if not line:
            raise EmbeddingSetupError(f"Embedding worker exited unexpectedly ({self.process.poll()})")
        try:
            response = json.loads(line)
        except json.JSONDecodeError as exc:
            raise EmbeddingSetupError('Invalid embedding worker response') from exc
        if "error" in response:
            if response.get('fatal'):
                raise EmbeddingSetupError(response['error'])
            raise RuntimeError(response["error"])
        return response

    def _request(self, operation: str, value: str):
        return self._exchange(operation, value)['embedding']

    def image(self, path: Path):
        suffix = path.suffix.lower()
        if ((suffix in HEIF_EXTENSIONS and not self.decoders.get('heif')) or
                (suffix in RAW_EXTENSIONS and not self.decoders.get('raw')) or
                suffix in DOCUMENT_EXTENSIONS):
            # Optional decoders stay in the CLI env; send a lossless raster to the GPU worker.
            with tempfile.TemporaryDirectory(prefix='image-atlas-vision-') as directory:
                raster = Path(directory) / 'decoded.png'
                with open_image(path) as (source, _, _, _):
                    source.convert('RGB').save(raster)
                return self._request('image', str(raster))
        return self._request("image", str(path))

    def text(self, query: str):
        return self._request("text", query)

    def reference_image(self, path: Path):
        return self._request('reference-image', str(path))

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            self.process.wait(timeout=20)

    def abort(self):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=5)


def create_embedder(*, cpu: bool = False, model='openclip'):
    spec = get_model(model)
    modules = ('torch', 'sentence_transformers') if spec == GEMMA else ('torch', 'open_clip')
    other_python = model_python('IMAGE_CLI_GEMMA_PYTHON' if spec == GEMMA else 'IMAGE_CLI_VISION_PYTHON', modules)
    return EmbeddingWorkerClient(other_python, cpu=cpu, model=spec) if other_python else local_embedder(cpu=cpu, model=spec)


def process_pending(conn, *, limit: int | None = None, cpu: bool = False, retry_errors: bool = False,
                    root_path: str | None = None, image_ids: list[int] | None = None, progress=None,
                    selected_table: bool = False, model='openclip', runtime=None):
    from .db import pending_images, vector_literal
    spec = get_model(model)
    vector_column, model_column = spec.column(), spec.column('_model')
    status_column, error_column = spec.column('_status'), spec.column('_error')
    encoder = None
    result = {"processed": 0, "skipped_cloud": 0, "errors": []}
    try:
        for row in pending_images(conn, status_column, limit=limit, retry_errors=retry_errors,
                                  root_path=root_path, image_ids=image_ids, selected_table=selected_table):
            if unavailable_cloud_file(Path(row["path"])):
                result["skipped_cloud"] += 1
                if progress is not None:
                    progress()
                continue
            if encoder is None:
                encoder = create_embedder(cpu=cpu, model=spec.key)
                if runtime is not None:
                    runtime(model='OpenCLIP ViT-B/32' if spec.key == 'openclip' else spec.name,
                            model_id=spec.id, device=str(getattr(encoder, 'device', 'unknown')))
            try:
                from .catalogs import resolve
                catalog = resolve()
                if catalog.id:
                    catalog.require_path(row['path'])
                vector = vector_literal(encoder.image(Path(row["path"])))
                conn.execute(f"""UPDATE images SET {vector_column}=%s::halfvec,{model_column}=%s,
                               {status_column}='done',{error_column}=NULL WHERE id=%s""", (vector, spec.id, row["id"]))
                result["processed"] += 1
            except (EmbeddingSetupError, ModuleNotFoundError):
                raise
            except Exception as exc:  # noqa: BLE001 - isolate failures per image
                conn.execute(f"UPDATE images SET {status_column}='error',{error_column}=%s WHERE id=%s",
                             (str(exc)[:1000], row["id"]))
                result["errors"].append({"path": row["path"], "error": str(exc)})
            conn.commit()
            if progress is not None:
                progress()
    finally:
        if encoder is not None and hasattr(encoder, "close"):
            if sys.exc_info()[0] is not None and hasattr(encoder, "abort"):
                encoder.abort()
            else:
                encoder.close()
    return result
