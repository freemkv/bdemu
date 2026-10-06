use super::{
    ControlOutcome, classify_response, exit_code_for, flag_value, parse_run_args,
    response_is_error, validate_profile,
};

/// A per-test scratch directory under the crate's `target/` (never /tmp, per
/// the project no-/tmp scratch rule); `cargo clean` reclaims it.
fn test_scratch_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-scratch")
        .join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test scratch dir");
    dir
}

fn s(v: &str) -> String {
    v.to_string()
}

// Pins that the CLI derives its socket path from the shared `socket_name` policy, not a
// local computation.
#[test]
fn cli_socket_path_uses_the_shared_policy() {
    let p = crate::socket_name::socket_path_from(Some("/run/user/1000"), None)
        .expect("must accept a runtime dir");
    assert_eq!(
        p,
        std::path::PathBuf::from("/run/user/1000").join(crate::socket_name::SOCKET_FILENAME)
    );
    assert!(
        crate::socket_name::socket_path_from(None, None).is_err(),
        "unset XDG_RUNTIME_DIR must be refused, not silently /tmp"
    );
}

#[test]
fn ok_response_is_success() {
    assert!(!response_is_error(&["OK ejected".to_string()]));
    assert!(!response_is_error(&[
        "OK".to_string(),
        "profile: /x".to_string(),
        "disc: empty".to_string(),
    ]));
}

#[test]
fn err_response_is_failure() {
    // `bdemu load <bad-name>` -> "ERR disc not found": must be non-zero.
    assert!(response_is_error(&["ERR disc not found".to_string()]));
    assert!(response_is_error(&["ERR invalid disc name".to_string()]));
    // ERR appearing on a later line still fails.
    assert!(response_is_error(&[
        "OK".to_string(),
        "ERR something".to_string()
    ]));
}

#[test]
fn empty_or_garbage_response_is_failure() {
    // Closed socket / no reply: not an OK, so treat as failure.
    assert!(response_is_error(&[]));
    // First line lacks the OK prefix.
    assert!(response_is_error(&["unexpected".to_string()]));
}
// Catches a regression to existence-only checking: a zero-length blob must be BROKEN,
// non-empty must pass, absent stays non-fatal.
#[test]
fn zero_byte_blob_is_a_failure_not_a_pass() {
    use super::{BlobState, classify_blob};
    use std::io::ErrorKind;

    assert_eq!(classify_blob(Ok(0)), BlobState::Empty);
    assert!(
        classify_blob(Ok(0)).is_broken(),
        "an interrupted capture must fail validate, not pass it"
    );
    assert!(classify_blob(Ok(0)).describe().contains("0 bytes"));

    assert_eq!(classify_blob(Ok(204800)), BlobState::Present(204800));
    assert!(!classify_blob(Ok(204800)).is_broken());
    assert!(
        classify_blob(Ok(204800)).describe().contains("204800"),
        "the size must be reported, not a bare true/false"
    );

    // Absent is legitimate (a metadata-only disc fixture) and stays non-fatal.
    assert_eq!(classify_blob(Err(ErrorKind::NotFound)), BlobState::Missing);
    assert!(!classify_blob(Err(ErrorKind::NotFound)).is_broken());

    // Anything else (permissions, I/O error) cannot be vouched for.
    assert_eq!(
        classify_blob(Err(ErrorKind::PermissionDenied)),
        BlobState::Unreadable
    );
    assert!(classify_blob(Err(ErrorKind::PermissionDenied)).is_broken());
}

// Catches the mutation that ignores `terminated`: a reply cut short must be truncation even
// if it starts with "OK".
#[test]
fn missing_terminator_is_truncation_even_if_it_starts_ok() {
    // Terminator seen: ordinary OK / ERR handling.
    assert_eq!(
        classify_response(&[s("OK"), s("  disc-a (sectors=true)")], true),
        ControlOutcome::Ok
    );
    assert_eq!(
        classify_response(&[s("ERR disc not found")], true),
        ControlOutcome::Error
    );
    // Terminator NOT seen: truncated, even though the bytes that arrived look
    // like a valid OK response. This is the whole point of the fix.
    assert_eq!(
        classify_response(&[s("OK"), s("  disc-a (sectors=true)")], false),
        ControlOutcome::Truncated
    );
    // Nothing at all, unterminated: also truncated (not a bare error).
    assert_eq!(classify_response(&[], false), ControlOutcome::Truncated);
    // Empty but TERMINATED reply (no OK line) is a genuine error, not truncation.
    assert_eq!(classify_response(&[], true), ControlOutcome::Error);
}

