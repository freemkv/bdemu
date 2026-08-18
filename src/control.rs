// bdemu — Control socket for runtime interaction
// MIT — freemkv project
//
// The LD_PRELOAD library listens on a Unix socket for commands.
// The CLI binary sends commands to control the running emulator.

use crate::profile::SECTOR_SIZE;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// The control-socket path policy (private per-user runtime dir, refusal of the
// insecure /tmp fallback, and the per-instance socket name) is shared with the
// CLI binary via a `#[path]` include so the bind side here and the connect side
// in bin.rs cannot drift apart.
#[path = "socket_name.rs"]
mod socket_name;
pub use socket_name::socket_path;

// Terminal-escape sanitiser for untrusted text we echo back to an operator's
// terminal, shared with the CLI binary through the same `#[path]` mechanism.
#[path = "sanitize.rs"]
mod sanitize;
use sanitize::sanitize_for_terminal;

/// Commands the CLI can send to the running emulator.
#[derive(Debug)]
pub enum Command {
    Status,
    Eject,
    Load(String),
    ListDiscs,
}

/// Response from the emulator.
#[derive(Debug)]
pub struct Response {
    pub lines: Vec<String>,
}

impl Response {
    pub fn ok(msg: &str) -> Self {
        Response {
            lines: vec![format!("OK {}", msg)],
        }
    }
    pub fn error(msg: &str) -> Self {
        Response {
            lines: vec![format!("ERR {}", msg)],
        }
    }
    pub fn multi(lines: Vec<String>) -> Self {
        Response { lines }
    }
}

/// Whether a disc is loaded, and if so its name. Modeled as a single enum so the
/// "loaded" flag and the disc name cannot drift out of sync (they were
/// previously a `disc_loaded: bool` + `disc_name: Option<String>` pair kept
/// consistent by hand in three places). `Loaded(None)` preserves the one
/// intentional case where a disc is present but unnamed — startup with a disc
/// captured into the profile but `BDEMU_DISC` unset — which cmd_status renders
/// as "loaded (unknown)".
#[derive(Debug, Clone)]
pub enum DiscState {
    Empty,
    Loaded(Option<String>),
}

impl DiscState {
    /// The loaded disc's name, if it has one. `None` for both Empty and the
    /// unnamed-loaded case — callers needing to distinguish those use the enum.
    pub fn name(&self) -> Option<&str> {
        match self {
            DiscState::Loaded(Some(name)) => Some(name.as_str()),
            _ => None,
        }
    }
}

/// Shared state between the SCSI handler and the control socket.
pub struct EmulatorState {
    pub profile_dir: PathBuf,
    pub disc: DiscState,
}

/// Make `path` free to bind, WITHOUT stealing a socket somebody is using.
///
/// The unconditional `let _ = std::fs::remove_file(&path)` this replaces was how
/// two concurrent `bdemu run` instances (two emulated drives, or two CI-matrix
/// jobs sharing a runner) silently took each other's control socket: the second
/// emulator unlinked the first's inode, the first went on accepting on a socket
/// no path pointed at any more, and every later `bdemu load` / `eject` / `status`
/// reached the SECOND emulator and answered OK. Nothing failed, so nothing was
/// noticed — the disc the operator thought they loaded was loaded into the other
/// drive.
///
/// A socket with a live listener answers `connect(2)`; one left behind by a
/// crashed emulator answers ECONNREFUSED. So: refuse loudly on a live peer (and
/// point the operator at `BDEMU_INSTANCE` for running a second emulator), and
/// only reclaim a socket that is provably dead. This is detect-and-refuse, not a
/// lock: a competing instance could still bind between our probe and our bind,
/// but that residual race fails loudly at `bind` (EADDRINUSE) instead of stealing
/// silently, and the collision it does close — an emulator already running — is
/// the one that actually happens.
fn reclaim_socket_path(path: &std::path::Path) -> Result<(), String> {
    if std::fs::symlink_metadata(path).is_err() {
        // Nothing there: the ordinary first-instance case.
        return Ok(());
    }
    if UnixStream::connect(path).is_ok() {
        return Err(format!(
            "{} is already in use by a running emulator; refusing to steal it. \
             Set {}=<id> to give this instance its own socket.",
            path.display(),
            socket_name::INSTANCE_ENV
        ));
    }
    // Nothing is listening: a stale socket (or a leftover non-socket file).
    // Reclaim it, but surface an unlink failure rather than letting it resurface
    // as a confusing EADDRINUSE from bind.
    std::fs::remove_file(path).map_err(|e| format!("cannot remove stale {}: {}", path.display(), e))
}

