use super::*;

/// Catches the mutation that drops per-instance identity (always returning
/// `bdemu.sock`): two concurrent emulators would then share one socket and
/// `bdemu load` would reach whichever bound last.
#[test]
fn instance_id_yields_a_distinct_socket() {
    let a = socket_path_from(Some("/run/user/1000"), Some("drive-a")).expect("valid id");
    let b = socket_path_from(Some("/run/user/1000"), Some("drive_b")).expect("valid id");
    assert_ne!(a, b, "distinct instances must not share a socket");
    assert_eq!(a, PathBuf::from("/run/user/1000/bdemu-drive-a.sock"));
}

/// Catches a regression in the zero-configuration path: with no instance set
/// (or an exported-but-empty variable) the historical single-instance socket
/// name must be preserved, or every existing invocation breaks.
#[test]
fn default_instance_keeps_the_historical_name() {
    assert_eq!(
        socket_path_from(Some("/run/user/1000"), None).unwrap(),
        PathBuf::from("/run/user/1000").join(SOCKET_FILENAME)
    );
    assert_eq!(
        socket_path_from(Some("/run/user/1000"), Some("")).unwrap(),
        PathBuf::from("/run/user/1000").join(SOCKET_FILENAME)
    );
}

/// Catches a mutation that sanitises instead of rejecting, or that forgets
/// the component check entirely: an id containing a separator would escape
/// the 0700 runtime directory that makes the socket owner-only.
#[test]
fn hostile_instance_ids_are_rejected() {
    for bad in ["../evil", "a/b", "a\\b", "a b", "a\0b", "a.sock", "café"] {
        assert!(
            socket_filename(Some(bad)).is_err(),
            "instance id {bad:?} must be rejected"
        );
    }
    // Over-length ids are refused rather than truncated (truncation would
    // silently merge two distinct instances onto one socket).
    let long = "a".repeat(MAX_INSTANCE_LEN + 1);
    assert!(socket_filename(Some(&long)).is_err());
    assert!(socket_filename(Some(&"a".repeat(MAX_INSTANCE_LEN))).is_ok());
}

/// Catches removal of the /tmp-fallback refusal: a world-writable socket
/// would let any local user drive the emulator.
#[test]
fn insecure_tmp_fallback_is_refused() {
    let err = socket_path_from(None, None).expect_err("None must be refused");
    assert!(err.contains("XDG_RUNTIME_DIR"), "got: {err}");
    assert!(socket_path_from(Some(""), None).is_err(), "empty refused");
}
