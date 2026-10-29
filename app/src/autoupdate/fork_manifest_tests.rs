use super::*;

#[test]
fn parses_manifest_without_url() {
    let manifest: ForkManifest =
        serde_json::from_str(r#"{"version":"v0.2026.10.29.12.00.oss_00","sha256":"ab"}"#).unwrap();
    assert_eq!(manifest.version, "v0.2026.10.29.12.00.oss_00");
    assert_eq!(manifest.url, None);
    assert_eq!(manifest.sha256.as_deref(), Some("ab"));
}

#[test]
fn fork_versions_are_ordered_by_date() {
    let older = channel_versions::ParsedVersion::try_from("v0.2026.10.29.12.00.oss_00").unwrap();
    let newer = channel_versions::ParsedVersion::try_from("v0.2026.11.02.09.30.oss_00").unwrap();
    assert!(newer > older);
}

#[test]
fn verifies_sha256() {
    let actual = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"warp"));
    assert!(verify_sha256(b"warp", Some(&actual)).is_ok());
    assert!(verify_sha256(b"warp", Some(&actual.to_uppercase())).is_ok());
    assert!(verify_sha256(b"other", Some(&actual)).is_err());
    assert!(verify_sha256(b"warp", None).is_err());
}
