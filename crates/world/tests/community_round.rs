use cloud9_core::{Document, apply_overrides};
use cloud9_mc::{
    Account, ClientRuntime, Installer, Profile, bootstrap_client_registry,
    create_instance_with_helper,
};
use cloud9_world::{EpisodeWorld, SetupContext};
use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

#[test]
#[ignore = "requires Java 8, the official Lucky Block jar, the playtest helper and a desktop"]
fn chicken_jockey_from_launch_to_victory() -> Result<(), Box<dyn std::error::Error>> {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let temp = tempfile::tempdir()?;
    let cache = repo.join(".cache/minecraft");
    let base = Document::load(&repo.join("community/episodes/Xr5TLvfhZuU.json"))?;
    let patch = serde_json::from_slice(&fs::read(repo.join("community/Xr5TLvfhZuU.json"))?)?;
    let (document, _) = apply_overrides(&base, &patch)?;
    let profile = Profile::primary()?;
    let supplied =
        std::env::var_os("CLOUD9_TEST_MODS").map_or_else(|| repo.join("local-mods"), PathBuf::from);
    let java = std::env::var_os("CLOUD9_SMOKE_JAVA").map(PathBuf::from);
    let helper = repo.join("helper-mod/build/libs/runner-helper-playtest-0.1.0.jar");
    let account = Account::local_dev("Cloud9Tester")?;
    let instance = create_instance_with_helper(
        temp.path(),
        &cache,
        &document,
        &profile,
        &supplied,
        java.as_deref(),
        &helper,
    )?;
    let runtime = ClientRuntime::prepare(&Installer::new(&cache)?, &instance)?;
    let registry = bootstrap_client_registry(
        &runtime,
        &instance,
        &cache.join("registries"),
        1536,
        &account,
    )?;
    let round = EpisodeWorld::open(
        &document,
        instance.root.join("saves/Cloud9"),
        instance.root.join("round-snapshot"),
        SetupContext {
            templates: &temp.path().join("templates"),
            aliases: temp.path(),
            mods: &instance.mods,
            registry: &registry,
            mod_set_hash: &instance.mod_set_hash,
            run_id: "community-test",
        },
    )?;
    round.prepare()?;
    let completion = round.world.join("runner_challenge_complete.txt");
    let result = round.world.join("runner_playtest.txt");
    let (logs, receiver) = mpsc::channel();
    let mut client = runtime.launch_prepared(&instance, &account, 1536, logs)?;
    let outcome = (|| -> Result<String, Box<dyn std::error::Error>> {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(120) {
            if let Ok(value) = fs::read_to_string(&result)
                && !value.is_empty()
            {
                return Ok(value);
            }
            if let Ok(event) = receiver.recv_timeout(Duration::from_millis(200)) {
                eprintln!("{}", event.line);
            }
            if let Some(status) = client.try_wait()? {
                return Err(
                    format!("Minecraft exited before the playtest finished ({status})").into(),
                );
            }
        }
        Err("Playtest timed out waiting for the prepared world".into())
    })();
    if client.try_wait()?.is_none() {
        client.kill()?;
        client.wait()?;
    }
    assert_eq!(outcome?, "passed");
    assert!(completion.is_file());
    let previous = round.reset()?;
    assert!(previous.join("runner_challenge_complete.txt").is_file());
    assert!(!completion.exists());
    assert!(round.world.join("runner_setup.json").is_file());
    Ok(())
}
