use super::*;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

fn fixture_state() -> (
    Arc<Mutex<crate::profile::LoadedProfile>>,
    Arc<Mutex<EmulatorState>>,
) {
    let profile = Arc::new(Mutex::new(crate::profile::LoadedProfile {
        name: "test".to_string(),
        inquiry: Vec::new(),
        current_profile: 0,
        features: Vec::new(),
        rpc_state: Vec::new(),
        read_bufs: Vec::new(),
        mode_2a: Vec::new(),
        disc: None,
    }));
    let state = Arc::new(Mutex::new(EmulatorState {
        profile_dir: PathBuf::from("/nonexistent-bdemu-test-profile"),
        disc: DiscState::Empty,
    }));
    (profile, state)
}

fn run_one(input: Vec<u8>) -> String {
    let (client, server) = UnixStream::pair().expect("socketpair");
    let (profile, state) = fixture_state();
    let worker = std::thread::spawn(move || handle_client(server, &profile, &state));

    // Tolerates a broken pipe: once the server hits its 1 KiB cap it
    // responds and drops the socket, so a client still pushing sees EPIPE.
    // That's the cap working, not a test failure — ignore the write result.
    let mut writer = client.try_clone().expect("clone");
    let pusher = std::thread::spawn(move || {
        let _ = writer.write_all(&input);
        let _ = writer.shutdown(std::net::Shutdown::Write);
    });

    // Read tolerantly: the server drops the socket right after writing, so
    // the read can surface ECONNRESET. `read_to_string` would discard
    // everything on error; `read_to_end` keeps bytes read so far. Decode lossily.
    let mut buf = Vec::new();
    let mut reader = client;
    let _ = reader.read_to_end(&mut buf);
    let resp = String::from_utf8_lossy(&buf).into_owned();
    let _ = pusher.join();
    worker.join().expect("worker join");
    resp
}

#[test]
fn oversized_line_is_rejected_without_oom_or_panic() {
    // A local peer streams far more than the 1 KiB cap with no command
    // keyword: the read is truncated at the cap and the truncated text falls
    // through to the unknown-command path. No panic, no unbounded allocation.
    let input = vec![b'A'; 64 * 1024];
    let resp = run_one(input);
    assert!(
        resp.starts_with("ERR unknown command:"),
        "oversized input should be rejected as unknown command, got: {resp:?}"
    );
    // The echoed-back command text is bounded by the read cap, not the input.
    assert!(
        resp.len() <= 1024 + 64,
        "response should reflect the capped read, got {} bytes",
        resp.len()
    );
}

#[test]
fn normal_command_still_parses() {
    let resp = run_one(b"status\n".to_vec());
    assert!(
        resp.lines().next() == Some("OK"),
        "status should respond OK first, got: {resp:?}"
    );
    assert!(
        resp.contains("disc: empty"),
        "status should report the empty disc state, got: {resp:?}"
    );
}

// Every control verb must reach its handler through parse_command +
// handle_client over a real socket, not just `status`. eject/load touch
// the process-global media-change flag, so serialize against SCSI tests.
#[test]
fn dispatch_routes_every_verb_end_to_end() {
    let _g = crate::scsi::TEST_GUARD
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    // status -> OK with disc state.
    assert!(
        run_one(b"status\n".to_vec()).contains("disc:"),
        "status must route to cmd_status"
    );
    // eject -> OK ejected.
    assert!(
        run_one(b"eject\n".to_vec()).contains("OK ejected"),
        "eject must route to cmd_eject"
    );
    // list-discs -> the fixture profile_dir has no discs directory.
    assert!(
        run_one(b"list-discs\n".to_vec()).contains("no discs directory"),
        "list-discs must route to cmd_list_discs"
    );
    // load <bad> -> ERR (routes to cmd_load, which rejects the traversal name).
    assert!(
        run_one(b"load ../escape\n".to_vec()).contains("ERR"),
        "load must route to cmd_load"
    );
    // An unrecognised verb is the explicit unknown-command error.
    assert!(
        run_one(b"bogus\n".to_vec()).contains("ERR unknown command"),
        "an unknown verb must be reported, not silently accepted"
    );
}

