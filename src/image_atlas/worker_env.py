"""Choose a compatible model worker without duplicating large GPU dependencies."""
import importlib.util
import os
import sys
from pathlib import Path

SHARED_PYTHON = Path("D:/CUIN/.venv/Scripts/python.exe")


def model_python(env_name: str, packages: tuple[str, ...]) -> str | None:
    """Honor an explicit worker, then local packages, then this machine's shared CUDA env."""
    explicit = os.environ.get(env_name)
    if explicit:
        return explicit
    if all(importlib.util.find_spec(package) is not None for package in packages):
        return None
    if sys.platform == "win32" and SHARED_PYTHON.is_file():
        return str(SHARED_PYTHON)
    return None
