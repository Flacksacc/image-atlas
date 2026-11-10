"""Discover image paths from Everything's local Windows IPC index."""
import ctypes
import ntpath
import os
import struct
import sys
import time
from ctypes import wintypes
from pathlib import Path, PureWindowsPath

from .documents import is_document_cache
from .indexer import EXTENSIONS

WM_COPYDATA = 0x004A
QUERY2W = 18
REQUEST_FULL_PATH = 4
PAGE_SIZE = 1024
ROOT_DIRECTORY_NAMES = {"windows", "program files", "program files (x86)", "programdata",
                        "windowsapps", "$recycle.bin", "system volume information", "recovery"}
NESTED_DIRECTORY_NAMES = {"appdata", ".git", ".venv", "node_modules", "__pycache__"}


def system_exclusion_roots() -> tuple[Path, ...]:
    """Resolve installed system/application roots without assuming C: is the system drive."""
    names = ("SystemRoot", "WINDIR", "ProgramFiles", "ProgramFiles(x86)",
             "ProgramW6432", "ProgramData")
    roots = [Path(value) for name in names if (value := os.environ.get(name))]
    roots.extend(Path(value).parent for name in ("APPDATA", "LOCALAPPDATA")
                 if (value := os.environ.get(name)))
    return tuple(roots)


def _key(path: Path | str) -> str:
    return ntpath.normcase(ntpath.normpath(str(path)))


def _under(path: str, root: str) -> bool:
    return path == root or path.startswith(root.rstrip("\\") + "\\")


def allowed_image(path: Path, *, exclude_dirs: tuple[Path, ...] = (),
                  include_system: bool = False) -> bool:
    """Apply component and directory boundaries, independent of Everything syntax."""
    key = _key(path)
    if is_document_cache(path):
        return False
    from .catalogs import resolve, under
    catalog = resolve()
    if catalog.id and not any(under(path, root) for root in catalog.roots):
        return False
    if any(_under(key, _key(root)) for root in exclude_dirs):
        return False
    if include_system:
        return True
    if any(_under(key, _key(root)) for root in system_exclusion_roots()):
        return False
    directories = [part.casefold() for part in PureWindowsPath(path).parts[1:-1]]
    if directories and directories[0] in ROOT_DIRECTORY_NAMES:
        return False
    return not any(part in NESTED_DIRECTORY_NAMES for part in directories)


def image_query(*, exclude_dirs: tuple[Path, ...] = (), include_system: bool = False) -> str:
    """Push known roots into Everything so excluded files do not fill result pages."""
    search = "ext:" + ";".join(sorted(ext[1:] for ext in EXTENSIONS))
    from .catalogs import resolve
    catalog = resolve()
    if catalog.id:
        if not catalog.roots:
            raise ValueError('Catalog has no allowed folders')
        search += ' <' + ' | '.join('path:"' + p.rstrip('\\') + '\\"' for p in catalog.roots) + '>'
    roots = exclude_dirs if include_system else (*system_exclusion_roots(), *exclude_dirs)
    for root in dict.fromkeys(_key(path) for path in roots):
        if root and '"' not in root:
            prefix = root.rstrip("\\")
            search += f' !path:"{prefix}\\"'
    return search


