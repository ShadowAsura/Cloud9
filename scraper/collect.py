from dataclasses import asdict
from html import unescape
import json
from pathlib import Path
import re
import subprocess
import tempfile
import time
from urllib.parse import urlsplit

from .extract import Recording, Span
from .storage import atomic_write, encode

ID = re.compile(r"[A-Za-z0-9_-]{11}")
TIMER = re.compile(r"^(?:(\d+):)?(\d{2}):(\d{2})\.\d{3} --> ")
YT_HOSTS = {"youtube.com", "www.youtube.com", "m.youtube.com"}
THUMBNAIL_BUDGET = 256 * 1024 * 1024
IMAGE_EXTS = {".jpg", ".jpeg", ".png", ".webp"}


def watch_url(video_id):
    if not ID.fullmatch(video_id):
        raise ValueError(f"invalid YouTube video id {video_id!r}")
    return f"https://www.youtube.com/watch?v={video_id}"


def parse_vtt(text):
    spans = []
    seconds = None
    for line in text.splitlines():
        if match := TIMER.match(line):
            hour, minute, second = match.groups()
            seconds = int(hour or 0) * 3600 + int(minute) * 60 + int(second)
        elif not line.strip():
            seconds = None
        elif seconds is not None:
            value = unescape(re.sub(r"<[^>]*>", "", line)).strip()
            if value and (not spans or spans[-1].text != value):
                spans.append(Span(seconds, value))
    return spans


def recorded(data):
    return Recording(**{**data, "chapters": [Span(**span) for span in data.get("chapters", [])],
                        "captions": [Span(**span) for span in data.get("captions", [])]})


class YouTube:
    def __init__(self, cache, *, interval=2.0, ttl=86400, executable="yt-dlp"):
        self.cache = Path(cache).resolve()
        self.interval = max(1.0, interval)
        self.ttl = ttl
        self.executable = executable
        self.last = 0.0

    def prune_thumbnails(self, keep=None):
        root = self.cache / "thumbnails"
        images = []
        for path in root.glob("*"):
            if path.suffix.lower() in IMAGE_EXTS and path.is_file() and not path.is_symlink():
                try:
                    info = path.stat()
                except FileNotFoundError:
                    continue
                images.append((info.st_mtime, info.st_size, path))
        size = sum(length for _, length, _ in images)
        for _, length, path in sorted(images):
            if size <= THUMBNAIL_BUDGET:
                break
            if path != keep:
                path.unlink(missing_ok=True)
                size -= length

    def _run(self, arguments, target):
        time.sleep(max(0, self.interval - (time.monotonic() - self.last)))
        try:
            result = subprocess.run(
                [self.executable, "--ignore-config", "--no-plugin-dirs", "--skip-download",
                 "--no-progress", "--socket-timeout", "20", "--retries", "2",
                 *arguments, "--", target],
                capture_output=True, encoding="utf-8", errors="replace", timeout=180, check=False,
            )
        except subprocess.TimeoutExpired as error:
            raise RuntimeError("yt-dlp metadata request timed out") from error
        finally:
            self.last = time.monotonic()
        if result.returncode:
            raise RuntimeError(result.stderr.strip() or f"yt-dlp exited with {result.returncode}")
        return json.loads(result.stdout)

    def _fresh(self, path):
        return path.is_file() and time.time() - path.stat().st_mtime < self.ttl

    def discover(self, *, channel=None, playlist=None, keyword=None, limit=50):
        import hashlib

        if not 1 <= limit <= 500:
            raise ValueError("discovery limit must be between 1 and 500")
        selected = [value for value in (channel, playlist, keyword) if value]
        if len(selected) != 1:
            raise ValueError("select one channel, playlist, or keyword")
        if keyword:
            target = f"ytsearch{limit}:{keyword}"
        else:
            parsed = urlsplit(selected[0])
            if parsed.scheme != "https" or parsed.hostname not in YT_HOSTS or parsed.netloc != parsed.hostname:
                raise ValueError("use an HTTPS YouTube channel or playlist URL")
            allowed_channel = re.fullmatch(r"/(?:@[\w.-]+|channel/[\w-]+|c/[\w.-]+|user/[\w.-]+)(?:/videos)?/?", parsed.path)
            if channel and not allowed_channel or playlist and parsed.path != "/playlist":
                raise ValueError("expected a YouTube channel or playlist")
            target = (selected[0].rstrip("/") if playlist or parsed.path.rstrip("/").endswith("/videos")
                      else selected[0].rstrip("/") + "/videos")
        key = hashlib.sha256(f"{limit}\n{target}".encode()).hexdigest()
        cached = self.cache / "discovery" / (key + ".json")
        if self._fresh(cached):
            return json.loads(cached.read_text(encoding="utf-8"))
        result = self._run(["--flat-playlist", "--playlist-end", str(limit), "--dump-single-json"], target)
        ids = list(dict.fromkeys(entry["id"] for entry in result.get("entries") or []
                                 if entry and ID.fullmatch(entry.get("id") or "")))
        atomic_write(cached, encode(ids))
        return ids

    def fetch(self, video_id, *, thumbnails=False):
        target = watch_url(video_id)
        cache = self.cache / ("with-thumbnails" if thumbnails else "recordings") / f"{video_id}.json"
        if self._fresh(cache):
            recording = recorded(json.loads(cache.read_text(encoding="utf-8")))
            if not thumbnails or not recording.thumbnail_path or Path(recording.thumbnail_path).is_file():
                return recording
        with tempfile.TemporaryDirectory(prefix="cloud9-meta-") as staging:
            arguments = ["--no-playlist", "--no-simulate", "--dump-single-json",
                         "--write-subs", "--write-auto-subs", "--sub-langs", "en(-.*)?",
                         "--sub-format", "vtt", "--sleep-subtitles", str(self.interval),
                         "-o", str(Path(staging) / "episode.%(ext)s")]
            if thumbnails:
                arguments.append("--write-thumbnail")
            metadata = self._run(arguments, target)
            candidates = sorted(Path(staging).glob("episode.*.vtt"),
                                key=lambda path: (path.name != "episode.en.vtt", path.name))
            captions = parse_vtt(candidates[0].read_text(encoding="utf-8-sig")) if candidates else []
            thumbnail = None
            for path in Path(staging).glob("episode.*"):
                if path.suffix.lower() in IMAGE_EXTS:
                    output = self.cache / "thumbnails" / (video_id + path.suffix.lower())
                    if path.stat().st_size <= 8 * 1024 * 1024:
                        atomic_write(output, path.read_bytes())
                        thumbnail = str(output)
                        self.prune_thumbnails(keep=output)
                    break
            date = metadata.get("upload_date") or ""
            if re.fullmatch(r"\d{8}", date):
                date = f"{date[:4]}-{date[4:6]}-{date[6:]}"
            duration = metadata.get("duration")
            recording = Recording(
                video_id=video_id, title=metadata["title"], url=target,
                description=metadata.get("description") or "", upload_date=date or None,
                duration=int(duration) if duration is not None else None,
                thumbnail_url=metadata.get("thumbnail"), thumbnail_path=thumbnail,
                chapters=[Span(int(chapter["start_time"]), chapter.get("title") or "")
                          for chapter in metadata.get("chapters") or []], captions=captions,
            )
            atomic_write(cache, encode(asdict(recording)))
            return recording
