// bdemu — Control socket for runtime interaction
// AGPL-3.0 — freemkv project
//
// The LD_PRELOAD library listens on a Unix socket for commands.
// The CLI binary sends commands to control the running emulator.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const SOCKET_PATH: &str = "/tmp/bdemu.sock";

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
        Response { lines: vec![format!("OK {}", msg)] }
    }
    pub fn error(msg: &str) -> Self {
        Response { lines: vec![format!("ERR {}", msg)] }
    }
    pub fn multi(lines: Vec<String>) -> Self {
        Response { lines }
    }
}

/// Shared state between the SCSI handler and the control socket.
pub struct EmulatorState {
    pub profile_dir: PathBuf,
    pub disc_name: Option<String>,
    pub disc_loaded: bool,
}

/// Start the control socket listener in a background thread.
pub fn start_listener(
    profile: Arc<Mutex<crate::profile::LoadedProfile>>,
    state: Arc<Mutex<EmulatorState>>,
) {
    // Clean up stale socket
    let _ = std::fs::remove_file(SOCKET_PATH);

    let listener = match UnixListener::bind(SOCKET_PATH) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bdemu: control socket failed: {}", e);
            return;
        }
    };

    eprintln!("bdemu: control socket at {}", SOCKET_PATH);

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => handle_client(stream, &profile, &state),
                Err(e) => eprintln!("bdemu: socket error: {}", e),
            }
        }
    });
}

fn handle_client(
    stream: UnixStream,
    profile: &Arc<Mutex<crate::profile::LoadedProfile>>,
    state: &Arc<Mutex<EmulatorState>>,
) {
    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
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
        let _ = writeln!(writer, "{}", line);
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

fn cmd_status(state: &Arc<Mutex<EmulatorState>>) -> Response {
    let st = state.lock().unwrap();
    let disc_status = if st.disc_loaded {
        format!("loaded ({})", st.disc_name.as_deref().unwrap_or("unknown"))
    } else {
        "empty".to_string()
    };
    Response::multi(vec![
        format!("OK"),
        format!("profile: {}", st.profile_dir.display()),
        format!("disc: {}", disc_status),
    ])
}

fn cmd_eject(
    profile: &Arc<Mutex<crate::profile::LoadedProfile>>,
    state: &Arc<Mutex<EmulatorState>>,
) -> Response {
    let mut prof = profile.lock().unwrap();
    let mut st = state.lock().unwrap();

    prof.disc = None;
    st.disc_loaded = false;
    st.disc_name = None;

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

    let st = state.lock().unwrap();
    let disc_dir = st.profile_dir.join("discs").join(name);

    if !disc_dir.is_dir() {
        return Response::error(&format!("disc not found: {}", disc_dir.display()));
    }
    drop(st);

    // Load disc data
    let disc = crate::profile::load_disc(&disc_dir);

    let mut prof = profile.lock().unwrap();
    let mut st = state.lock().unwrap();

    prof.disc = Some(disc);
    st.disc_loaded = true;
    st.disc_name = Some(name.to_string());

    // Signal media change to SCSI layer
    crate::scsi::set_media_changed(true);

    let sector_count = prof.disc.as_ref()
        .map(|d| d.sectors.len() / 2048)
        .unwrap_or(0);

    Response::ok(&format!("loaded '{}' ({} sectors)", name, sector_count))
}

fn cmd_list_discs(state: &Arc<Mutex<EmulatorState>>) -> Response {
    let st = state.lock().unwrap();
    let discs_dir = st.profile_dir.join("discs");

    if !discs_dir.is_dir() {
        return Response::multi(vec!["OK".into(), "no discs directory".into()]);
    }

    let mut lines = vec!["OK".to_string()];
    if let Ok(entries) = std::fs::read_dir(&discs_dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let name = entry.file_name().to_string_lossy().to_string();
                let has_sectors = entry.path().join("sectors.bin").exists();
                let marker = if Some(&name) == st.disc_name.as_ref() { " *" } else { "" };
                lines.push(format!("  {}{} (sectors={})", name, marker, has_sectors));
            }
        }
    }

    Response::multi(lines)
}