/// Start the control socket listener in a background thread.
pub fn start_listener(
    profile: Arc<Mutex<crate::profile::LoadedProfile>>,
    state: Arc<Mutex<EmulatorState>>,
) {
    let path = match socket_path() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("bdemu: control socket disabled: {}", e);
            return;
        }
    };

    if let Err(e) = reclaim_socket_path(&path) {
        eprintln!("bdemu: control socket disabled: {}", e);
        return;
    }

    // Tighten umask to 0o177 around bind so the socket is created owner-only
    // (0600) from the start. set_permissions after bind would still leave a
    // brief world-accessible window during which a local peer could connect and
    // issue load/eject/status — the TOCTOU this closes. The socket lives in the
    // per-user XDG_RUNTIME_DIR (0700) so it is not world-reachable anyway, but
    // 0600 keeps it owner-only for defense in depth. umask is process-global, so
    // restore the previous value immediately after.
    #[cfg(unix)]
    let prev_umask = unsafe { libc::umask(0o177) };

    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            #[cfg(unix)]
            unsafe {
                libc::umask(prev_umask);
            }
            eprintln!("bdemu: control socket failed: {}", e);
            return;
        }
    };

    #[cfg(unix)]
    unsafe {
        libc::umask(prev_umask);
    }

    // Belt-and-suspenders: also chmod 0600 after bind in case a platform
    // ignored the umask for the socket inode.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }

    eprintln!("bdemu: control socket at {}", path.display());

    // Spawn the accept loop on a named thread via Builder rather than
    // thread::spawn: spawn() panics if the OS refuses the thread (e.g. EAGAIN
    // at RLIMIT_NPROC), and start_listener is forced from the STATE Lazy init
    // that runs BEFORE the catch_unwind in the exported `extern "C" fn ioctl`.
    // A panic here would unwind across the C boundary (UB under panic=unwind),
    // so degrade like the socket-bind failure above: log and return.
    let accept = std::thread::Builder::new()
        .name("bdemu-ctl-accept".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    // Handle each connection on its own thread. cmd_load reads
                    // sectors.bin (multi-GB) off-lock before its O(1) under-lock
                    // swap; if that read ran inline in the accept loop, no new
                    // connection would be accepted for its whole duration and a
                    // concurrent `status`/`eject`/`list-discs` (5s CLI read timeout)
                    // would time out. The shared state is Arc<Mutex<_>>, so
                    // per-connection threads are safe and the lone serialized
                    // section stays the brief swap inside cmd_load.
                    Ok(stream) => {
                        let profile = Arc::clone(&profile);
                        let state = Arc::clone(&state);
                        // Builder again, not thread::spawn: a spawn panic on
                        // thread exhaustion would kill the accept loop. Degrade
                        // instead — log and drop this one connection, keep
                        // accepting. Do NOT fall back to inline handling, which
                        // would re-block the accept loop for the full cmd_load
                        // read window.
                        if let Err(e) = std::thread::Builder::new()
                            .name("bdemu-ctl-conn".into())
                            .spawn(move || handle_client(stream, &profile, &state))
                        {
                            eprintln!("bdemu: control connection thread failed: {e}");
                        }
                    }
                    Err(e) => eprintln!("bdemu: socket error: {}", e),
                }
            }
        });
    if let Err(e) = accept {
        eprintln!("bdemu: control listener thread failed: {e}");
    }
}

fn handle_client(
    stream: UnixStream,
    profile: &Arc<Mutex<crate::profile::LoadedProfile>>,
    state: &Arc<Mutex<EmulatorState>>,
) {
    // Each connection runs on its own thread, so a peer that connects but never
    // sends a newline blocks only its own thread — not the listener. But without
    // a bound such threads accumulate without limit, one per stalled peer, until
    // the process exhausts threads/memory. Cap each read with a timeout, and
    // surface the failure: if set_read_timeout fails this protection silently does
    // not exist, so make it visible rather than discarding the Result.
    if stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .is_err()
    {
        eprintln!("bdemu: failed to set control-socket read timeout");
    }
    // Symmetric write timeout: a client that connects then stops draining its
    // receive buffer would otherwise block writeln! forever, leaving this thread
    // alive indefinitely. Payloads are tiny so this is low-probability, but the
    // rationale matches the read timeout — bound the per-thread lifetime so a
    // single misbehaving peer cannot leak a thread.
    if stream
        .set_write_timeout(Some(std::time::Duration::from_secs(5)))
        .is_err()
    {
        eprintln!("bdemu: failed to set control-socket write timeout");
    }

    // One command per connection: read a single line, respond, drop the socket.
    // Cap the read at 1 KiB so a local peer cannot force unbounded heap growth by
    // slowly streaming bytes without a newline — every chunk resets the 5s read
    // timeout, so the timeout bounds the inter-read gap, not the total length. All
    // valid commands are tiny ("status\n", "load <name>\n", etc.), so 1 KiB is a
    // generous ceiling. take() simply stops yielding at the limit; an over-long
    // line gets truncated and then falls through to the normal command-parse path,
    // which rejects it as an unknown command — no panic, no OOM.
    let mut reader = BufReader::new(&stream).take(1024);
    let mut line = String::new();
    // Ok(0) is a clean EOF: a peer that connects and immediately closes yields
    // an empty line, and parse_command("") -> None would otherwise write a
    // spurious "ERR unknown command:" to a socket the peer already closed.
    // Treat EOF and read errors alike: respond to neither, log nothing.
    match reader.read_line(&mut line) {
        Ok(0) | Err(_) => return,
        _ => {}
    }
    let line = line.trim();

    let response = match parse_command(line) {
        Some(Command::Status) => cmd_status(state),
        Some(Command::Eject) => cmd_eject(profile, state),
        Some(Command::Load(name)) => cmd_load(profile, state, &name),
        Some(Command::ListDiscs) => cmd_list_discs(state),
        None => Response::error(&format!("unknown command: {}", line)),
    };

    let mut writer = stream;
    for line in &response.lines {
        // Break on first write failure: once the peer has closed mid-response,
        // every remaining writeln! just fails (bounded only by the 5s write
        // timeout per line). Stop instead of churning through the rest.
        if writeln!(writer, "{}", line).is_err() {
            break;
        }
    }
}

