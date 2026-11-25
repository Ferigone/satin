from concurrent.futures import ThreadPoolExecutor
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urlparse
from urllib.request import Request, urlopen
from time import sleep

def get(url):
    for attempt in range(3):
        try:
            with urlopen(Request(url, headers={"User-Agent": "Mozilla/5.0 Satin/0.1.0-alpha.35 (independent Rust engine; development alpha)"}), timeout=35) as r:
                return r.read(14 * 1024 * 1024)
        except Exception:
            if attempt == 2: raise
            sleep(1+attempt)

class Scripts(HTMLParser):
    def __init__(self):
        super().__init__(); self.sources=[]
    def handle_starttag(self, tag, attrs):
        a=dict(attrs)
        if tag=="script" and a.get("src") and "nomodule" not in a:
            self.sources.append(a["src"])

root=Path("/tmp/satin-qa-assets")
root.mkdir(exist_ok=True)
page=get("https://www.twitch.tv/ewroon")
Path("/tmp/satin-qa-html").write_bytes(page)
p=Scripts();p.feed(page.decode())
def fetch(item):
    i,url=item
    parsed=urlparse(url)
    if parsed.scheme!="https" or parsed.hostname!="assets.twitch.tv":return None
    try:
        data=get(url)
        path=root/f"{i:03}.js"
        path.write_bytes(data)
        return f"{url}\t{path}\n"
    except Exception as exc:
        print("failed",i,type(exc).__name__)
        return None
with ThreadPoolExecutor(max_workers=5) as pool:
    rows=[r for r in pool.map(fetch,enumerate(p.sources)) if r]
(root/"manifest").write_text("".join(rows))
print("captured external assets",len(rows),"HTML size",len(page))
