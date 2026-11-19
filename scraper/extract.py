from dataclasses import dataclass, field
from difflib import SequenceMatcher
from hashlib import sha256
import re
from typing import Protocol

from .storage import PROFILE

MODS = {
    "spiral": ("lucky", "Spiral Lucky Block", ("spiral lucky block", "spiral lucky blocks")),
    "astral": ("lucky", "Astral Lucky Block", ("astral lucky block", "astral lucky blocks")),
    "emerald": ("lucky", "Emerald Lucky Block", ("emerald lucky block", "emerald lucky blocks")),
    "orespawn": ("companion", "OreSpawn", ("orespawn", "ore spawn")),
    "mutant-creatures": ("companion", "Mutant Creatures", ("mutant creatures", "mutant creature")),
}
MOBS = ("Mutant Creeper", "Mutant Zombie", "Mutant Skeleton", "Mutant Enderman",
        "The King", "The Queen", "Mobzilla", "Ender Dragon", "Wither")
MODE = {
    "challenge_games": re.compile(r"challenge games?", re.I),
    "lucky_block_bedwars": re.compile(r"bed[ -]?wars", re.I),
}
MC_VERSION = re.compile(r"\b(?:minecraft(?:\s+version)?|mc)\s*(?:v|[:=])?\s*(1\.\d{1,2}(?:\.\d{1,2})?)\b", re.I)
FORGE_VERSION = re.compile(r"\bforge\s*v?\s*(\d+\.\d+\.\d+\.\d+)\b", re.I)
SIGNALS = {
    "trade": re.compile(r"\b(villagers?|trades?|trading)\b", re.I),
    "center": re.compile(r"\b(middle|cent(?:er|re))\b", re.I),
    "chest": re.compile(r"\bchests?\b", re.I),
}


@dataclass(frozen=True)
class Span:
    seconds: int
    text: str

    @property
    def caption_source(self):
        return f"caption@{self.seconds // 60:02}:{self.seconds % 60:02}"


@dataclass
class Recording:
    video_id: str
    title: str
    url: str
    description: str = ""
    upload_date: str | None = None
    duration: int | None = None
    thumbnail_url: str | None = None
    thumbnail_path: str | None = None
    chapters: list[Span] = field(default_factory=list)
    captions: list[Span] = field(default_factory=list)


class Extractor(Protocol):
    def extract(self, recording: Recording) -> list[dict]:
        """Return source-cited findings conforming to the manifest schema."""
        ...


def candidate(value, confidence, source, span=None, seconds=None):
    result = dict(value=value, confidence=confidence, source=source)
    if span is not None:
        result["source_span"] = span
    if seconds is not None:
        result["timestamp_seconds"] = seconds
    return result


def select(candidates, *, review=False):
    ranked = sorted(candidates, key=lambda entry: -entry["confidence"])
    best = ranked[0]
    status = "needs_review" if review or best["confidence"] < .75 else "auto"
    result = dict(best, status=status)
    if len(ranked) > 1:
        alternatives = {}
        for entry in ranked[1:]:
            key = sha256(repr(sorted(entry.items())).encode()).hexdigest()[:16]
            alternatives.setdefault(key, dict(id=key, **entry))
        result["alternatives"] = list(alternatives.values())
    return result


def defaulted(value):
    return dict(candidate(value, .35, "manual"), status="needs_review", defaulted=True)


def title_key(title):
    return " ".join(re.findall(r"[\w]+", title.casefold()))


def duplicate_title(left, right):
    a, b = title_key(left), title_key(right)
    if re.findall(r"\d+", a) != re.findall(r"\d+", b):
        return False
    return bool(a and b and (a == b or (min(len(a), len(b)) >= 22
                                       and SequenceMatcher(None, a, b).ratio() >= .96)))


def caption_windows(captions):
    for i, caption in enumerate(captions):
        for kind, pattern in SIGNALS.items():
            if pattern.search(caption.text):
                nearby = captions[max(0, i - 1):i + 2]
                text = " ".join(span.text for span in nearby if abs(span.seconds - caption.seconds) <= 12)
                yield kind, caption, text


def mod_matches(text, aliases, fuzzy):
    lower = title_key(text)
    if any(re.search(rf"\b{re.escape(alias)}\b", lower) for alias in aliases):
        return 1.0
    if fuzzy:
        words = lower.split()
        for alias in aliases:
            size = len(alias.split())
            for offset in range(len(words) - size + 1):
                nearby = " ".join(words[offset:offset + size])
                if SequenceMatcher(None, alias, nearby).ratio() >= .88:
                    return .65
    return 0


