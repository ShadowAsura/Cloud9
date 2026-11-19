import argparse
from importlib import import_module
import json
import logging
from pathlib import Path
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

from jsonschema.exceptions import ValidationError

from .collect import YouTube, recorded
from .extract import duplicate_title, extract, extract_index
from .storage import MANIFEST, Catalog, apply_overrides, check_ids, community_patch, review_fields

LOG = logging.getLogger("cloud9")


def review_url(url, entry):
    seconds = entry.get("timestamp_seconds")
    if seconds is None and entry["source"].startswith("caption@"):
        minutes, seconds = map(int, entry["source"][8:].split(":"))
        seconds += minutes * 60
    if seconds is None:
        return url
    parts = urlsplit(url)
    query = [(key, value) for key, value in parse_qsl(parts.query) if key != "t"]
    return urlunsplit(parts._replace(query=urlencode([*query, ("t", f"{seconds}s")])))


def run(args):
    if args.command == "check-community":
        files = sorted(args.community.glob("*.json"))
        for path in files:
            patch = community_patch(path)
            base = args.community / "episodes" / path.name
            if base.is_file():
                document = json.loads(base.read_text(encoding="utf-8"))
                MANIFEST.validate(document)
                if document["episode"]["video_id"] != path.stem:
                    raise ValueError(f"community manifest identity does not match {path.name}")
                reviewed = apply_overrides(document, patch)
                MANIFEST.validate(reviewed)
                check_ids(reviewed)
        LOG.info("checked %d community patch(es)", len(files))
        return 0
    catalog = Catalog(args.output)
    if args.command == "sync":
        catalog.sync()
        return 0
    if args.command == "review":
        for document in catalog.documents():
            episode = document["episode"]
            if args.video_id and episode["video_id"] != args.video_id:
                continue
            for path, entry in review_fields(document):
                print(f"{episode['video_id']} {path} {entry['value']!r} {review_url(episode['url'], entry)}")
        return 0
    if args.command == "seed":
        for path in sorted(args.directory.glob("*.json")):
            catalog.write(json.loads(path.read_text(encoding="utf-8")), args.overrides, community_dir=args.community)
        return 0
    if args.command == "recorded":
        recordings = (recorded(json.loads(path.read_text(encoding="utf-8"))) for path in args.files)
    else:
        collector = YouTube(args.cache, interval=args.interval)
        if args.video:
            recordings = (collector.fetch(video_id, captions=False) for video_id in args.video)
        else:
            recordings = collector.index(channel=args.channel, playlist=args.playlist,
                                         keyword=args.keyword, limit=args.limit, uploader=args.uploader)
    plugin = None
    if args.command == "recorded" and args.extractor:
        module, separator, factory = args.extractor.partition(":")
        if not separator:
            raise ValueError("extractor must be module:factory")
        plugin = getattr(import_module(module), factory)()
    titles = catalog.titles()
    count = 0
    for recording in recordings:
        document = extract_index(recording) if args.command == "scrape" else extract(recording, plugin)
        if document is None:
            LOG.info("skipping unclassified video %s", recording.video_id)
            continue
        if "compilation" in recording.title.casefold().split():
            LOG.info("skipping compilation %s", recording.video_id)
            continue
        if recording.video_id not in titles and any(
            duplicate_title(recording.title, title) for title in titles.values()
        ):
            LOG.info("skipping probable re-upload %s", recording.video_id)
            continue
        if args.command == "scrape" and args.thumbnails:
            collector.cache_thumbnail(recording)
            document["episode"]["thumbnail_path"] = recording.thumbnail_path
        catalog.write(document, args.overrides, community_dir=args.community)
        titles[recording.video_id] = recording.title
        count += 1
    LOG.info("indexed %d episode(s)", count)
    return 0


def main():
    parser = argparse.ArgumentParser(prog="python -m scraper")
    parser.add_argument("--output", type=Path, default=Path("data"))
    parser.add_argument("--overrides", type=Path, default=Path("overrides"))
    parser.add_argument("--community", type=Path, default=Path("community"))
    commands = parser.add_subparsers(dest="command", required=True)
    scrape = commands.add_parser("scrape")
    source = scrape.add_mutually_exclusive_group(required=True)
    source.add_argument("--channel")
    source.add_argument("--playlist")
    source.add_argument("--keyword")
    source.add_argument("--video", action="append")
    scrape.add_argument("--cache", type=Path, default=Path(".cache"))
    scrape.add_argument("--interval", type=float, default=2)
    scrape.add_argument("--limit", type=int, default=50)
    scrape.add_argument("--thumbnails", action="store_true")
    scrape.add_argument("--uploader", help="Filter discovery results by the exact channel display name")
    offline = commands.add_parser("recorded")
    offline.add_argument("files", type=Path, nargs="+")
    offline.add_argument("--extractor")
    commands.add_parser("sync")
    commands.add_parser("check-community")
    commands.add_parser("seed").add_argument("directory", type=Path)
    commands.add_parser("review").add_argument("--video-id")
    logging.basicConfig(level=logging.INFO, format="%(levelname)s %(message)s")
    try:
        return run(parser.parse_args())
    except (OSError, RuntimeError, ValidationError, ValueError, ImportError, AttributeError) as error:
        LOG.error("%s", error)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
