use cloud9_core::{Document, apply_overrides};
use cloud9_mc::{download_pinned_mods, resolve_mods};

#[test]
#[ignore = "downloads the official Lucky Block release to a disposable cache"]
fn official_lucky_block_download_resolves_without_local_jars()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let source =
        serde_json::from_str(include_str!("../../../community/episodes/Xr5TLvfhZuU.json"))?;
    let patch = serde_json::from_str(include_str!("../../../community/Xr5TLvfhZuU.json"))?;
    let (episode, _) = apply_overrides(&Document::from_value(source)?, &patch)?;
    let cache = root.path().join("cache");
    assert_eq!(download_pinned_mods(episode.manifest(), &cache)?, 1);
    let mods = resolve_mods(
        episode.manifest(),
        &root.path().join("unused"),
        &cache,
        false,
    )?;
    assert_eq!(mods[0].mod_id, "lucky");
    assert_eq!(mods[0].version, "7.0.0");
    assert_eq!(download_pinned_mods(episode.manifest(), &cache)?, 1);
    Ok(())
}
