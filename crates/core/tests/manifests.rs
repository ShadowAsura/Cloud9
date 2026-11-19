use cloud9_core::{Document, Error, apply_overrides, migrate};
use serde_json::{Value, json};

const FIXTURES: [&str; 6] = [
    include_str!("../../../schema/fixtures/v1/fixture-01.json"),
    include_str!("../../../schema/fixtures/v1/fixture-02.json"),
    include_str!("../../../schema/fixtures/v1/fixture-03.json"),
    include_str!("../../../schema/fixtures/v1/fixture-04.json"),
    include_str!("../../../schema/fixtures/v1/fixture-05.json"),
    include_str!("../../../schema/fixtures/v1/fixture-06.json"),
];

fn fixture(index: usize) -> Value {
    serde_json::from_str(FIXTURES[index]).expect("valid fixture JSON")
}

#[test]
fn round_trip_all_fixtures() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("雪 and spaces.json");
    for index in 0..FIXTURES.len() {
        let source = fixture(index);
        let document = Document::from_value(source.clone()).expect("validate manifest");
        document.save(&path).expect("atomic save");
        let loaded = Document::load(&path).expect("load manifest");
        assert_eq!(loaded.source(), &source);
        assert_eq!(loaded.manifest().schema_version, 1);
    }
}

#[test]
fn migration_dispatch() {
    let source = fixture(0);
    assert_eq!(migrate(source.clone()).expect("current format"), source);
    assert!(matches!(
        migrate(json!({"schema_version": 8})),
        Err(Error::Version(8))
    ));
    assert!(migrate(json!({})).is_err());
}

#[test]
fn stable_override_survives_reordering_and_reports_conflict() {
    let mut source = fixture(0);
    let mut second = source["mods"][0].clone();
    second["id"] = json!("second");
    source["mods"]
        .as_array_mut()
        .expect("mods")
        .insert(0, second);
    let patches = json!({"schema_version": 1, "patches": [{
        "path": "/mods/@spiral/mod_id",
        "expected": {"value": "old_scrape"},
        "value": {"value": "verified_id", "status": "overridden", "confidence": 1, "source": "manual"}
    }]});
    let (updated, conflicts) =
        apply_overrides(&Document::from_value(source).expect("manifest"), &patches)
            .expect("valid override");
    assert_eq!(conflicts.len(), 1);
    assert_eq!(
        updated.manifest().mods[1].mod_id.value.as_deref(),
        Some("verified_id")
    );
    assert_eq!(updated.manifest().mods[0].mod_id.value, None);
}

#[test]
fn rejects_duplicate_ids_and_invalid_evidence() {
    let mut source = fixture(0);
    let first = source["mods"][0].clone();
    source["mods"].as_array_mut().expect("mods").push(first);
    assert!(Document::from_value(source).is_err());
    let mut source = fixture(4);
    source["findings"][0]
        .as_object_mut()
        .expect("finding")
        .remove("timestamp_seconds");
    assert!(Document::from_value(source).is_err());
}
