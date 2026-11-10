"""Private line-delimited IPC protocol; never a user-facing CLI report."""
import json
import sys
from pathlib import Path
from .face_engine import FaceEngine


def main():
    engine = None
    for line in sys.stdin:
        fatal = False
        try:
            request = json.loads(line)
            if engine is None:
                try:
                    engine = FaceEngine()
                except Exception:
                    fatal = True
                    raise
            if request['operation'] == 'check':
                response = {'ready': True}
            elif request['operation'] == 'detect':
                response = {'faces': list(engine.detect(Path(request['path'])))}
            else:
                raise ValueError('Unknown face worker operation')
        except Exception as exc:
            response = {'error': str(exc), 'fatal': fatal}
        print(json.dumps(response, allow_nan=False), flush=True)
        if fatal:
            break


if __name__ == '__main__':
    main()