fn parse_command(line: &str) -> Option<Command> {
    let parts: Vec<&str> = line.splitn(2, ' ').collect();
    match parts[0] {
        "status" => Some(Command::Status),
        "eject" => Some(Command::Eject),
        "load" => Some(Command::Load(parts.get(1).unwrap_or(&"").to_string())),
        "list-discs" => Some(Command::ListDiscs),
        _ => None,
    }
}

/// Lock recovering from poison: a panic in a prior lock holder must not wedge
/// the control socket (every subsequent `.unwrap()` would panic and kill the
/// listener thread, silently dropping all commands).
fn lock_recover<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn cmd_status(state: &Arc<Mutex<EmulatorState>>) -> Response {
    let st = lock_recover(state);
    let disc_status = match &st.disc {
        DiscState::Loaded(name) => {
            format!("loaded ({})", name.as_deref().unwrap_or("unknown"))
        }
        DiscState::Empty => "empty".to_string(),
    };
    Response::multi(vec![
        "OK".to_string(),
        format!("profile: {}", st.profile_dir.display()),
        format!("disc: {}", disc_status),
    ])
}

fn cmd_eject(
    profile: &Arc<Mutex<crate::profile::LoadedProfile>>,
    state: &Arc<Mutex<EmulatorState>>,
) -> Response {
    let mut prof = lock_recover(profile);
    let mut st = lock_recover(state);

    prof.disc = None;
    st.disc = DiscState::Empty;

    // Signal media change to SCSI layer
    crate::scsi::set_media_changed(true);

    Response::ok("ejected")
}

fn cmd_load(
    profile: &Arc<Mutex<crate::profile::LoadedProfile>>,
    state: &Arc<Mutex<EmulatorState>>,
    name: &str,
) -> Response {
    if name.is_empty() {
        return Response::error("usage: load <disc_name>");
    }

    // Validate + resolve the disc directory under-lock so a concurrent
    // rename/symlink swap cannot slip between the canonicalize-based containment
    // check and our decision to load it. The name comes from an untrusted
    // control-socket peer, so the path-traversal containment is shared with the
    // BDEMU_DISC env path via the single profile::safe_disc_dir guard.
    let disc_dir = {
        let st = lock_recover(state);
        let discs_base = st.profile_dir.join("discs");
        match crate::profile::safe_disc_dir(&discs_base, name) {
            Some(dir) => dir,
            None => {
                // Either an invalid (non-component) name or a name that does not
                // resolve to an existing contained directory.
                return Response::error("invalid disc name");
            }
        }
    };

    // Read the disc OFF-lock: load_disc issues fs::read for sectors.bin (which
    // can be gigabytes), toc.bin, capacity.bin, disc_info.bin, sector_data.bin
    // and each ds_*.bin. The shared state is Arc<Mutex<_>>, so holding the lock
    // across that read would block every concurrent command thread (status/eject/
    // list-discs) and any SCSI path that locks the same mutexes for the whole read
    // window. Load into a local, then re-acquire and swap in O(1).
    let disc = crate::profile::load_disc(&disc_dir);
    let sector_count = disc_sector_count(&disc);

    {
        let mut prof = lock_recover(profile);
        let mut st = lock_recover(state);
        prof.disc = Some(disc);
        st.disc = DiscState::Loaded(Some(name.to_string()));
        // Signal media change to SCSI layer while holding the locks.
        crate::scsi::set_media_changed(true);
    }

    Response::ok(&format!("loaded '{}' ({} sectors)", name, sector_count))
}