/// The wire response ends with the shared terminator sentinel so the CLI can
/// tell a complete reply from one cut short. Catches the mutation that stops
/// writing the terminator (which would make every CLI call look truncated).
#[test]
fn response_is_terminated_on_the_wire() {
    let resp = run_one(b"status\n".to_vec());
    let last = resp.lines().last();
    assert_eq!(
        last,
        Some(socket_name::CONTROL_TERMINATOR),
        "a complete response must end with the terminator line, got: {resp:?}"
    );
}
/// A per-test scratch directory under the crate's `target/` (never /tmp, per
/// the project no-/tmp scratch rule) — short enough that a Unix socket path
/// inside it stays within `sun_path`.
fn test_scratch_dir(tag: &str) -> PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-scratch")
        .join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test scratch dir");
    dir
}

// Catches a regression to the unconditional unlink-before-bind: a second emulator would
// steal a LIVE socket out from under the first.
#[test]
fn a_live_control_socket_is_never_stolen() {
    let dir = test_scratch_dir("ctl_live");
    let path = dir.join("bdemu.sock");

    // A live listener, exactly as a running emulator would leave one.
    let _live = UnixListener::bind(&path).expect("bind fixture listener");
    let err = reclaim_socket_path(&path).expect_err("a live socket must not be reclaimed");
    assert!(err.contains("already in use"), "got: {err}");
    assert!(
        err.contains(socket_name::INSTANCE_ENV),
        "the error must tell the operator how to run a second instance: {err}"
    );
    // …and the live socket is still there.
    assert!(path.exists(), "the live socket must not have been unlinked");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The other half: a socket left behind by a crashed emulator has no
/// listener, so it must still be reclaimable — otherwise a single crash would
/// wedge the control socket until someone deleted the file by hand.
#[test]
fn a_stale_control_socket_is_reclaimed() {
    let dir = test_scratch_dir("ctl_stale");
    let path = dir.join("bdemu.sock");

    // Bind then drop: the socket file survives, but nothing is listening.
    {
        let _dead = UnixListener::bind(&path).expect("bind fixture listener");
    }
    assert!(path.exists(), "dropping a listener leaves the socket file");

    reclaim_socket_path(&path).expect("a stale socket must be reclaimable");
    assert!(!path.exists(), "the stale socket must have been removed");

    // A path with nothing at it is the ordinary first-instance case.
    reclaim_socket_path(&path).expect("an absent path is fine");

    let _ = std::fs::remove_dir_all(&dir);
}

// End-to-end coverage of the two mutating control verbs: `load` must resolve
// a disc, swap it into the live profile, and arm the media-change flag;
// `eject` must clear it. Catches a `load` that answers OK without swapping.
#[test]
fn load_then_eject_round_trip() {
    // cmd_load / cmd_eject set the process-global media-change flag the SCSI
    // layer reports as a UNIT ATTENTION, so serialize against the SCSI tests
    // that assert on it (see scsi::TEST_GUARD).
    let _g = crate::scsi::TEST_GUARD
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = test_scratch_dir("ctl_load_eject");
    let disc_dir = dir.join("discs").join("sample");
    std::fs::create_dir_all(&disc_dir).unwrap();
    // Two captured sectors in the legacy flat format.
    std::fs::write(disc_dir.join("sectors.bin"), vec![0xA5u8; 2 * SECTOR_SIZE]).unwrap();

    let (profile, state) = fixture_state();
    lock_recover(&state).profile_dir = dir.clone();

    // load
    let resp = cmd_load(&profile, &state, "sample");
    assert_eq!(
        resp.lines,
        vec!["OK loaded 'sample' (2 sectors)".to_string()],
        "load must report the disc it actually read"
    );
    assert!(
        lock_recover(&profile).disc.is_some(),
        "the disc must be swapped into the live profile, not just the state"
    );
    assert_eq!(lock_recover(&state).disc.name(), Some("sample"));
    // status reflects it.
    assert!(
        cmd_status(&state)
            .lines
            .iter()
            .any(|l| l == "disc: loaded (sample)"),
        "status must show the loaded disc"
    );

    // A name that does not resolve under discs/ is refused, and leaves the
    // loaded disc alone.
    let bad = cmd_load(&profile, &state, "../escape");
    assert_eq!(bad.lines, vec!["ERR invalid disc name".to_string()]);
    assert!(
        lock_recover(&profile).disc.is_some(),
        "a rejected load must not eject"
    );

    // eject
    let resp = cmd_eject(&profile, &state);
    assert_eq!(resp.lines, vec!["OK ejected".to_string()]);
    assert!(
        lock_recover(&profile).disc.is_none(),
        "eject must clear the disc from the profile"
    );
    assert!(matches!(lock_recover(&state).disc, DiscState::Empty));

    let _ = std::fs::remove_dir_all(&dir);
}

/// `cmd_load` with an empty disc name must be refused with a usage error,
/// not fall through to `safe_disc_dir` with an empty string (which happens
/// to also reject it, but for the wrong reason and with the wrong message).
#[test]
fn cmd_load_rejects_empty_name() {
    let (profile, state) = fixture_state();
    let resp = cmd_load(&profile, &state, "");
    assert_eq!(resp.lines, vec!["ERR usage: load <disc_name>".to_string()]);
}

// A truncated `sectors.bin` (not a whole multiple of SECTOR_SIZE) must not
// panic or silently round up: truncate toward whole sectors and log.
// Also covers the BDSM sparse-map branch (sums ranges, not raw length).
#[test]
fn disc_sector_count_covers_flat_truncation_and_sparse_sum() {
    let flat = crate::profile::DiscProfile {
        toc: Vec::new(),
        capacity: Vec::new(),
        disc_info: Vec::new(),
        disc_structures: std::collections::HashMap::new(),
        sector_data: Vec::new(),
        sectors: vec![0u8; SECTOR_SIZE * 2 + 100],
        sector_map: Vec::new(),
    };
    assert_eq!(
        disc_sector_count(&flat),
        2,
        "a non-multiple length must truncate toward whole sectors, not panic"
    );

    let sparse = crate::profile::DiscProfile {
        toc: Vec::new(),
        capacity: Vec::new(),
        disc_info: Vec::new(),
        disc_structures: std::collections::HashMap::new(),
        sector_data: Vec::new(),
        sectors: Vec::new(),
        sector_map: vec![(0, 10, 0), (100, 5, 0)],
    };
    assert_eq!(
        disc_sector_count(&sparse),
        15,
        "a BDSM sparse map must sum range counts, not divide the raw length"
    );
}

/// An immediate EOF (a peer that connects and closes without writing
/// anything) must get no response at all — not the unknown-command error a
/// spurious empty-string parse would otherwise produce.
#[test]
fn empty_connection_gets_no_response() {
    let resp = run_one(Vec::new());
    assert!(
        resp.is_empty(),
        "an immediate EOF must get no response, got: {resp:?}"
    );
}

/// `list-discs` must mark the currently loaded disc (and only it) with the
/// ` *` suffix. Nothing previously exercised the marker with more than one
/// candidate directory present.
#[test]
fn list_discs_marks_the_currently_loaded_disc() {
    let dir = test_scratch_dir("ctl_list_marker");
    std::fs::create_dir_all(dir.join("discs").join("alpha")).unwrap();
    std::fs::create_dir_all(dir.join("discs").join("beta")).unwrap();
    std::fs::write(
        dir.join("discs").join("beta").join("sectors.bin"),
        vec![0u8; SECTOR_SIZE],
    )
    .unwrap();

    let (_profile, state) = fixture_state();
    {
        let mut st = lock_recover(&state);
        st.profile_dir = dir.clone();
        st.disc = DiscState::Loaded(Some("beta".to_string()));
    }

    let resp = cmd_list_discs(&state);
    let body = resp.lines.join("\n");
    assert!(
        body.contains("beta * (sectors=true)"),
        "the loaded disc must carry the marker: {body}"
    );
    assert!(
        body.contains("alpha (sectors=false)"),
        "a non-loaded disc must not carry the marker: {body}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Serializes the two tests below, the only ones in this crate that mutate
/// `XDG_RUNTIME_DIR` / `BDEMU_INSTANCE` — process-global state that would
/// otherwise race against each other under parallel test threads.
static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

// `start_listener` with no usable socket path (XDG_RUNTIME_DIR unset) must
// log and return, not panic: it runs ahead of the `ioctl` catch_unwind,
// so a panic here would unwind across the C boundary.
#[test]
fn start_listener_disabled_when_socket_path_unavailable() {
    let _g = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let prev_runtime = std::env::var("XDG_RUNTIME_DIR").ok();
    unsafe {
        std::env::remove_var("XDG_RUNTIME_DIR");
    }

    let (profile, state) = fixture_state();
    start_listener(profile, state);

    unsafe {
        match prev_runtime {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
    }
}

// `start_listener` with a resolvable path but a live peer already on it
// must hit the `reclaim_socket_path` refusal, not try to bind. Skips the
// real bind/umask path: umask is process-global, would race other tests.
#[test]
fn start_listener_disabled_when_path_already_has_a_live_peer() {
    let _g = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let prev_runtime = std::env::var("XDG_RUNTIME_DIR").ok();
    let prev_instance = std::env::var(socket_name::INSTANCE_ENV).ok();

    let dir = test_scratch_dir("ctl_live2");
    unsafe {
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        std::env::set_var(socket_name::INSTANCE_ENV, "cov2");
    }

    let path = socket_name::socket_path().expect("path must resolve");
    // A live listener already occupying the resolved path, bound directly
    // (not via start_listener) so this test never touches the umask.
    let _live = UnixListener::bind(&path).expect("bind fixture listener");

    let (profile, state) = fixture_state();
    // Must log and return via reclaim_socket_path's refusal, not panic and
    // not disturb the pre-existing listener.
    start_listener(profile, state);
    assert!(
        UnixStream::connect(&path).is_ok(),
        "the pre-existing live listener must be untouched"
    );

    unsafe {
        match prev_runtime {
            Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
            None => std::env::remove_var("XDG_RUNTIME_DIR"),
        }
        match prev_instance {
            Some(v) => std::env::set_var(socket_name::INSTANCE_ENV, v),
            None => std::env::remove_var(socket_name::INSTANCE_ENV),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// `list-discs` writes directory names into a newline-delimited protocol the
// CLI prints to a terminal, and profiles come from strangers. Catches a
// dropped sanitiser: an ESC could repaint the screen, a newline forge a line.
#[test]
fn list_discs_sanitises_hostile_directory_names() {
    let dir = test_scratch_dir("ctl_list_discs");
    // A directory name carrying an escape sequence and a forged response line.
    let hostile = "evil\u{1b}[2J\nOK forged";
    std::fs::create_dir_all(dir.join("discs").join(hostile)).unwrap();

    let (_profile, state) = fixture_state();
    lock_recover(&state).profile_dir = dir.clone();

    let resp = cmd_list_discs(&state);
    let body = resp.lines.join("\n");
    assert!(
        !body.contains('\u{1b}'),
        "ESC must not reach the wire: {body:?}"
    );
    assert!(
        resp.lines.len() == 2,
        "a name with a newline must not forge extra lines: {:?}",
        resp.lines
    );

    let _ = std::fs::remove_dir_all(&dir);
}
