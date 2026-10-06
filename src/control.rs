// bdemu — Control socket for runtime interaction, MIT — freemkv project
// The LD_PRELOAD library listens on a Unix socket for commands; the CLI
// binary sends commands to control the running emulator.

use crate::profile::SECTOR_SIZE;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

// The control-socket path policy (private runtime dir, refusal of the /tmp
// fallback, per-instance name) is shared with the CLI via a `#[path]` include
// so the bind side here and the connect side in bin.rs cannot drift apart.
#[path = "socket_name.rs"]
mod socket_name;
pub use socket_name::socket_path;

// Terminal-escape sanitiser for untrusted text we echo back to an operator's
// terminal; the lib crate owns the module (crate::sanitize), the CLI binary
// pulls the same file in via its own `#[path]`.
use crate::sanitize::sanitize_for_terminal;

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

// Make `path` free to bind, WITHOUT stealing a socket somebody is using: refuse loudly on a
// live peer, only reclaim a provably dead one.
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
    // (0600) from the start — set_permissions after bind would leave a brief
    // world-accessible TOCTOU window. umask is process-global; restore after.
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

    // Builder, not thread::spawn: spawn() panics if the OS refuses the thread,
    // and start_listener runs before the catch_unwind in the exported
    // `extern "C" fn ioctl` — a panic here would unwind across the C boundary.
    let accept = std::thread::Builder::new()
        .name("bdemu-ctl-accept".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    // Each connection gets its own thread: cmd_load reads
                    // sectors.bin (multi-GB) off-lock, so running it inline here
                    // would stall every concurrent status/eject/list-discs call.
                    Ok(stream) => {
                        let profile = Arc::clone(&profile);
                        let state = Arc::clone(&state);
                        // Builder again: a spawn panic on thread exhaustion
                        // would kill the accept loop. Degrade — log and drop
                        // this connection, keep accepting (not inline handling).
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
    // A stalled peer blocks only its own thread, but without a bound such
    // threads accumulate until the process exhausts threads/memory. Cap each
    // read with a timeout, and log if set_read_timeout itself fails.
    if stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .is_err()
    {
        eprintln!("bdemu: failed to set control-socket read timeout");
    }
    // Symmetric write timeout: a client that stops draining its receive buffer
    // would otherwise block writeln! forever. Low-probability given tiny
    // payloads, but bounds the per-thread lifetime like the read timeout.
    if stream
        .set_write_timeout(Some(std::time::Duration::from_secs(5)))
        .is_err()
    {
        eprintln!("bdemu: failed to set control-socket write timeout");
    }

    // One command per connection. Cap the read at 1 KiB so slow byte-at-a-time
    // streaming can't grow heap unbounded (the timeout only bounds the inter-read
    // gap). take() truncates; an over-long line just falls to unknown-command.
    let mut reader = BufReader::new(&stream).take(1024);
    let mut line = String::new();
    // Ok(0) is a clean EOF: parse_command("") -> None would otherwise write a
    // spurious error to a socket the peer already closed. Treat EOF and read
    // errors alike: respond to neither, log nothing.
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
    let mut wrote_all = true;
    for line in &response.lines {
        // Break on first write failure: once the peer has closed mid-response,
        // every remaining writeln! just fails (bounded only by the 5s write
        // timeout per line). Stop instead of churning through the rest.
        if writeln!(writer, "{}", line).is_err() {
            wrote_all = false;
            break;
        }
    }
    // Terminate with the shared sentinel so the CLI can tell a complete reply
    // from one cut short (see CONTROL_TERMINATOR). Only emit it when every line
    // wrote successfully — withholding it is how the CLI detects truncation.
    if wrote_all {
        let _ = writeln!(writer, "{}", socket_name::CONTROL_TERMINATOR);
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

    // Resolve the disc directory under-lock so a concurrent rename/symlink
    // swap can't slip in between the containment check and loading it. The
    // name is untrusted, so containment reuses the safe_disc_dir guard.
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

    // Read the disc OFF-lock: load_disc reads sectors.bin (can be gigabytes)
    // plus other files, and holding the mutex across that would block every
    // concurrent command thread. Load into a local, then swap in O(1).
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

// Logical sector count of a loaded disc. BDSM sparse images' `sectors` also
// holds a 12-byte header + range table, so raw length/2048 over-counts; sum
// the range counts instead. Flat images have an empty map, use length/2048.
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
    // Copy what we need out of the guard and drop it before touching the
    // filesystem: holding the lock across read_dir + iteration would stall
    // every concurrent command thread for the full directory scan.
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

    let entries = match std::fs::read_dir(&discs_dir) {
        Ok(entries) => entries,
        // The dir exists (checked above) but couldn't be enumerated (perms,
        // race with removal): that's not "no discs", so report it distinctly
        // instead of the bare OK a swallowed error would have produced.
        Err(e) => return Response::error(&format!("could not enumerate discs: {}", e)),
    };

    let mut lines = vec!["OK".to_string()];
    for entry in entries.flatten() {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let name = entry.file_name().to_string_lossy().to_string();
            let has_sectors = entry.path().join("sectors.bin").exists();
            let marker = if Some(&name) == loaded_name.as_ref() {
                " *"
            } else {
                ""
            };
            // Untrusted name printed to a terminal: an ESC could repaint the
            // screen, a newline could forge a response line. Compare on the
            // RAW name (what `load` addresses) but display the sanitised form.
            lines.push(format!(
                "  {}{} (sectors={})",
                sanitize_for_terminal(&name),
                marker,
                has_sectors
            ));
        }
    }

    Response::multi(lines)
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
