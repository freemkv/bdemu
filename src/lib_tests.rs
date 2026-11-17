use super::profile_base;
use std::path::PathBuf;

#[test]
fn profile_base_maps_a_bare_json_filename_to_cwd() {
    assert_eq!(profile_base("profile.json"), PathBuf::from("."));
}

#[test]
fn profile_base_uses_a_json_profiles_parent_dir() {
    assert_eq!(
        profile_base("/nonexistent-bdemu/profiles/drive.json"),
        PathBuf::from("/nonexistent-bdemu/profiles")
    );
}

#[test]
fn profile_base_keeps_a_profile_directory() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert_eq!(profile_base(dir.to_str().unwrap()), dir);
}
