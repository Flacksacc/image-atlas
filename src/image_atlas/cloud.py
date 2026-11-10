"""Read Windows file availability without opening image data or hydrating placeholders."""
import ctypes
import sys
from ctypes import wintypes
from pathlib import Path

OFFLINE = 0x00001000
RECALL_ON_OPEN = 0x00040000
RECALL_ON_DATA_ACCESS = 0x00400000
REMOTE_CONTENT = OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS


class _FindData(ctypes.Structure):
    _fields_ = [("attributes", wintypes.DWORD),
                ("created", wintypes.FILETIME), ("accessed", wintypes.FILETIME),
                ("modified", wintypes.FILETIME), ("size_high", wintypes.DWORD),
                ("size_low", wintypes.DWORD), ("reserved0", wintypes.DWORD),
                ("reserved1", wintypes.DWORD), ("name", wintypes.WCHAR * 260),
                ("alternate_name", wintypes.WCHAR * 14)]


def _win_path(path: Path) -> str:
    name = str(path)
    if len(name) < 240 or name.startswith("\\\\?\\"):
        return name
    return "\\\\?\\UNC\\" + name[2:] if name.startswith("\\\\") else "\\\\?\\" + name


def file_attributes(path: Path) -> int | None:
    """Directory metadata includes RECALL_ON_OPEN, which GetFileAttributes can omit."""
    if sys.platform != "win32":
        return None
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.FindFirstFileW.argtypes = (wintypes.LPCWSTR, ctypes.POINTER(_FindData))
    kernel.FindFirstFileW.restype = wintypes.HANDLE
    kernel.FindClose.argtypes = (wintypes.HANDLE,)
    data = _FindData()
    handle = kernel.FindFirstFileW(_win_path(path), ctypes.byref(data))
    if handle == ctypes.c_void_p(-1).value:
        return None
    try:
        return data.attributes
    finally:
        kernel.FindClose(handle)


def unavailable_cloud_file(path: Path) -> bool:
    """Skip files marked unavailable, including on Google Drive's virtual volume.

    Volume type and pinning policy do not establish whether this file's bytes
    are local. Recheck availability on every call so hydration/eviction changes
    are respected between discovery, scanning, and analysis.
    """
    if sys.platform != "win32":
        return False
    attributes = file_attributes(path)
    return attributes is not None and bool(attributes & REMOTE_CONTENT)