/// The `!v.starts_with('-')` guard in flag_value: a flag's value that looks
/// like another flag is a MISSING value, not the value. Catches the regression
/// where `bdemu run --profile --disc x` silently set profile="--disc".
#[test]
fn flag_value_rejects_a_following_flag_and_missing_value() {
    let args = vec![s("bdemu"), s("run"), s("--profile"), s("mydir")];
    assert_eq!(
        flag_value(&args, 3),
        Some(&s("mydir")),
        "a plain value is taken"
    );

    let swallow = vec![s("bdemu"), s("run"), s("--profile"), s("--disc")];
    assert_eq!(
        flag_value(&swallow, 3),
        None,
        "a value that looks like a flag must be refused, not swallowed"
    );

    // Past the end of args -> None (missing value).
    assert_eq!(flag_value(&args, 99), None);
}

/// The `bdemu run` flag scan: flags before `--`, the first positional starting
/// the command, and a missing flag value surfacing as Err(flag). Catches
/// mutations to the loop's stop conditions or index handling.
#[test]
fn parse_run_args_covers_the_flag_scan() {
    // Full form with an explicit `--` separator.
    let a = [
        "bdemu",
        "run",
        "--profile",
        "p",
        "--disc",
        "d",
        "--",
        "cmd",
        "arg",
    ]
    .map(s)
    .to_vec();
    let r = parse_run_args(&a).expect("well-formed");
    assert_eq!(r.profile.as_deref(), Some("p"));
    assert_eq!(r.disc.as_deref(), Some("d"));
    assert_eq!(r.cmd_start, 7, "cmd_start points just past `--`");

    // Short flags, no `--`: the first non-flag token starts the command.
    let b = ["bdemu", "run", "-p", "p", "cmd", "arg"].map(s).to_vec();
    let r = parse_run_args(&b).expect("well-formed");
    assert_eq!(r.profile.as_deref(), Some("p"));
    assert!(r.disc.is_none());
    assert_eq!(r.cmd_start, 4);

    // No flags at all: the command starts immediately.
    let c = ["bdemu", "run", "cmd"].map(s).to_vec();
    let r = parse_run_args(&c).expect("well-formed");
    assert!(r.profile.is_none());
    assert_eq!(r.cmd_start, 2);

    // A flag missing its value (next token looks like a flag) is an error
    // naming that flag.
    let d = ["bdemu", "run", "--profile", "--disc", "x"].map(s).to_vec();
    assert!(
        matches!(parse_run_args(&d), Err("--profile")),
        "a flag whose value looks like another flag is a missing-value error"
    );
}

/// exit_code_for maps a signal-killed child to 128+signum (so CI can tell a
/// crash from an ordinary non-zero exit) and a normal exit to its code. Catches
/// a mutation that collapses signals to a bare 1.
#[test]
fn exit_code_for_distinguishes_signals_from_normal_exit() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    // Wait-status encoding: exit code n is (n << 8); a signal is the low 7 bits.
    assert_eq!(exit_code_for(&ExitStatus::from_raw(0)), 0, "clean exit");
    assert_eq!(
        exit_code_for(&ExitStatus::from_raw(5 << 8)),
        5,
        "exit code 5"
    );
    assert_eq!(
        exit_code_for(&ExitStatus::from_raw(9)),
        128 + 9,
        "SIGKILL must be 137, not a bare 1"
    );
    assert_eq!(
        exit_code_for(&ExitStatus::from_raw(11)),
        128 + 11,
        "SIGSEGV must be 139"
    );
}

// Catches a mutation that drops an `ok = false` assignment (a broken profile must not pass
// validate).
#[test]
fn validate_returns_true_only_for_a_complete_profile() {
    let root = test_scratch_dir("validate_exit");

    // A complete-enough profile: drive.toml + 96-byte inquiry + the three
    // required feature blobs. No discs dir is a warning, not a failure.
    let good = root.join("good");
    std::fs::create_dir_all(&good).unwrap();
    std::fs::write(good.join("drive.toml"), "[drive]\nproduct = \"X\"\n").unwrap();
    std::fs::write(good.join("inquiry.bin"), vec![0u8; 96]).unwrap();
    for f in ["gc_0000.bin", "gc_0108.bin", "gc_010c.bin"] {
        std::fs::write(good.join(f), vec![1u8; 8]).unwrap();
    }
    assert!(
        validate_profile(good.to_str().unwrap()),
        "a complete profile must validate clean"
    );

    // Break it with an interrupted-capture disc: a zero-byte sectors.bin.
    let disc = good.join("discs").join("d1");
    std::fs::create_dir_all(&disc).unwrap();
    std::fs::write(disc.join("toc.bin"), vec![1u8; 4]).unwrap();
    std::fs::write(disc.join("sectors.bin"), Vec::<u8>::new()).unwrap();
    assert!(
        !validate_profile(good.to_str().unwrap()),
        "a zero-byte sectors.bin (interrupted capture) must fail the gate"
    );

    // A profile missing the required inquiry/features fails too.
    let bad = root.join("bad");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("drive.toml"), "[drive]\nproduct = \"X\"\n").unwrap();
    assert!(
        !validate_profile(bad.to_str().unwrap()),
        "a profile missing inquiry/features must fail the gate"
    );

    let _ = std::fs::remove_dir_all(&root);
}
