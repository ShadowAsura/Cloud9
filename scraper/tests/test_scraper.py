import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from scraper.__main__ import review_url
from scraper.collect import YouTube, parse_vtt, recorded
from scraper.extract import duplicate_title, extract, mod_matches
from scraper.storage import Catalog, MANIFEST, review_fields

FIXTURES = Path(__file__).parent / "fixtures"


def load(name):
    return recorded(json.loads((FIXTURES / name).read_text(encoding="utf-8")))


class ExtractionTest(unittest.TestCase):
    def test_caption_windows_and_conflicting_versions(self):
        manifest = extract(load("with-captions.json"))
        MANIFEST.validate(manifest)
        self.assertEqual(manifest["mode"]["value"], "challenge_games")
        version = manifest["game"]["mc_version"]
        self.assertEqual(version["value"], "1.8.9")
        self.assertEqual(version["alternatives"][0]["value"], "1.7.10")
        self.assertEqual([mod["id"] for mod in manifest["mods"]], ["spiral", "orespawn"])
        self.assertEqual(manifest["mods"][0]["version_range"]["value"], "1.2")
        findings = dict(review_fields(manifest))
        self.assertIn("/findings/@visual-chest-65", findings)
        self.assertTrue(review_url(manifest["episode"]["url"], findings["/findings/@trade-60"]).endswith("&t=60s"))

    def test_no_english_captions_uses_profile_defaults(self):
        manifest = extract(load("no-captions.json"))
        MANIFEST.validate(manifest)
        self.assertEqual(manifest["mode"]["value"], "lucky_block_bedwars")
        self.assertTrue(manifest["game"]["mc_version"]["defaulted"])
        self.assertEqual(manifest["game"]["java_major"]["value"], 8)

    def test_rescrape_preserves_patch_and_logs_conflict(self):
        manifest = extract(load("with-captions.json"))
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            overrides = root / "overrides"
            overrides.mkdir()
            path = overrides / "recorded-01.json"
            patch_data = json.dumps({"schema_version": 1, "patches": [{
                "path": "/mods/@spiral/mod_id",
                "expected": {"value": "old"},
                "value": {"value": "local_id", "confidence": 1, "source": "manual", "status": "overridden"},
            }]})
            path.write_text(patch_data, encoding="utf-8")
            catalog = Catalog(root / "index")
            catalog.write(manifest, root / "no-overrides")
            manifest["mods"].reverse()
            with self.assertLogs("scraper.storage", level="WARNING") as logs:
                updated = catalog.write(manifest, overrides)
            self.assertIn("/mods/@spiral/mod_id", logs.output[0])
            self.assertEqual(path.read_text(encoding="utf-8"), patch_data)
            self.assertEqual(updated["mods"][1]["mod_id"]["value"], "local_id")
            exported = catalog.output / "episodes" / "recorded-01.json"
            self.assertEqual(json.loads(exported.read_text(encoding="utf-8")), catalog.documents()[0])
            exported.unlink()
            catalog.sync()
            self.assertEqual(json.loads(exported.read_text(encoding="utf-8")), updated)

    def test_metadata_only_command(self):
        with tempfile.TemporaryDirectory() as temporary, patch("scraper.collect.subprocess.run") as run:
            run.return_value.returncode = 0
            run.return_value.stdout = '{"title":"Spiral Challenge Games"}'
            collector = YouTube(temporary)
            collector.fetch("abcdefghijk")
            collector.fetch("abcdefghijk")
            run.assert_called_once()
            command = run.call_args.args[0]
            self.assertIn("--skip-download", command)
            self.assertIn("--ignore-config", command)
            self.assertNotIn("--write-thumbnail", command)

    def test_noisy_transcript_and_deduplication(self):
        captions = parse_vtt("WEBVTT\n\n00:01:05.000 --> 00:01:08.000\nspyral lucky block\n")
        self.assertEqual(captions[0].seconds, 65)
        self.assertEqual(mod_matches(captions[0].text, ["spiral lucky block"], True), .65)
        self.assertTrue(duplicate_title("Spiral Challenge Games!", "Spiral Challenge Games"))
        self.assertFalse(duplicate_title("Spiral Challenge Games vs. Oak", "Spiral Challenge Games vs. Birch"))
        self.assertFalse(duplicate_title("Spiral Challenge Games part 1", "Spiral Challenge Games part 2"))
