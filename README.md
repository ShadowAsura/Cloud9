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

For live indexing, put yt-dlp on PATH and use `python -m scraper scrape --keyword "PopularMMOs Challenge Games" --uploader PopularMMOs --limit 25 --thumbnails`. Live indexing collects titles, game modes, mod clues, and thumbnail links from public listings. Thumbnail images stay in the ignored local cache. [yt-dlp options](https://github.com/yt-dlp/yt-dlp#usage-and-options)

Contribute reviewed villager trades, chests, kits, and layouts in `community/VIDEO_ID.json`. The catalog applies personal changes from `overrides/VIDEO_ID.json` afterward. [Community setup guide](community/README.md)

The [Chicken Jockey community setup](community/chicken-jockey.md) is a tested solo example that can be added to the catalog offline.

## M3 · Desktop episode browser

After seeding or scraping the index, run `cargo run -p cloud9-app`. Pass a custom SQLite path after `--` if required. The browser queries one page at a time and loads local thumbnails for visible cards on a background thread. The desktop fonts are distributed under the SIL Open Font License. [IBM Plex](https://github.com/IBM/plex)

## M4 · Minecraft runtime

The **Instances** screen reopens saved episodes, shows their locked Java and mods, and lets you resume a save or start a fresh round. Start a new instance from the episode browser to pick up later community changes. [Prism instance guide](https://www.prismlauncher.org/wiki/getting-started/create-instance/)

Choose **Run plan** on an episode card, point it at your local mod directory, and select a Java 8 executable or set `JAVA8_HOME`. The launcher downloads hashed Minecraft and Forge artifacts to the ignored cache and copies verified user-supplied mods into a fresh instance. If the Forge version JSON exists without its JAR, preparation repairs the installation.

For Microsoft sign-in, supply an authorized public-client ID when prompted or set `CLOUD9_MICROSOFT_CLIENT_ID`. Cloud 9 keeps game credentials in memory. Local development mode is an explicit setting for local testing. Minecraft output and diagnostics appear under **Running**. [Microsoft device sign-in](https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-device-code)

The opt-in runtime smoke test requires a desktop, an internet connection, and `CLOUD9_SMOKE_JAVA` pointing to Java 8. Run `cargo test -p cloud9-mc --test live_client -- --ignored`.

## M5 · Worlds and setup

Build the helper with Java 8 and Gradle 2.14.1 using `gradle -p helper-mod setupCIWorkspace build`. The run plan expects `helper-mod/build/libs/runner-helper-0.1.0.jar` by default. The first run with a new mod set opens a disposable local Forge client to produce the exact registry. The run plan resolves manifest items before preparing the playable save.

The two built-in template saves are generated locally as needed. To use your own template, put its save folder under `templates/` and set the manifest's `map.template_id` to that folder name. The current helper opens the prepared solo save on launch, fills chests, builds structures, adds villagers and trades, and applies round settings. **Reset round and launch** restores the pre-launch snapshot after Minecraft has exited and keeps a backup of the prior round.

Use `aliases/<mod_id>/<version>.json` to map lowercased raw labels or registry IDs to an object with `id` and optional `meta` and `nbt` fields.

## M6 · Dedicated servers

Open **Servers** after choosing an episode. Choose a local folder name, player limit, RAM, and port. Port zero selects an available port. Review and explicitly accept the EULA before starting. The screen shows the join address, TCP reachability, console output, and connected players. Servers authenticate players by default. Keep offline servers on a trusted LAN. [Minecraft EULA](https://www.minecraft.net/en-us/eula)

Copy `server.example.toml` to `server.toml` for headless use. Paths are relative to the configuration file. Set `accept_eula = true` only after accepting the EULA. Use separate terminals or a service unit for the supervisor and control commands.

```sh
cargo run -p runner-cli -- server create
cargo run -p runner-cli -- server start
cargo run -p runner-cli -- server logs --follow
cargo run -p runner-cli -- server reset
cargo run -p runner-cli -- server stop
```

To expose a port, confirm internet exposure before enabling UPnP. A tunnel can be configured with `settings.tunnel` as an executable and argument array, `settings.tunnel_address` as the public join address, and `settings.authorize_public = true`. The provider receives `CLOUD9_LOCAL_ADDRESS`. A TCP check reports reachability from the machine issuing the test. Cloud 9 applies Mojang's legacy server logging mitigation. [Minecraft server logging guidance](https://www.minecraft.net/en-us/article/important-message--security-vulnerability-java-edition)

Export a `.matchup` file from Servers or with `runner-cli server export round.matchup`. The recipient imports it from Servers after indexing the same episode, supplies missing mod jars, then signs in and joins. The matchup pins server-side mod hashes and carries a manifest ID and join address.

## M7 · Maintenance

GitHub Actions tests the Rust workspace and offline Python scraper on Windows and Linux. Successful runs provide desktop and headless binaries in build artifacts. The helper and game dependencies are built or acquired separately.

Use `runner-cli bundle instances/INSTANCE report.zip` or pass a server folder to collect local diagnostic log excerpts. Review the archive before sharing. `runner-cli cache-prune` removes older content-addressed mod cache entries. Existing runs have their own copies. The scraper keeps its personal thumbnail cache within 256 MiB. If an interrupted scrape leaves a JSON export missing, run `python -m scraper sync` to rebuild exports from SQLite.

The offline recorded-fixture command supports `--extractor module:factory` for testing source-cited findings against the manifest schema.

Set `CLOUD9_PROFILE` to a JSON runtime profile for another compatible legacy Forge target. The headless form accepts the same fields in a TOML profile. A profile needs Java, Forge artifacts and hashes, and its server logging configuration. Use a helper built for the targeted Minecraft version.
