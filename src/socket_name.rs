// bdemu — Control socket path policy, shared between the cdylib (control.rs)
// and the CLI binary (bin.rs).
//
// The library is a `cdylib` and cannot be linked as an rlib, so the binary
// cannot import items from it directly. The socket-path policy therefore lives
// here once and both sides pull it in via `#[path = "socket_name.rs"]`, so the
// bind side and the connect side cannot drift apart — a drift would mean
// `bdemu load` silently talking to nothing (or, worse, to the wrong emulator).

use std::path::PathBuf;

/// Basename of the control socket for the default (single-instance) case.
pub const SOCKET_FILENAME: &str = "bdemu.sock";

/// Terminator line the emulator writes after a complete control-socket response,
/// and the CLI reads to confirm the reply was not cut short.
///
/// The control protocol is newline-delimited with no length prefix or terminator,
/// so a reply cut off by a read timeout (or a crashed emulator) after `OK\n` was
/// INDISTINGUISHABLE from a complete one: `bdemu list-discs` would print a partial
/// list and exit 0. The emulator now writes this sentinel as the final line of
/// every response and the CLI treats its ABSENCE as a truncation failure. It is a
/// bare `.` on its own line, which no real response line can equal — status/OK/ERR
/// lines carry a keyword prefix and every `list-discs` entry is indented with two
/// leading spaces. Lives here, in the module both the bind side (control.rs) and
/// the connect side (bin.rs) already share, so the two ends cannot disagree.
pub const CONTROL_TERMINATOR: &str = ".";

/// Environment variable naming this emulator instance.
///
/// The socket used to be a fixed `$XDG_RUNTIME_DIR/bdemu.sock` with no per-
/// instance identity, and `start_listener` unlinked it unconditionally before
/// binding. Two concurrent `bdemu run` invocations — two emulated drives, or two
/// jobs in a CI matrix sharing a runner — therefore collided: the second unlinked
/// the first's socket, the first kept accepting on an unlinked inode nobody could
/// reach, and every later `bdemu load` / `eject` / `status` was routed to the
/// second emulator. Nothing failed; the commands just addressed the wrong drive.
///
/// Setting `BDEMU_INSTANCE=<id>` gives each emulator its own socket
/// (`bdemu-<id>.sock`). The variable is read by BOTH the emulator (bind) and the
/// CLI (connect), and `bdemu run` passes the parent's environment to the child it
/// preloads, so exporting it once covers both sides. Unset — the overwhelmingly
/// common single-drive case — keeps the historical `bdemu.sock` path and the
/// zero-configuration UX.
pub const INSTANCE_ENV: &str = "BDEMU_INSTANCE";

/// Longest accepted instance id. The id becomes part of a filename in
/// `$XDG_RUNTIME_DIR`; 64 characters is far more than any human label needs and
/// keeps the result inside the shortest `sun_path` limit (108 bytes on Linux)
/// with room for the runtime-dir prefix.
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
mod tests {
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
}