def _query_page(search: str, offset: int) -> tuple[list[Path], int]:
    if sys.platform != "win32":
        raise RuntimeError("Search Everything requires Windows and a running Everything client")
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    user32.DefWindowProcW.argtypes = (wintypes.HWND, wintypes.UINT,
                                      wintypes.WPARAM, wintypes.LPARAM)
    user32.DefWindowProcW.restype = ctypes.c_ssize_t
    user32.FindWindowW.argtypes = (wintypes.LPCWSTR, wintypes.LPCWSTR)
    user32.FindWindowW.restype = wintypes.HWND
    target = next((handle for name in ("EVERYTHING_TASKBAR_NOTIFICATION_(1.5a)",
                                      "EVERYTHING_TASKBAR_NOTIFICATION")
                   if (handle := user32.FindWindowW(name, None))), None)
    if not target:
        raise RuntimeError("Everything IPC is unavailable; start the Everything desktop client")

    class COPYDATA(ctypes.Structure):
        _fields_ = [("dwData", ctypes.c_size_t), ("cbData", wintypes.DWORD), ("lpData", ctypes.c_void_p)]

    class WNDCLASS(ctypes.Structure):
        _fields_ = [("style", wintypes.UINT), ("lpfnWndProc", ctypes.c_void_p),
                    ("cbClsExtra", ctypes.c_int), ("cbWndExtra", ctypes.c_int),
                    ("hInstance", wintypes.HINSTANCE), ("hIcon", wintypes.HICON),
                    ("hCursor", wintypes.HANDLE), ("hbrBackground", wintypes.HANDLE),
                    ("lpszMenuName", wintypes.LPCWSTR), ("lpszClassName", wintypes.LPCWSTR)]

    paths = []
    total = 0
    reply_received = False
    WNDPROC = ctypes.WINFUNCTYPE(ctypes.c_ssize_t, wintypes.HWND, wintypes.UINT,
                                wintypes.WPARAM, wintypes.LPARAM)

    @WNDPROC
    def window_proc(hwnd, message, wparam, lparam):
        nonlocal paths, total, reply_received
        if message == WM_COPYDATA:
            data = COPYDATA.from_address(lparam)
            if data.dwData == QUERY2W and data.cbData >= 20:
                raw = ctypes.string_at(data.lpData, data.cbData)
                total, count, _, flags, _ = struct.unpack_from("<IIIII", raw)
                if flags != REQUEST_FULL_PATH or 20 + 8 * count > len(raw):
                    return 0
                for index in range(count):
                    _, data_offset = struct.unpack_from("<II", raw, 20 + index * 8)
                    if data_offset + 4 > len(raw):
                        return 0
                    length = struct.unpack_from("<I", raw, data_offset)[0]
                    start = data_offset + 4
                    end = start + 2 * length
                    if end + 2 > len(raw):
                        return 0
                    paths.append(Path(raw[start:end].decode("utf-16-le")))
                reply_received = True
                return 1
        return user32.DefWindowProcW(hwnd, message, wparam, lparam)

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.GetModuleHandleW.argtypes = (wintypes.LPCWSTR,)
    kernel32.GetModuleHandleW.restype = wintypes.HMODULE
    class_name = f"image_atlas_everything_{kernel32.GetCurrentProcessId()}"
    instance = kernel32.GetModuleHandleW(None)
    klass = WNDCLASS(lpfnWndProc=ctypes.cast(window_proc, ctypes.c_void_p).value,
                     hInstance=instance, lpszClassName=class_name)
    user32.RegisterClassW.argtypes = (ctypes.POINTER(WNDCLASS),)
    user32.UnregisterClassW.argtypes = (wintypes.LPCWSTR, wintypes.HINSTANCE)
    atom = user32.RegisterClassW(ctypes.byref(klass))
    if not atom:
        raise OSError(ctypes.get_last_error(), "Cannot register Everything reply window")
    user32.CreateWindowExW.restype = wintypes.HWND
    user32.CreateWindowExW.argtypes = (wintypes.DWORD, wintypes.LPCWSTR, wintypes.LPCWSTR,
                                       wintypes.DWORD, ctypes.c_int, ctypes.c_int, ctypes.c_int,
                                       ctypes.c_int, wintypes.HWND, wintypes.HMENU,
                                       wintypes.HINSTANCE, ctypes.c_void_p)
    hwnd = user32.CreateWindowExW(0, class_name, class_name, 0, 0, 0, 0, 0,
                                   None, None, instance, None)
    if not hwnd:
        user32.UnregisterClassW(class_name, instance)
        raise OSError(ctypes.get_last_error(), "Cannot create Everything reply window")
    try:
        # Everything's IPC query2 uses a 32-bit window handle and packed DWORD fields.
        payload = struct.pack("<7I", hwnd & 0xffffffff, QUERY2W, 0, offset,
                              PAGE_SIZE, REQUEST_FULL_PATH, 1) + search.encode("utf-16-le") + b"\0\0"
        buffer = ctypes.create_string_buffer(payload)
        data = COPYDATA(QUERY2W, len(payload), ctypes.addressof(buffer))
        user32.SendMessageTimeoutW.argtypes = (wintypes.HWND, wintypes.UINT, wintypes.WPARAM,
                                                wintypes.LPARAM, wintypes.UINT, wintypes.UINT,
                                                ctypes.POINTER(ctypes.c_size_t))
        outcome = ctypes.c_size_t()
        sent = user32.SendMessageTimeoutW(target, WM_COPYDATA, hwnd,
                                          ctypes.addressof(data), 2, 5000, ctypes.byref(outcome))
        if not sent or not outcome.value:
            raise RuntimeError("Everything did not accept the IPC query")
        message = wintypes.MSG()
        deadline = time.monotonic() + 15
        while not reply_received and time.monotonic() < deadline:
            while user32.PeekMessageW(ctypes.byref(message), hwnd, 0, 0, 1):
                user32.TranslateMessage(ctypes.byref(message))
                user32.DispatchMessageW(ctypes.byref(message))
            if not reply_received:
                time.sleep(0.01)
        if not reply_received:
            raise TimeoutError("Timed out waiting for Everything results")
        return paths, total
    finally:
        user32.DestroyWindow(hwnd)
        user32.UnregisterClassW(class_name, instance)


def iter_images(*, exclude_dirs: tuple[Path, ...] = (), include_system: bool = False):
    """Yield allowed indexed image paths in pages."""
    search = image_query(exclude_dirs=exclude_dirs, include_system=include_system)
    offset = 0
    while True:
        page, total = _query_page(search, offset)
        for path in page:
            if path.suffix.lower() in EXTENSIONS and allowed_image(
                    path, exclude_dirs=exclude_dirs, include_system=include_system):
                yield path
        offset += len(page)
        if not page or offset >= total:
            break


def count_indexed_images(*, exclude_dirs: tuple[Path, ...] = (), include_system: bool = False,
                         progress=None) -> int:
    """Count the same filtered path stream used by mining before decoding begins."""
    count = 0
    for _ in iter_images(exclude_dirs=exclude_dirs, include_system=include_system):
        count += 1
        if progress is not None:
            progress()
    return count
