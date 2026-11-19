"""Check generated local page links and fragments after the static build."""

from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit
import sys


class Page(HTMLParser):
    def __init__(self, path):
        super().__init__()
        self.ids = set()
        self.links = []
        self.feed(path.read_text())

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if "id" in attrs:
            self.ids.add(attrs["id"])
        if tag == "a" and "href" in attrs:
            self.links.append(attrs["href"])


root = Path(__file__).resolve().parent.parent / "dist"
pages = {str(path.relative_to(root)): Page(path) for path in root.rglob("*.html")}
# Built files that aren't pages (/llms.txt, /og.png): a link to one needs the file.
files = {str(path.relative_to(root)) for path in root.rglob("*") if path.is_file()}
if not pages:
    sys.exit("No built pages. Run bun run build first.")

errors = []
checked = 0
for name, page in pages.items():
    for href in page.links:
        link = urlsplit(href)
        if link.scheme or link.netloc:
            continue
        path = unquote(link.path).lstrip("/")
        if link.path and not link.path.endswith("/") and path in files:
            checked += 1
            continue
        target = f"{path}index.html" if link.path else name
        if target not in pages:
            errors.append(f"{name}: missing page {href}")
            continue
        checked += 1
        if link.fragment and unquote(link.fragment) not in pages[target].ids:
            errors.append(f"{name}: missing anchor {href}")

print(f"{len(pages)} pages, {checked} internal links checked, {len(errors)} broken links")
if errors:
    sys.exit("\n".join(errors))
