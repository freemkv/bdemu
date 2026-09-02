# Control socket naming policy

## `CONTROL_TERMINATOR`

The control protocol is newline-delimited with no length prefix or terminator,
so a reply cut off by a read timeout (or a crashed emulator) after `OK\n` was
INDISTINGUISHABLE from a complete one: `bdemu list-discs` would print a partial
list and exit 0. The emulator now writes this sentinel as the final line of
every response and the CLI treats its ABSENCE as a truncation failure. It is a
bare `.` on its own line, which no real response line can equal — status/OK/ERR
lines carry a keyword prefix and every `list-discs` entry is indented with two
leading spaces. Lives here, in the module both the bind side (control.rs) and
the connect side (bin.rs) already share, so the two ends cannot disagree.

## `INSTANCE_ENV`

The socket used to be a fixed `$XDG_RUNTIME_DIR/bdemu.sock` with no per-
instance identity, and `start_listener` unlinked it unconditionally before
binding. Two concurrent `bdemu run` invocations — two emulated drives, or two
jobs in a CI matrix sharing a runner — therefore collided: the second unlinked
the first's socket, the first kept accepting on an unlinked inode nobody could
reach, and every later `bdemu load` / `eject` / `status` was routed to the
second emulator. Nothing failed; the commands just addressed the wrong drive.

Setting `BDEMU_INSTANCE=<id>` gives each emulator its own socket
(`bdemu-<id>.sock`). The variable is read by BOTH the emulator (bind) and the
CLI (connect), and `bdemu run` passes the parent's environment to the child it
preloads, so exporting it once covers both sides. Unset — the overwhelmingly
common single-drive case — keeps the historical `bdemu.sock` path and the
zero-configuration UX.
