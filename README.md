# Cloud 9

Cloud 9 is a toolchain for indexing Minecraft Challenge Games and Lucky Block Bedwars episodes and preparing sessions from a shared manifest. The primary runtime profile is Minecraft 1.8.9, Forge 11.15.1.2318 and Java 8. [Forge releases for Minecraft 1.8.9](https://files.minecraftforge.net/net/minecraftforge/forge/index_1.8.9.html)

## M1 · Manifests

Run `cargo test -p cloud9-core`. The schemas, initial SQLite migration, and six original examples are under `schema/`. The two messy examples include unresolved items, conflicting evidence, and extensions to exercise round trips. Version 1 establishes the first supported format. The migration dispatcher rejects unknown versions and is the entry point for future schema upgrades.

Evidence records preserve their source, confidence, status, alternatives, and timestamp when applicable. Setup records use stable sibling IDs. Review files contain replacements with a JSON Pointer path, value, and optional expected scrape value. An array segment such as `/mods/@spiral/mod_id` selects a record by its stable ID. A mismatch with `expected` is reported as a conflict and the reviewed value still applies.

The MIT license covers Cloud 9 source and original fixtures. Minecraft, Forge, third-party mod jars, videos, and locally cached thumbnails are obtained separately.
## M2 · Episode scraper

Create a Python 3.11+ virtual environment and install `scraper/requirements.txt`. Run the offline tests from the repository root.

```sh
python -m unittest discover -s scraper/tests
python -m scraper seed schema/fixtures/v1
python -m scraper recorded scraper/tests/fixtures/with-captions.json
python -m scraper review --video-id recorded-01
```

For live indexing, put yt-dlp on PATH and use `python -m scraper scrape --keyword "PopularMMOs Challenge Games" --limit 20`. Channel and playlist URLs and explicit video IDs are also supported. Add `--thumbnails` to keep personal-use thumbnails in the ignored cache. Live scraping uses yt-dlp in metadata-only mode. [yt-dlp options](https://github.com/yt-dlp/yt-dlp#usage-and-options)

Review changes belong in `overrides/VIDEO_ID.json`. The scraper reapplies them on each successful rescrape. Use the review command to get stable paths and timestamped source links.

## M3 · Desktop episode browser

After seeding or scraping the index, run `cargo run -p cloud9-app`. Pass a custom SQLite path after `--` if required. The browser queries one page at a time and loads local thumbnails for visible cards on a background thread. The desktop fonts are distributed under the SIL Open Font License. [IBM Plex](https://github.com/IBM/plex)