/// Logical sector count of a loaded disc. For Blu-ray Disc Sector Map (BDSM)
/// sparse images `sectors` also holds the 12-byte header + range table, so
/// dividing the raw length by 2048 over-counts; sum the range counts instead.
/// Flat images have an empty map and fall back to length/2048.
fn disc_sector_count(d: &crate::profile::DiscProfile) -> usize {
    if d.sector_map.is_empty() {
        if !d.sectors.len().is_multiple_of(SECTOR_SIZE) {
            eprintln!(
                "bdemu: sectors.bin length {} is not a multiple of {} (truncated capture?)",
                d.sectors.len(),
                SECTOR_SIZE
            );
        }
        d.sectors.len() / SECTOR_SIZE
    } else {
        d.sector_map.iter().map(|(_, c, _)| *c as usize).sum()
    }
}

fn cmd_list_discs(state: &Arc<Mutex<EmulatorState>>) -> Response {
    // Copy the two values we need out of the guard and drop it BEFORE touching
    // the filesystem. The shared state is Arc<Mutex<_>>, so holding the state lock
    // across read_dir + entry iteration would stall every concurrent command
    // thread that locks the same mutex (status, eject, and load's validation
    // phase) for the full directory scan. Mirrors cmd_load.
    let (discs_dir, loaded_name) = {
        let st = lock_recover(state);
        (
            st.profile_dir.join("discs"),
            st.disc.name().map(str::to_string),
        )
    };

    if !discs_dir.is_dir() {
        return Response::multi(vec!["OK".into(), "no discs directory".into()]);
    }

    let mut lines = vec!["OK".to_string()];
    if let Ok(entries) = std::fs::read_dir(&discs_dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let name = entry.file_name().to_string_lossy().to_string();
                let has_sectors = entry.path().join("sectors.bin").exists();
                let marker = if Some(&name) == loaded_name.as_ref() {
                    " *"
                } else {
                    ""
                };
                // The directory name is untrusted: profiles arrive from strangers
                // through the documented GitHub-issue workflow, and this line is
                // written to the control socket for the CLI to print verbatim to a
                // terminal. An ESC in a disc directory name would otherwise repaint
                // or erase the operator's screen, and an embedded newline would
                // forge extra response lines in a newline-delimited protocol —
                // including a fake leading "OK". Compare on the RAW name (that is
                // what `load` addresses) but display the sanitised form.
                lines.push(format!(
                    "  {}{} (sectors={})",
                    sanitize_for_terminal(&name),
                    marker,
                    has_sectors
                ));
            }
        }
    }

    Response::multi(lines)
}

#[cfg(test)]
mod tests {
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

        // Write on a side thread that tolerates a broken pipe: once the server
        // hits its 1 KiB read cap it responds and drops the socket, so a client
        // still pushing a larger payload sees EPIPE. That is the cap working, not
        // a test failure — ignore the write result and just read the response.
        let mut writer = client.try_clone().expect("clone");
        let pusher = std::thread::spawn(move || {
            let _ = writer.write_all(&input);
            let _ = writer.shutdown(std::net::Shutdown::Write);
        });

        // Read tolerantly: once the server hits its 1 KiB cap it writes the
        // response and drops the socket, so the client's read can surface
        // ECONNRESET right after the bytes arrive. `read_to_string` discards
        // everything on any error (it restores the buffer to keep UTF-8 valid),
        // which would lose the response and fail the test on a timing race;
        // `read_to_end` retains the bytes read before the error. Decode lossily.
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

    /// Catches the mutation that restores the unconditional
    /// `let _ = std::fs::remove_file(&path)` before bind: a second emulator would
    /// unlink a LIVE socket, leaving the first emulator accepting on an inode no
    /// path reaches while every later `bdemu load`/`eject` silently addressed the
    /// second one.
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

    /// End-to-end coverage of the two mutating control verbs over a real socket
    /// pair, which nothing exercised before: `load` must resolve a disc under the
    /// profile's `discs/` directory, swap it into the live profile, report the
    /// sector count and arm the media-change flag the SCSI layer reports as a UNIT
    /// ATTENTION; `eject` must clear it again. Catches a `load` that answers OK
    /// without actually swapping the disc in (the state and the profile going out
    /// of sync is exactly the drift `DiscState` exists to prevent).
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

    /// `list-discs` writes directory names into a newline-delimited protocol that
    /// the CLI prints straight to a terminal, and profiles come from strangers.
    /// Catches the mutation that drops the sanitiser: an ESC in a disc directory
    /// name would repaint the operator's screen, and an embedded newline would
    /// forge an extra response line.
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
}