def extract(recording, plugin: Extractor | None = None):
    mode_candidates = []
    for text, weight in [(recording.title, .95), (recording.description, .85)]:
        for mode, pattern in MODE.items():
            if match := pattern.search(text):
                mode_candidates.append(candidate(mode, weight, "description", match.group()))
    if not mode_candidates:
        return None
    mode = select(mode_candidates)
    primary = [(recording.title, "description", .92, None),
               (recording.description, "description", .94, None)]
    primary.extend((entry.text, "chapter", .88, entry.seconds) for entry in recording.chapters)
    sources = primary + [(entry.text, entry.caption_source, .79, entry.seconds)
                         for entry in recording.captions]
    versions, loader_versions = [], []
    for text, source, weight, seconds in sources:
        for pattern, destination in ((MC_VERSION, versions), (FORGE_VERSION, loader_versions)):
            for match in pattern.finditer(text):
                destination.append(candidate(match[1], weight, source, match.group(), seconds))
    mc = select(versions) if versions else defaulted(PROFILE["mc_version"])
    known_profile = mc["value"] == PROFILE["mc_version"]
    loader_version = select(loader_versions) if loader_versions else defaulted(
        PROFILE["loader_version"] if known_profile else None
    )
    game = {
        "mc_version": mc,
        "loader": {
            "type": select([dict(entry, value="forge") for entry in loader_versions])
            if loader_versions else defaulted(PROFILE["loader"]),
            "version": loader_version,
        },
        "java_major": defaulted(PROFILE["java_major"] if known_profile else None),
    }
    mods = []
    for key, (role, name, aliases) in MODS.items():
        mentions, mod_versions = [], []
        for text, source, weight, seconds in sources:
            score = mod_matches(text, aliases, source.startswith("caption@"))
            if not score and role == "lucky" and seconds is None and text == recording.title:
                if re.search(rf"\b{re.escape(key)}\b", text, re.I):
                    score = .77
            if score:
                mentions.append(candidate(name, min(weight, score), source, text, seconds))
            for alias in aliases:
                pattern = rf"\b{re.escape(alias)}\s+(?:version\s*|v)(\d+(?:\.\d+)+(?:[-\w]+)?)\b"
                for match in re.finditer(pattern, text, re.I):
                    mod_versions.append(candidate(match[1], weight, source, match.group(), seconds))
        if mentions:
            mods.append(dict(id=key, role=role, mod_id=defaulted(None),
                             display_name=select(mentions), aliases=list(aliases),
                             version_range=select(mod_versions) if mod_versions else defaulted(None),
                             optional=False, client_only=False))
    kind = "island" if mode["value"] == "lucky_block_bedwars" else "arena"
    findings = {}
    for label, caption, window in caption_windows(recording.captions):
        key = f"{label}-{caption.seconds}"
        findings.setdefault(key, dict(id=key, **select([
            candidate(window, .65, caption.caption_source, window, caption.seconds)
        ], review=True)))
        if label in {"chest", "trade"}:
            key = f"visual-{label}-{caption.seconds}"
            subject = "Exact chest contents" if label == "chest" else "Exact villager trade"
            findings.setdefault(key, dict(id=key, **select([
                candidate(subject, .2, "visual", seconds=caption.seconds)
            ], review=True)))
    if sum(mod["role"] == "lucky" for mod in mods) > 1:
        findings["multiple-lucky"] = dict(id="multiple-lucky", **select([
            candidate("Verify compatibility of multiple Lucky Block addons.", .9, "manual")
        ], review=True))
    mobs = []
    for name in MOBS:
        mentions = [
            candidate(name, weight, source, match.group(), seconds)
            for text, source, weight, seconds in sources
            if (match := re.search(rf"\b{re.escape(name)}\b", text, re.I))
        ]
        if mentions:
            mobs.append(dict(id=title_key(name).replace(" ", "-"), **select(mentions)))
    if plugin is not None:
        for finding in plugin.extract(recording):
            span = finding.get("source_span")
            source = finding.get("source")
            if source == "description":
                evidence_text = [recording.title, recording.description]
            elif source == "chapter":
                evidence_text = [chapter.text for chapter in recording.chapters]
            elif isinstance(source, str) and source.startswith("caption@"):
                evidence_text = [caption.text for caption in recording.captions if caption.caption_source == source]
            else:
                evidence_text = []
            if not isinstance(span, str) or not span or not any(span in text for text in evidence_text):
                raise ValueError("extractor finding must quote its recorded source span")
            if finding["id"] in findings:
                raise ValueError(f"duplicate finding {finding['id']}")
            findings[finding["id"]] = finding
    return {
        "schema_version": 1, "episode": {
            key: getattr(recording, key) for key in (
                "video_id", "title", "url", "upload_date", "duration",
                "thumbnail_url", "thumbnail_path",
            )
        },
        "mode": mode, "game": game, "mods": mods,
        "map": {"template_id": mode["value"], "overrides": {}},
        "villagers": [], "center": {"kind": kind, "blocks": [], "chests": [], "generators": []},
        "findings": list(findings.values()), "mob_tags": mobs,
    }
