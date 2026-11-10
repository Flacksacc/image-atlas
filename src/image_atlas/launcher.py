"""Console-free Windows entry point for the ImageAtlas Start menu shortcut."""
import ctypes
import subprocess
import sys


def main():
    try:
        result = subprocess.run(
            [sys.executable, '-m', 'image_atlas.cli', 'view'],
            capture_output=True, text=True, encoding='utf-8', errors='replace',
            creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0),
        )
        if result.returncode:
            raise RuntimeError(result.stderr.strip() or result.stdout.strip() or 'Viewer could not start')
    except (OSError, RuntimeError) as exc:
        message = f'ImageAtlas could not start.\n\n{exc}'
        if sys.platform == 'win32':
            ctypes.windll.user32.MessageBoxW(None, message, 'ImageAtlas', 0x10)
        else:
            print(message, file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
