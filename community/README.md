# Community episode setups

## Tested example

[Chicken Jockey Challenge Games](chicken-jockey.md) includes a playable solo round, a pinned Lucky Block dependency, and a repeatable Forge playtest.

[Community map sources](maps.md) lists creator-published world downloads for other setups.

## Contributing a setup

Cloud 9 indexes public episode metadata and caches thumbnails locally. Contribute gameplay setup as `community/VIDEO_ID.json`. Use the YouTube video ID and include timestamps for evidence that appears on screen.

Patch paths can target `/game`, `/mods`, `/map`, `/villagers`, `/center`, `/findings`, and `/allow_multiple_lucky`. A path such as `/villagers/@merchant/trades/@starter` selects stable IDs. To add the first record, replace its entire list. See the [manifest fixtures](../schema/fixtures/v1) for trades and chest slots.

Keep contributions to original setup data and source references. Use `overrides/VIDEO_ID.json` for personal changes. The catalog applies community patches first and personal patches second.

For example, this patch sets the reviewed spawn and cites the video time that informed it.

```json
{
  "schema_version": 1,
  "patches": [{
    "path": "/map/overrides",
    "value": {"spawn": [0, 65, 0], "gamerules": {"keepInventory": "true"}}
  }],
  "evidence": [{"path": "/map/overrides", "timestamp_seconds": 94}]
}
```

Re-index the episode to apply the patch. Test its run plan with the specified jars and Java before submitting a pull request.
