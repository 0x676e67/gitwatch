"""Check links, anchors, assets and translation parity in the built site."""

from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit
import json
import struct

ROOT = Path(__file__).resolve().parent
SITE = ROOT / "_site"


def image_size(path):
    data = path.read_bytes()
    if data[:8] == b"\x89PNG\r\n\x1a\n":
        return struct.unpack(">II", data[16:24])
    if data[:2] == b"\xff\xd8":
        offset = 2
        while offset + 4 < len(data) and data[offset] == 255:
            marker = data[offset + 1]
            offset += 2
            length = int.from_bytes(data[offset:offset + 2], "big")
            if marker in (192, 193, 194):
                height, width = struct.unpack(">HH", data[offset + 3:offset + 7])
                return width, height
            if length < 2:
                break
            offset += length
    raise ValueError(f"Unsupported image: {path.name}")


class Page(HTMLParser):
    def __init__(self, path):
        super().__init__(convert_charrefs=True)
        self.path = path
        self.ids = set()
        self.sections = []
        self.links = []
        self.errors = []
        self.images = []
        self.h1 = 0
        self.lang = None
        self.feed(path.read_text(encoding="utf-8"))

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == "html":
            self.lang = attrs.get("lang")
        if tag == "h1":
            self.h1 += 1
        if "id" in attrs:
            if attrs["id"] in self.ids:
                self.errors.append(f"duplicate ID: {attrs['id']}")
            self.ids.add(attrs["id"])
            if tag == "h2":
                self.sections.append(attrs["id"])
        for key in ("href", "src"):
            if key in attrs:
                self.links.append(attrs[key])
        if tag == "img":
            if "alt" not in attrs:
                self.errors.append("image missing alt text")
            self.images.append(attrs)


def check():
    errors = []
    pages = {path.resolve(): Page(path) for path in SITE.rglob("*.html")}
    manifest = json.loads((ROOT / "pages.json").read_text(encoding="utf-8"))
    if len(pages) != len(manifest) * 2 + 4:
        errors.append("unexpected page count")
    for path, page in pages.items():
        errors.extend(f"{path.relative_to(SITE)}: {error}" for error in page.errors)
        if page.h1 != 1 or page.lang not in ("en", "zh-CN"):
            errors.append(f"{path.name}: expected one h1 and a supported document language")
        for link in page.links:
            url = urlsplit(link)
            if url.scheme or url.netloc:
                if url.scheme not in ("https", "mailto"):
                    errors.append(f"unsupported URL: {link}")
                continue
            value = unquote(url.path)
            if value.startswith("/"):
                target = (SITE / value.lstrip("/")).resolve()
            else:
                target = (path.parent / value).resolve() if value else path
            if not target.is_relative_to(SITE.resolve()) or not target.is_file():
                errors.append(f"{path.relative_to(SITE)}: missing target {link}")
            elif url.fragment and (target not in pages or unquote(url.fragment) not in pages[target].ids):
                errors.append(f"{path.relative_to(SITE)}: missing anchor {link}")
        for img in page.images:
            target = path.parent / img.get("src", "")
            if target.suffix in (".png", ".jpg") and target.is_file():
                width, height = image_size(target)
                if (str(width), str(height)) != (img.get("width"), img.get("height")):
                    errors.append(f"{path.name}: wrong dimensions for {target.name}")
    for entry in manifest:
        en = pages.get((SITE / "en" / f"{entry['slug']}.html").resolve())
        zh = pages.get((SITE / "zh-CN" / f"{entry['slug']}.html").resolve())
        if not en or not zh or en.sections != zh.sections:
            errors.append(f"translation section mismatch: {entry['slug']}")
        for lang, page, other in [("en", en, "zh-CN"), ("zh-CN", zh, "en")]:
            if page and f"../{other}/{entry['slug']}.html" not in page.links:
                errors.append(f"missing language switch: {lang}/{entry['slug']}")
    if errors:
        raise SystemExit("\n".join(errors))
    print(f"Validated {len(pages)} pages: links, anchors, language parity and image dimensions")


if __name__ == "__main__":
    check()
