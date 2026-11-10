"""Native remote viewer bridge. Credentials remain in the local session broker."""
import io
import json
import sys

from PIL import Image

from . import remote_client as remote


def main():
    action = sys.argv[1]
    if action == 'search':
        args = json.loads(sys.stdin.readline())
        path = args.pop('image_path', None)
        vector = args.get('image_vector')
        if path:
            from .image_query import encode_reference
            vector = args['image_vector'] = encode_reference(path)
        result = remote.search(**args)
        if vector is not None:
            result['reference_vector'] = vector
        print(json.dumps(result, default=str))
    elif action in {'thumb', 'preview'}:
        with remote.request(f'/v1/images/{int(sys.argv[2])}/{action}') as response:
            data = response.read(16*1024*1024+1)
        if len(data) > 16*1024*1024:
            raise ValueError('Preview response too large')
        if action == 'preview':
            from .previews import decode
            image = decode(data)
        else:
            image = Image.open(io.BytesIO(data))
        image = image.convert('RGBA')
        if max(image.size) > 1024:
            raise ValueError('Invalid preview dimensions')
        sys.stdout.buffer.write(f'{image.width} {image.height}\n'.encode()+image.tobytes())
    elif action == 'original':
        print(json.dumps({'path': remote.open_original(int(sys.argv[2]))}))
    else:
        raise ValueError('Unknown viewer operation')


if __name__ == '__main__':
    try:
        main()
    except Exception as exc:
        # Never dump URLs with connection credentials or environment contents.
        print('Remote operation failed: '+str(exc), file=sys.stderr)
        raise SystemExit(1) from None
