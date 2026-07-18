use std::path::Path;

fn main() {
    bake_github_client_id();
    bake_release_tag();
    tauri_build::build();
}

/// Bake the release tag (e.g. `v0.1.0-beta.10`) so the app can tell whether a
/// newer published version exists (see `check_for_app_update`). CI sets
/// `HIVE_RELEASE_TAG`; local/dev builds get `dev` and never show the update
/// banner (avoids false "update available" prompts during development).
fn bake_release_tag() {
    println!("cargo:rerun-if-env-changed=HIVE_RELEASE_TAG");
    let tag = std::env::var("HIVE_RELEASE_TAG")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dev".to_string());
    println!("cargo:rustc-env=HIVE_RELEASE_TAG={tag}");
}

/// Inject the GitHub OAuth App client id at compile time so `option_env!` /
/// `env!("HIVE_GITHUB_CLIENT_ID")` resolves in the app, *without* committing the
/// id to source. Resolution order:
///   1. env `HIVE_GITHUB_CLIENT_ID` (CI / one-off builds)
///   2. a gitignored `app/github_client_id` file (official local builds)
/// Forks without either build with no default (users paste their own in-app).
fn bake_github_client_id() {
    println!("cargo:rerun-if-env-changed=HIVE_GITHUB_CLIENT_ID");
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("github_client_id");
    println!("cargo:rerun-if-changed={}", file.display());

    let id = std::env::var("HIVE_GITHUB_CLIENT_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::fs::read_to_string(&file).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default();

    println!("cargo:rustc-env=HIVE_GITHUB_CLIENT_ID={id}");
}
