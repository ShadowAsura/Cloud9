use cloud9_core::Document;
use cloud9_mc::{
    Account, ClientRuntime, Installer, Profile, bootstrap_client_registry,
    create_instance_with_helper,
};
use cloud9_world::{EpisodeWorld, SetupContext};
use std::path::PathBuf;

#[test]
#[ignore = "starts a local Forge client with the locally built helper"]
fn discover_registry_and_prepare_world() -> Result<(), Box<dyn std::error::Error>> {
    let java = PathBuf::from(std::env::var_os("CLOUD9_SMOKE_JAVA").ok_or("set CLOUD9_SMOKE_JAVA")?);
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cache = repo.join(".cache/minecraft");
    let temp = tempfile::tempdir()?;
    let mut raw: serde_json::Value =
        serde_json::from_str(include_str!("../../../schema/fixtures/v1/fixture-02.json"))?;
    raw["mods"] = serde_json::json!([]);
    let document = Document::from_value(raw)?;
    let instance = create_instance_with_helper(
        temp.path(),
        &cache,
        &document,
        &Profile::primary()?,
        temp.path(),
        Some(&java),
        &repo.join("helper-mod/build/libs/runner-helper-0.1.0.jar"),
    )?;
    let runtime = ClientRuntime::prepare(&Installer::new(&cache)?, &instance)?;
    let registry = bootstrap_client_registry(
        &runtime,
        &instance,
        &temp.path().join("registries"),
        1536,
        &Account::local_dev("Cloud9Registry")?,
    )?;
    assert!(registry.items.contains_key("minecraft:gold_ingot"));
    assert!(registry.blocks.contains_key("minecraft:stone"));
    let templates = temp.path().join("templates");
    let episode = EpisodeWorld::open(
        &document,
        instance.root.join("saves/Cloud9"),
        instance.root.join("snapshot"),
        SetupContext {
            templates: &templates,
            aliases: temp.path(),
            mods: &instance.mods,
            registry: &registry,
            mod_set_hash: &instance.mod_set_hash,
            run_id: "test",
        },
    )?;
    episode.prepare()?;
    assert!(episode.world.join("runner_setup.json").is_file());
    Ok(())
}
