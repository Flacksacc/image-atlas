"""One-shot isolated Photoshop/PDFium decoder. Never modifies source documents."""
import json
import sys
from contextlib import closing
from pathlib import Path

from PIL import Image

from .documents import PREVIEW_EDGE, basic_metadata


def save_metadata(output, info):
    temporary = output / 'metadata.tmp'
    temporary.write_text(json.dumps(info), encoding='utf-8')
    temporary.replace(output / 'metadata.json')


def decode(path, output, page_number=0):
    info = basic_metadata(path)
    image = None
    try:
        if path.suffix.lower() in {'.psd', '.psb'}:
            from psd_tools import PSDImage
            from psd_tools.constants import Resource
            psd = PSDImage.open(path)
            info.update(width=psd.width, height=psd.height, bit_depth=psd.depth,
                        color_mode=psd.color_mode.name, channels=psd.channels,
                        layer_count=sum(1 for _ in psd.descendants()),
                        layers=[{'name': layer.name, 'kind': layer.kind, 'visible': layer.visible}
                                for _, layer in zip(range(2000), psd.descendants())])
            xmp = psd.image_resources.get_data(Resource.XMP_METADATA)
            if xmp:
                info['xmp'] = xmp.decode('utf-8', errors='replace') if isinstance(xmp, bytes) else str(xmp)
            save_metadata(output, info)
            if psd.width * psd.height > 40_000_000:
                image = psd.thumbnail()
                info['preview_source'] = 'embedded thumbnail (large canvas)'
                if image is None:
                    raise ValueError('Photoshop canvas exceeds the 40-megapixel preview limit and has no embedded thumbnail; metadata retained')
            else:
                info['preview_source'] = 'saved composite' if psd.has_preview() else 'layer composite (effects may differ)'
                image = psd.composite()
            if image is None:
                raise ValueError('Photoshop document has no renderable composite')
        else:
            import pypdfium2 as pdfium
            with pdfium.PdfDocument(path) as pdf:
                info.update(pdf_compatible=True, page_count=len(pdf), pdf_metadata=pdf.get_metadata_dict())
                if not 0 <= page_number < len(pdf):
                    raise ValueError('Requested artboard is outside this document')
                info['artboards'] = [{'index': i, 'width_points': pdf.get_page_size(i)[0],
                                      'height_points': pdf.get_page_size(i)[1]}
                                     for i in range(min(len(pdf), 2000))]
                with closing(pdf[page_number]) as page:
                    width, height = page.get_size()
                    info.update(width=round(width), height=round(height), dimension_unit='points',
                                preview_page=page_number, preview_source='PDF-compatible artwork')
                    save_metadata(output, info)
                    with closing(page.render(scale=min(2., PREVIEW_EDGE / max(width, height)))) as bitmap:
                        image = bitmap.to_pil().copy()
        image.thumbnail((PREVIEW_EDGE, PREVIEW_EDGE), Image.Resampling.LANCZOS)
        with image.convert('RGBA' if 'A' in image.getbands() else 'RGB') as raster:
            raster.save(output / 'preview.png')
        info['preview_available'] = True
    except Exception as exc:  # noqa: BLE001 - preserve document metadata across third-party decoder failures
        info.update(preview_available=False, preview_reason=str(exc)[:2000])
    finally:
        if image is not None:
            image.close()
    save_metadata(output, info)


if __name__ == '__main__':
    decode(Path(sys.argv[1]), Path(sys.argv[2]), int(sys.argv[3]))
