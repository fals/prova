//! Committed JSON Schemas must match the Rust contract types.
//! Regenerate with `TOE_UPDATE_SCHEMAS=1 cargo test --test schemas`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

fn check(name: &str, schema: schemars::Schema) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas")
        .join(name);
    let generated = serde_json::to_string_pretty(&schema).expect("serialize") + "\n";
    if std::env::var_os("TOE_UPDATE_SCHEMAS").is_some() {
        std::fs::write(&path, &generated).expect("write schema");
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == generated,
        "{name} is out of date; run TOE_UPDATE_SCHEMAS=1 cargo test --test schemas"
    );
}

#[test]
fn schemas_are_current() {
    check(
        "event.v1.json",
        schemars::schema_for!(toe_core::model::Event),
    );
}
