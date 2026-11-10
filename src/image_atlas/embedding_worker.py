"""Line-delimited JSON OpenCLIP worker for an existing CUDA environment."""
import json
import importlib.util
import sys
from pathlib import Path

from .embeddings import Embedder
from .image_io import HEIF_AVAILABLE


def main():
    cpu = "--cpu" in sys.argv[1:]
    encoder = None
    for line in sys.stdin:
        fatal = False
        try:
            request = json.loads(line)
            if encoder is None:
                try:
                    encoder = Embedder(cpu=cpu)
                except Exception:
                    fatal = True
                    raise
            if request['operation'] == 'check':
                output = {'ready': True, 'device': str(getattr(encoder, 'device', 'unknown')), 'decoders': {'heif': HEIF_AVAILABLE,
                           'raw': importlib.util.find_spec('rawpy') is not None}}
            elif request["operation"] == "image":
                vector = encoder.image(Path(request["value"]))
                output = {"embedding": vector}
            elif request["operation"] == "text":
                vector = encoder.text(request["value"])
                output = {"embedding": vector}
            elif request['operation'] == 'reference-image':
                output = {'embedding': encoder.reference_image(Path(request['value']))}
            else:
                raise ValueError("Unknown embedding operation")
        except Exception as exc:  # noqa: BLE001 - return errors over the worker protocol
            fatal = fatal or isinstance(exc, ModuleNotFoundError)
            output = {"error": str(exc), "fatal": fatal}
        sys.stdout.write(json.dumps(output) + "\n")
        sys.stdout.flush()
        if fatal:
            break
    if encoder is not None:
        encoder.close()


if __name__ == "__main__":
    main()
