// bdemu — Control socket filename, shared between the cdylib (control.rs) and
// the CLI binary (bin.rs).
//
// The library is a `cdylib` and cannot be linked as an rlib, so the binary
// cannot import items from it directly. The socket-path *policy* is therefore
// restated in both files, but the filename is the one field that could
// realistically drift between them, so it lives here once and both sides pull
// it in via `#[path = "socket_name.rs"]`.

/// Basename of the control socket, joined onto $XDG_RUNTIME_DIR in both
/// `control::socket_path` (the emulator's bind side) and `bin::socket_path`
/// (the CLI's connect side).
pub const SOCKET_FILENAME: &str = "bdemu.sock";
