"""Windows shell actions in a credential-free helper; no database imports."""
import ctypes
import sys

def main():
    if sys.platform != 'win32' or len(sys.argv) != 3 or sys.argv[1] not in {'open', 'openas'}:
        raise SystemExit('Unsupported shell action')
    execute = ctypes.windll.shell32.ShellExecuteW
    execute.argtypes = [ctypes.c_void_p, ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.c_wchar_p, ctypes.c_int]
    execute.restype = ctypes.c_void_p
    result = execute(None, sys.argv[1], sys.argv[2], None, None, 1) or 0
    if result <= 32:
        raise SystemExit(f'Windows could not open the selected path (code {result})')

if __name__ == '__main__':
    main()
