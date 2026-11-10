"""Bounded OpenCV face inference. This module has no database dependencies."""
import os
import tempfile
import urllib.request
from pathlib import Path

MODEL_FILES = {
    'detector': ('face_detection_yunet_2023mar.onnx', 'https://github.com/opencv/opencv_zoo/raw/refs/heads/main/models/face_detection_yunet/face_detection_yunet_2023mar.onnx'),
    'recognizer': ('face_recognition_sface_2021dec.onnx', 'https://github.com/opencv/opencv_zoo/raw/refs/heads/main/models/face_recognition_sface/face_recognition_sface_2021dec.onnx'),
}
MAX_EDGE = 2048


def model_file(kind):
    base = Path(os.environ.get('IMAGE_CLI_MODEL_DIR', Path.home() / '.cache' / 'image-cli'))
    base.mkdir(parents=True, exist_ok=True)
    name, url = MODEL_FILES[kind]
    target = base / name
    if target.exists():
        return target
    with tempfile.NamedTemporaryFile(dir=base, delete=False) as tmp:
        temporary = Path(tmp.name)
    try:
        urllib.request.urlretrieve(url, temporary)
        if temporary.stat().st_size < 100_000:
            raise RuntimeError(f'Model download was incomplete: {url}')
        temporary.replace(target)
    finally:
        temporary.unlink(missing_ok=True)
    return target


class FaceEngine:
    def __init__(self):
        os.environ.setdefault('OPENCV_LOG_LEVEL', 'ERROR')
        try:
            import cv2
        except ImportError as exc:
            raise RuntimeError("Install the vision extra: pip install -e '.[vision]'") from exc
        self.cv2 = cv2
        self.device = 'cpu'  # FaceDetectorYN / FaceRecognizerSF use their default OpenCV CPU backend.
        cv2.setNumThreads(1)
        if hasattr(cv2, 'setLogLevel'):
            cv2.setLogLevel(2)
        self.detector = cv2.FaceDetectorYN.create(str(model_file('detector')), '', (320, 320), .85)
        self.recognizer = cv2.FaceRecognizerSF.create(str(model_file('recognizer')), '')

    def detect(self, path):
        import numpy as np
        from PIL import Image, ImageOps
        from .image_io import open_image
        with open_image(Path(path)) as (source, original_width, original_height, _):
            orientation = source.getexif().get(274, 1)
            if orientation in {5, 6, 7, 8}:
                original_width, original_height = original_height, original_width
            # JPEG draft avoids decoding an enormous full-resolution raster when supported.
            ratio = min(1., MAX_EDGE / max(source.size))
            source.draft('RGB', (max(1, round(source.width * ratio)), max(1, round(source.height * ratio))))
            with ImageOps.exif_transpose(source) as oriented:
                oriented.thumbnail((MAX_EDGE, MAX_EDGE), Image.Resampling.LANCZOS)
                with oriented.convert('RGB') as reduced:
                    rgb = np.array(reduced)
        image = self.cv2.cvtColor(rgb, self.cv2.COLOR_RGB2BGR)
        height, width = image.shape[:2]
        self.detector.setInputSize((width, height))
        _, found = self.detector.detect(image)
        for face in found if found is not None else []:
            aligned = self.recognizer.alignCrop(image, face)
            embedding = self.recognizer.feature(aligned).flatten().astype('float32')
            norm = float(np.linalg.norm(embedding))
            if not np.isfinite(embedding).all() or not np.isfinite(norm) or norm <= 1e-12:
                raise ValueError('Face recognizer returned an invalid embedding')
            embedding /= norm
            sx, sy = original_width / width, original_height / height
            x, y, w, h = (round(value * scale) for value, scale in zip(face[:4], (sx, sy, sx, sy)))
            x, y = min(max(x, 0), original_width - 1), min(max(y, 0), original_height - 1)
            w, h = min(max(w, 1), original_width - x), min(max(h, 1), original_height - y)
            yield {'box': (x, y, w, h), 'embedding': embedding.tolist(), 'confidence': float(face[-1])}
