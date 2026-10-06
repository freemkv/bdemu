// bdemu — Control socket path policy, shared via `#[path = "socket_name.rs"]`
// between the cdylib and CLI binary so bind and connect sides can't drift
// apart — drift would mean `load` silently talking to nothing.

use std::path::PathBuf;

/// Basename of the control socket for the default (single-instance) case.
pub const SOCKET_FILENAME: &str = "bdemu.sock";

/// Terminator line the emulator writes after a complete control-socket response,
/// and the CLI reads to confirm the reply was not cut short.
///
/// A bare `.` on its own line, which no real response line can equal —
/// status/OK/ERR lines carry a keyword prefix and every `list-discs` entry is
/// indented with two leading spaces. Shared here by both the bind side
/// (control.rs) and the connect side (bin.rs) so the two ends cannot disagree.
pub const CONTROL_TERMINATOR: &str = ".";

/// Environment variable naming this emulator instance.
///
/// Setting `BDEMU_INSTANCE=<id>` gives each emulator its own socket
/// (`bdemu-<id>.sock`) instead of the shared default, so concurrent `bdemu run`
/// invocations don't collide. Read by BOTH the emulator (bind) and the CLI
/// (connect); `bdemu run` passes it to the child it preloads. Unset keeps the
/// historical `bdemu.sock` path and zero-configuration UX.
pub const INSTANCE_ENV: &str = "BDEMU_INSTANCE";

// Longest accepted instance id: the id becomes part of a filename in
// `$XDG_RUNTIME_DIR`, so 64 chars keeps the result inside the shortest
// `sun_path` limit (108 bytes on Linux) with room for the runtime-dir prefix.
const MAX_INSTANCE_LEN: usize = 64;

/// Socket basename for an instance id.
///
/// The id lands in a filename, so it must be a single plain path component:
/// anything else would let `BDEMU_INSTANCE=../../tmp/evil` place the socket
/// outside the private per-user runtime directory whose 0700 mode is the entire
/// reason we refuse the /tmp fallback below. Restrict to a conservative
/// alphanumeric/`-`/`_` set rather than blacklisting separators, and REJECT
/// (rather than sanitising) so the emulator and the CLI cannot silently disagree
/// about which socket a given id maps to.
pub fn socket_filename(instance: Option<&str>) -> Result<String, String> {
    match instance {
        // An exported-but-empty variable means "not configured", not "an instance
        // whose name is the empty string" — treat it as the default.
        None | Some("") => Ok(SOCKET_FILENAME.to_string()),
        Some(id) if id.len() > MAX_INSTANCE_LEN => Err(format!(
            "{} is too long ({} chars, max {})",
            INSTANCE_ENV,
            id.len(),
            MAX_INSTANCE_LEN
        )),
        Some(id)
            if id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
        {
            Ok(format!("bdemu-{}.sock", id))
        }
        Some(id) => Err(format!(
            "{} '{}' is not a valid instance id: use only ASCII letters, digits, '-' and '_'",
            INSTANCE_ENV, id
        )),
    }
}

/// Path to the control socket: the per-user runtime dir (mode 0700 on Linux) so
/// the socket is never world-accessible. The control socket accepts load/eject/
/// status commands, so a world-writable `/tmp/bdemu.sock` would let any local
/// user drive the emulator (and race a setup TOCTOU); we therefore REFUSE the
/// insecure /tmp fallback. If $XDG_RUNTIME_DIR is unset/empty, return an error
/// instructing the user to set it rather than silently binding in /tmp.
///
/// Pure in its inputs so the policy is unit-testable without mutating the
/// process environment.
pub fn socket_path_from(
    xdg_runtime_dir: Option<&str>,
    instance: Option<&str>,
) -> Result<PathBuf, String> {
    let filename = socket_filename(instance)?;
    match xdg_runtime_dir {
        Some(dir) if !dir.is_empty() => Ok(PathBuf::from(dir).join(filename)),
        _ => Err(
            "XDG_RUNTIME_DIR is unset or empty; refusing to create a world-accessible \
             control socket in /tmp. Set XDG_RUNTIME_DIR to a private per-user runtime \
             directory (e.g. /run/user/$(id -u)) and retry."
                .to_string(),
        ),
    }
}

/// Resolve the control-socket path from the process environment. Used by both
/// the emulator's bind side and the CLI's connect side.
pub fn socket_path() -> Result<PathBuf, String> {
    socket_path_from(
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        std::env::var(INSTANCE_ENV).ok().as_deref(),
    )
}

#[cfg(test)]
#[path = "socket_name_tests.rs"]
mod tests;
