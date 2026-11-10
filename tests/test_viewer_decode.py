from PIL import Image
from image_atlas.viewer_decode import pixels_from_source


def test_face_crop_scales_original_coordinates_to_raw_preview():
    # RAW previews can have smaller dimensions than the stored sensor-space box.
    source = Image.new('RGB', (150, 100), 'red')
    source.paste('blue', (75, 0, 150, 100))
    with pixels_from_source(source, 300, 200, 120, (150, 0, 150, 200)) as face:
        assert face.size == (75, 100)
        assert face.getpixel((20, 20))[:3] == (0, 0, 255)


def test_face_crop_maps_exif_rotated_coordinates():
    source = Image.new('RGB', (200, 100), 'red')
    source.paste('blue', (100, 0, 200, 100))
    source.getexif()[274] = 6
    with pixels_from_source(source, 200, 100, 120, (0, 100, 100, 100)) as face:
        assert face.size == (100, 100)
        assert face.getpixel((50, 50))[:3] == (0, 0, 255)
