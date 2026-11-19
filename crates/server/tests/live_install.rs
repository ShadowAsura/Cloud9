use cloud9_core::Document;
use cloud9_mc::Profile;
use cloud9_server::{Create, Options, Server};
use std::path::{Path, PathBuf};

#[test]
#[ignore = "downloads verified runtime artifacts and invokes the official Forge installer"]
fn installs_without_accepting_eula() -> Result<(), Box<dyn std::error::Error>> {
    let java = PathBuf::from(std::env::var_os("CLOUD9_SMOKE_JAVA").ok_or("set CLOUD9_SMOKE_JAVA")?);
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let root = tempfile::tempdir()?;
    let mut raw: serde_json::Value =
        serde_json::from_str(include_str!("../../../schema/fixtures/v1/fixture-01.json"))?;
    raw["mods"] = serde_json::json!([]);
    let document = Document::from_value(raw)?;
    let server = Server::create(
        &document,
        Create {
            servers: root.path(),
            slug: "install-test",
            cache: &repo.join(".cache/minecraft"),
            templates: root.path(),
            aliases: root.path(),
            local_mods: root.path(),
            java: Some(&java),
            helper: &repo.join("helper-mod/build/libs/runner-helper-0.1.0.jar"),
            profile: &Profile::primary()?,
            options: Options {
                bind: Some("127.0.0.1".parse()?),
                ..Options::default()
            },
            accept_eula: false,
        },
    )?;
    assert!(!server.eula_accepted());
    assert!(!server.root.join("eula.txt").exists());
    assert!(Path::new(&server.root).join("server.properties").is_file());
    Ok(())
}
