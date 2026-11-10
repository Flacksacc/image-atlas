"""Everything source exclusions use directory boundaries, not filename substrings."""
from pathlib import Path

from image_atlas.everything import allowed_image, image_query


def test_default_exclusions_keep_personal_images(monkeypatch):
    monkeypatch.setenv("SystemRoot", r"C:\Windows")
    monkeypatch.setenv("ProgramFiles", r"C:\Program Files")
    blocked = (
        r"C:\Windows\Web\Wallpaper\flower.jpg",
        r"C:\Program Files\Vendor\splash.png",
        r"D:\Program Files\Other App\icon.png",
        r"D:\WindowsApps\Game\logo.png",
        r"C:\Users\War\AppData\Local\Cache\picture.jpg",
        r"D:\Projects\site\node_modules\package\image.png",
        r"E:\$Recycle.Bin\deleted.jpg",
    )
    assert all(not allowed_image(Path(path)) for path in blocked)
    assert allowed_image(Path(r"C:\Users\War\Pictures\flower.jpg"))
    assert allowed_image(Path(r"D:\Photos\Program Files\album.jpg"))
    assert allowed_image(Path(r"D:\Photos\node_modules.png"))


def test_custom_exclusions_are_case_insensitive_and_boundary_aware():
    excluded = (Path(r"D:\Photos\Private"),)
    assert not allowed_image(Path(r"d:\photos\PRIVATE\portrait.jpg"), exclude_dirs=excluded,
                             include_system=True)
    assert allowed_image(Path(r"D:\Photos\Private2\portrait.jpg"), exclude_dirs=excluded,
                         include_system=True)
    assert allowed_image(Path(r"C:\Windows\Web\Wallpaper\flower.jpg"), include_system=True)


def test_everything_query_pushes_directory_exclusions(monkeypatch):
    monkeypatch.setenv("SystemRoot", r"C:\Windows")
    monkeypatch.setenv("APPDATA", r"C:\Users\War\AppData\Roaming")
    query = image_query(exclude_dirs=(Path(r"D:\Photos\Private"),))
    assert '!path:"c:\\windows\\"' in query
    assert '!path:"c:\\users\\war\\appdata\\"' in query
    assert '!path:"d:\\photos\\private\\"' in query
    assert "!path:" not in image_query(include_system=True)
