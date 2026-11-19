import json
import logging
import os
from pathlib import Path
import re
import sqlite3
import tempfile
from contextlib import closing

from jsonschema import Draft202012Validator

SCHEMA = Path(__file__).resolve().parent.parent / "schema"
PROFILE = json.loads((SCHEMA / "profiles" / "default.json").read_text(encoding="utf-8"))
MANIFEST = Draft202012Validator(json.loads((SCHEMA / "manifest-v1.schema.json").read_text(encoding="utf-8")))
OVERRIDES = Draft202012Validator(json.loads((SCHEMA / "overrides-v1.schema.json").read_text(encoding="utf-8")))
LOG = logging.getLogger(__name__)


def atomic_write(path, contents):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(dir=path.parent, prefix=".cloud9-")
    try:
        with os.fdopen(fd, "wb") as file:
            file.write(contents)
            file.flush()
            os.fsync(file.fileno())
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def encode(value):
    return (json.dumps(value, indent=2, ensure_ascii=False, allow_nan=False) + "\n").encode("utf-8")


def review_fields(node, path=""):
    if isinstance(node, dict):
        if node.get("status") == "needs_review" and {"value", "source", "confidence"} <= node.keys():
            yield path, node
        for key, value in node.items():
            escaped = key.replace("~", "~0").replace("/", "~1")
            yield from review_fields(value, f"{path}/{escaped}")
    elif isinstance(node, list):
        for index, value in enumerate(node):
            token = f"@{value['id']}" if isinstance(value, dict) and isinstance(value.get("id"), str) else index
            yield from review_fields(value, f"{path}/{token}")


def apply_overrides(document, overrides):
    OVERRIDES.validate(overrides)
    result = json.loads(json.dumps(document))
    for patch in overrides["patches"]:
        cursor = result
        parts = patch["path"][1:].split("/")
        if any(re.search(r"~(?![01])", token) for token in parts):
            raise ValueError(f"invalid JSON Pointer {patch['path']}")
        for part in parts:
            token = part.replace("~1", "/").replace("~0", "~")
            parent = cursor
            if isinstance(parent, list):
                if token.startswith("@"):
                    matches = [i for i, value in enumerate(parent)
                               if isinstance(value, dict) and value.get("id") == token[1:]]
                    if len(matches) != 1:
                        raise ValueError(f"missing or ambiguous target {patch['path']}")
                    token = matches[0]
                elif re.fullmatch(r"0|[1-9][0-9]*", token):
                    token = int(token)
                else:
                    raise ValueError(f"invalid array index {patch['path']}")
            try:
                cursor = parent[token]
            except (KeyError, IndexError, TypeError) as error:
                raise ValueError(f"missing override target {patch['path']}") from error
        if "expected" in patch and cursor != patch["expected"] and cursor != patch["value"]:
            LOG.warning("scrape disagrees with override baseline at %s", patch["path"])
        parent[token] = patch["value"]
    return result


class Catalog:
    def __init__(self, output):
        self.output = Path(output)
        self.output.mkdir(parents=True, exist_ok=True)
        self.database = self.output / "episodes.sqlite"
        with closing(self.connect()) as db:
            db.execute("PRAGMA journal_mode = WAL")
            db.execute("BEGIN IMMEDIATE")
            try:
                version = db.execute("PRAGMA user_version").fetchone()[0]
                for migration in sorted((SCHEMA / "migrations").glob("*.sql")):
                    number = int(migration.stem)
                    if number > version:
                        for statement in migration.read_text(encoding="utf-8").split(";"):
                            if statement.strip():
                                db.execute(statement)
                        db.execute(f"PRAGMA user_version = {number:d}")
                db.commit()
            except Exception:
                db.rollback()
                raise

    def connect(self):
        db = sqlite3.connect(self.database, timeout=30)
        db.execute("PRAGMA foreign_keys = ON")
        return db

    def titles(self):
        with closing(self.connect()) as db:
            return dict(db.execute("SELECT video_id, title FROM episodes"))

    def documents(self):
        with closing(self.connect()) as db:
            return [json.loads(row[0]) for row in db.execute(
                "SELECT manifest_json FROM episodes ORDER BY upload_date DESC, video_id"
            )]

    def write(self, document, overrides_dir):
        MANIFEST.validate(document)
        video_id = document["episode"]["video_id"]
        override_path = Path(overrides_dir) / f"{video_id}.json"
        if override_path.is_file():
            document = apply_overrides(document, json.loads(override_path.read_text(encoding="utf-8")))
        MANIFEST.validate(document)
        episode = document["episode"]
        if episode["video_id"] != video_id:
            raise ValueError("override cannot change the episode identity")
        tags = set()
        for mod in document["mods"]:
            tags.update(tag.casefold() for tag in [mod["id"], mod["display_name"]["value"], *mod["aliases"]])
        tags.update(str(tag).casefold() for tag in document.get("mob_tags", []))
        payload = encode(document)
        with closing(self.connect()) as db, db:
            db.execute(
                """INSERT INTO episodes VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(video_id) DO UPDATE SET title=excluded.title, url=excluded.url,
                   upload_date=excluded.upload_date, duration=excluded.duration,
                   thumbnail_path=excluded.thumbnail_path, mode=excluded.mode,
                   mc_version=excluded.mc_version, review_count=excluded.review_count,
                   manifest_json=excluded.manifest_json""",
                (video_id, episode["title"], episode["url"], episode["upload_date"],
                 episode["duration"], episode["thumbnail_path"], document["mode"]["value"],
                 document["game"]["mc_version"]["value"],
                 sum(1 for _ in review_fields(document)), payload.decode("utf-8")),
            )
            db.execute("DELETE FROM episode_tags WHERE video_id = ?", (video_id,))
            db.executemany("INSERT INTO episode_tags VALUES (?, ?)", [(video_id, tag) for tag in sorted(tags)])
            atomic_write(self.output / "episodes" / f"{video_id}.json", payload)
        return document
