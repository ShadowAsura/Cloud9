use cloud9_core::Document;
use cloud9_mc::{Account, ClientRuntime, Installer, Profile, create_instance};
use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

#[test]
#[ignore = "downloads official runtime artifacts and opens a local Minecraft window"]
fn launch_legacy_client() -> Result<(), Box<dyn std::error::Error>> {
    let java = PathBuf::from(std::env::var_os("CLOUD9_SMOKE_JAVA").ok_or("set CLOUD9_SMOKE_JAVA")?);
    let cache = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.cache/minecraft");
    let tmp = tempfile::tempdir()?;
    let mut raw: serde_json::Value =
        serde_json::from_str(include_str!("../../../schema/fixtures/v1/fixture-01.json"))?;
    raw["mods"] = serde_json::json!([]);
    raw["villagers"] = serde_json::json!([]);
    let document = Document::from_value(raw)?;
    let profile = Profile::primary()?;
    let instance = create_instance(
        tmp.path(),
        &cache,
        &document,
        &profile,
        tmp.path(),
        Some(&java),
    )?;
    let installer = Installer::new(&cache)?;
    ClientRuntime::prepare(&installer, &instance)?;
    let forge_jar = installer
        .root
        .join("versions")
        .join(&profile.forge_version_id)
        .join(format!("{}.jar", profile.forge_version_id));
    std::fs::remove_file(&forge_jar)?;
    let runtime = ClientRuntime::prepare(&installer, &instance)?;
    assert!(forge_jar.is_file());
    let (logs, receiver) = mpsc::channel();
    let mut child = runtime.launch(
        &instance,
        &Account::local_dev("Cloud9Dev")?,
        1536,
        None,
        logs,
    )?;
    let until = Instant::now() + Duration::from_secs(75);
    let mut started = false;
    while Instant::now() < until {
        if let Ok(event) = receiver.recv_timeout(Duration::from_millis(250)) {
            eprintln!("{}", event.line);
            if event.line.contains("Created: ") && event.line.contains("textures-atlas") {
                started = true;
                break;
            }
        }
        if child.try_wait()?.is_some() {
            break;
        }
    }
    if child.try_wait()?.is_none() {
        child.kill()?;
        child.wait()?;
    }
    assert!(started, "Minecraft did not reach texture atlas creation");
    Ok(())
}
