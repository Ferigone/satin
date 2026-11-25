from hashlib import sha256
from html import unescape
from html.parser import HTMLParser
from pathlib import Path
import re
from urllib.parse import urljoin, urlsplit
from urllib.request import Request, urlopen

headers = {"User-Agent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.0.0 Safari/537.36", "Accept-Language": "en-US,en;q=0.9"}

def fetch(url):
    with urlopen(Request(url, headers=headers), timeout=40) as response:
        return response.read(32 * 1024 * 1024)

class ScriptTags(HTMLParser):
    def __init__(self):
        super().__init__()
        self.urls = []

    def handle_starttag(self, tag, attrs):
        if tag == "script":
            value = dict(attrs).get("src")
            if value:
                self.urls.append(value)

urls = [
    "https://www.youtube.com/results?search_query=Me+at+the+zoo",
    "https://www.youtube.com/watch?v=jNQXAC9IVRw",
    "https://www.youtube.com/",
]
for page in urls:
    try:
        raw = fetch(page)
        html = raw.decode("utf8", "replace")
        tags = ScriptTags()
        tags.feed(html)
        clean = unescape(html).replace(r"\/", "/").replace(r"\u0026", "&")
        more = re.findall(r"""(?:https?://[^/"'<> ]+)?/s/desktop/[^"'<>\\\s]+\.js""", clean)
        all_scripts = list(dict.fromkeys(urljoin(page, u) for u in tags.urls + more))
        for script in all_scripts:
            parts = urlsplit(script)
            if parts.hostname not in {"www.youtube.com", "www.gstatic.com", "www.youtube-nocookie.com"}:
                continue
            if not any(part in parts.path for part in ("/jsbin/desktop_polymer", "/jsbin/desktop_kevlar", "ytmainappweb.kevlar_base.")):
                continue
            data = fetch(script)
            print("Script", script, "bytes", len(data), "sha256", sha256(data).hexdigest(), flush=True)
            if len(data) >= 5_000_000 and not data.startswith(b"<"):
                Path("/tmp/satin-desktop.js").write_bytes(data)
                raise SystemExit(0)
        print("Page", page, "bytes", len(raw), "scripts", [urlsplit(s).path[:160] for s in all_scripts][:30], flush=True)
    except OSError as e:
        print("Request failed", page, e, flush=True)
raise SystemExit("The runner did not receive a public YouTube desktop application script")
