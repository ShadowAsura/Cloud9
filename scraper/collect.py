from dataclasses import asdict
from html import unescape
import json
import hashlib
import logging
from pathlib import Path
import re
import subprocess
import tempfile
import time
from urllib.error import URLError
from urllib.parse import urlsplit
from urllib.request import HTTPRedirectHandler, build_opener

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


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, file, code, message, headers, url):
        return None


def thumbnail_url(url, video_id):
    parsed = urlsplit(url)
    return (parsed.scheme == "https"
            and parsed.netloc in {"i.ytimg.com", "i1.ytimg.com", "i2.ytimg.com", "i3.ytimg.com", "i4.ytimg.com"}
            and re.fullmatch(rf"/vi(?:_webp)?/{re.escape(video_id)}/[A-Za-z0-9_.-]+", parsed.path))


def listing_record(entry):
    video_id = entry["id"]
    thumbs = [value["url"] for value in entry.get("thumbnails") or []
              if thumbnail_url(value.get("url") or "", video_id)]
    duration = entry.get("duration")
    date = entry.get("upload_date") or ""
    if re.fullmatch(r"\d{8}", date):
        date = f"{date[:4]}-{date[4:6]}-{date[6:]}"
    return Recording(video_id, entry["title"], watch_url(video_id),
                     description=entry.get("description") or "", upload_date=date or None,
                     duration=int(duration) if duration is not None else None,
                     thumbnail_url=thumbs[-1] if thumbs else None)


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

    def _discovery_target(self, *, channel=None, playlist=None, keyword=None, limit=50):
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
        return target

    def index(self, *, channel=None, playlist=None, keyword=None, limit=50, uploader=None):
        """Collect public listing metadata without opening player pages."""
        target = self._discovery_target(channel=channel, playlist=playlist, keyword=keyword, limit=limit)
        key = hashlib.sha256(f"{limit}\n{target}\n{uploader or ''}".encode()).hexdigest()
        path = self.cache / "episode-index" / f"{key}.json"
        if self._fresh(path):
            return [recorded(entry) for entry in json.loads(path.read_text(encoding="utf-8"))]
        result = self._run(["--flat-playlist", "--playlist-end", str(limit), "--dump-single-json"], target)
        entries = {}
        for entry in result.get("entries") or []:
            if not entry or not ID.fullmatch(entry.get("id") or "") or not entry.get("title"):
                continue
            channel_name = entry.get("channel") or entry.get("uploader") or ""
            if uploader is not None and channel_name.casefold() != uploader.casefold():
                continue
            entries[entry["id"]] = listing_record(entry)
        atomic_write(path, encode([asdict(entry) for entry in entries.values()]))
        return list(entries.values())

    def discover(self, *, channel=None, playlist=None, keyword=None, limit=50):
        return [entry.video_id for entry in self.index(channel=channel, playlist=playlist, keyword=keyword, limit=limit)]

    def cache_thumbnail(self, entry):
        url = entry.thumbnail_url
        if not url or not thumbnail_url(url, entry.video_id):
            return
        for extension in IMAGE_EXTS:
            cached = self.cache / "thumbnails" / (entry.video_id + extension)
            if cached.is_file():
                entry.thumbnail_path = str(cached)
                return
        time.sleep(max(0, self.interval - (time.monotonic() - self.last)))
        try:
            # The image URL came from yt-dlp's public listing. Redirects are
            # disabled so only that YouTube thumbnail resource can be read.
            with build_opener(NoRedirect).open(url, timeout=12) as response:
                extension = {"image/jpeg": ".jpg", "image/png": ".png", "image/webp": ".webp"}.get(
                    response.headers.get_content_type()
                )
                if extension is None:
                    return
                content = response.read(2 * 1024 * 1024 + 1)
            if len(content) > 2 * 1024 * 1024:
                return
            output = self.cache / "thumbnails" / (entry.video_id + extension)
            atomic_write(output, content)
            entry.thumbnail_path = str(output)
            self.prune_thumbnails(keep=output)
        except (OSError, URLError):
            logging.getLogger(__name__).info("thumbnail unavailable for %s", entry.video_id)
        finally:
            self.last = time.monotonic()

    def fetch(self, video_id, *, thumbnails=False, captions=True):
        target = watch_url(video_id)
        cache_root = ("with-thumbnails" if thumbnails else "recordings") if captions else "metadata"
        cache = self.cache / cache_root / f"{video_id}.json"
        if self._fresh(cache):
            recording = recorded(json.loads(cache.read_text(encoding="utf-8")))
            if not thumbnails or not recording.thumbnail_path or Path(recording.thumbnail_path).is_file():
                return recording
        with tempfile.TemporaryDirectory(prefix="cloud9-meta-") as staging:
            arguments = ["--no-playlist", "--no-simulate", "--dump-single-json",
                         "-o", str(Path(staging) / "episode.%(ext)s")]
            if captions:
                arguments.extend(["--write-subs", "--write-auto-subs", "--sub-langs", "en(-.*)?",
                                  "--sub-format", "vtt", "--sleep-subtitles", str(self.interval)])
            if thumbnails and captions:
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
