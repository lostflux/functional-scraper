#!/usr/bin/env python3
"""
Live crawl dashboard.

Tails the crawler's log (the lines `scraper crawl` prints) into JSON and serves
a single-page dashboard that polls it.

  python3 scripts/dashboard.py [--log data/state/crawl.log] [--port 8765]

Then open http://localhost:8765/?budget=5h (budget is optional; it only drives
the ETA).
"""

from __future__ import annotations

import argparse
import json
import re
import threading
from collections import Counter, deque
from datetime import datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlsplit

HERE = Path(__file__).resolve().parent
ANSI = re.compile(r"\x1b\[[0-9;]*m")
TS = re.compile(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?)Z?\s")
PROGRESS = re.compile(
  r"progress: fetched=(\d+) hits=(\d+) errors=(\d+) throttled=(\d+) "
  r"queued=(\d+) active=(\d+) hosts=(\d+) ([\d.]+) pages/s"
)
HIT = re.compile(r"hit #(\d+) \[([^\]]*)\] (\S+) \| (.*)$")
RUN = re.compile(r"run (\S+) \|")
DONE = re.compile(r"done in (\d+)s: (.*)$")
RESUMED = re.compile(r"resumed (\d+) of (\d+)")


def bare_host(url: str) -> str:
  host = (urlsplit(url).hostname or "").lower()
  return host[4:] if host.startswith("www.") else host


class LogState:
  """Incremental parser; `refresh()` reads only what was appended."""

  def __init__(self, path: Path):
    self.path = path
    self.offset = 0
    self.remainder = b""
    self.lock = threading.Lock()
    self.run_id = None
    self.started = None
    self.last_ts = None
    self.finished = None
    self.done_line = None
    self.resumed = None
    self.series = []  # [t_sec, fetched, hits, errors, throttled, queued, active, hosts, rate]
    self.hits_per_min = Counter()
    self.domains = Counter()
    self.years = Counter()
    self.recent = deque(maxlen=40)
    self.hits = 0
    self.first_id = None
    self.last_id = None

  def _ts(self, line: str):
    m = TS.match(line)
    if not m:
      return None
    return datetime.fromisoformat(m.group(1).replace("Z", "")).timestamp()

  def _handle(self, raw: str) -> None:
    line = ANSI.sub("", raw).rstrip()
    ts = self._ts(line)
    if ts is None:
      return
    if self.started is None:
      self.started = ts
    self.last_ts = ts
    t = ts - self.started
    if m := PROGRESS.search(line):
      vals = [int(x) for x in m.groups()[:7]] + [float(m.group(8))]
      self.series.append([round(t, 1)] + vals)
    elif m := HIT.search(line):
      doc_id, date, url, title = m.groups()
      self.hits += 1
      self.first_id = self.first_id or int(doc_id)
      self.last_id = int(doc_id)
      self.hits_per_min[int(t // 60)] += 1
      self.domains[bare_host(url)] += 1
      self.years[date[:4] if date[:1].isdigit() else "unknown"] += 1
      self.recent.append(
        {"t": round(t), "id": int(doc_id), "date": date, "domain": bare_host(url), "url": url, "title": title}
      )
    elif m := RUN.search(line):
      self.run_id = m.group(1)
    elif m := DONE.search(line):
      self.finished = ts
      self.done_line = m.group(2)
    elif m := RESUMED.search(line):
      self.resumed = int(m.group(1))

  def refresh(self) -> None:
    with self.lock:
      if not self.path.exists():
        return
      size = self.path.stat().st_size
      if size < self.offset:
        self.__init__(self.path)
      with open(self.path, "rb") as fh:
        fh.seek(self.offset)
        chunk = fh.read()
        self.offset = fh.tell()
      data = self.remainder + chunk
      lines = data.split(b"\n")
      self.remainder = lines.pop()
      for raw in lines:
        self._handle(raw.decode("utf-8", "replace"))

  def hosts(self) -> list:
    """Per-host rows from the crawler's progress.json, if it is there."""
    path = self.path.resolve().parent / "progress.json"
    for candidate in (self.path.parent / "progress.json", path):
      try:
        data = json.loads(candidate.read_text())
      except (OSError, ValueError):
        continue
      rows = [
        {"host": h, **v, "hit_ratio": (v["hits"] / v["fetched"]) if v.get("fetched") else None}
        for h, v in data.get("hosts", {}).items()
      ]
      rows.sort(key=lambda r: (-(r.get("fetched") or 0), r["host"]))
      return rows
    return []

  def snapshot(self) -> dict:
    self.refresh()
    hosts = self.hosts()
    with self.lock:
      last = self.series[-1] if self.series else None
      minutes = int((self.last_ts - self.started) // 60) + 1 if self.started else 0
      return {
        "run_id": self.run_id,
        "resumed": self.resumed,
        "started": self.started,
        "last_ts": self.last_ts,
        "elapsed": round(self.last_ts - self.started) if self.started else 0,
        "finished": self.finished is not None,
        "done": self.done_line,
        "totals": {
          "fetched": last[1] if last else 0,
          "hits": self.hits,
          "errors": last[3] if last else 0,
          "throttled": last[4] if last else 0,
          "queued": last[5] if last else 0,
          "active": last[6] if last else 0,
          "hosts": last[7] if last else 0,
          "rate": last[8] if last else 0.0,
          "first_id": self.first_id,
          "last_id": self.last_id,
        },
        "series": self.series[-4000:],
        "hits_per_min": [self.hits_per_min.get(i, 0) for i in range(minutes)],
        "domains": self.domains.most_common(15),
        "years": sorted(self.years.items()),
        "recent": list(reversed(self.recent)),
        "hosts": hosts,
      }


def make_handler(state: LogState):
  page_path = HERE / "dashboard.html"

  class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
      pass

    def do_GET(self):
      path = self.path.split("?", 1)[0]
      if path == "/api/progress":
        body = json.dumps(state.snapshot()).encode()
        ctype = "application/json"
      elif path in ("/", "/index.html"):
        body, ctype = page_path.read_bytes(), "text/html; charset=utf-8"
      else:
        self.send_response(404)
        self.end_headers()
        return
      self.send_response(200)
      self.send_header("Content-Type", ctype)
      self.send_header("Content-Length", str(len(body)))
      self.send_header("Cache-Control", "no-store")
      self.end_headers()
      self.wfile.write(body)

  return Handler


def main() -> None:
  ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
  ap.add_argument("--log", type=Path, default=HERE.parent / "data" / "state" / "crawl.log")
  ap.add_argument("--port", type=int, default=8765)
  args = ap.parse_args()
  state = LogState(args.log)
  server = ThreadingHTTPServer(("127.0.0.1", args.port), make_handler(state))
  print(f"dashboard: http://localhost:{args.port}/  (log: {args.log})")
  server.serve_forever()


if __name__ == "__main__":
  main()
