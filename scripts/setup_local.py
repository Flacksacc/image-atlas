"""Create private local database credentials; preserve existing installations."""
import secrets
from pathlib import Path


def setup(root=None):
    destination = Path(root or Path(__file__).resolve().parents[1]) / '.env'
    try:
        with destination.open('x', encoding='utf-8') as handle:
            handle.write('IMAGEATLAS_DB_PASSWORD=' + secrets.token_urlsafe(32) + '\n')
    except FileExistsError:
        print('Existing .env preserved. Keep its password when reusing the database volume.')
    else:
        print('Created private .env database settings. This file is ignored by Git.')
    return destination


if __name__ == '__main__':
    setup()
