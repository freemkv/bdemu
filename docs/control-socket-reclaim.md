# Control socket reclaim rationale

`reclaim_socket_path` (src/control.rs) replaces an unconditional
`let _ = std::fs::remove_file(&path)`.

That unconditional unlink was how two concurrent `bdemu run` instances (two
emulated drives, or two CI-matrix jobs sharing a runner) silently took each
other's control socket: the second emulator unlinked the first's inode, the
first went on accepting on a socket no path pointed at any more, and every
later `bdemu load` / `eject` / `status` reached the SECOND emulator and
answered OK. Nothing failed, so nothing was noticed — the disc the operator
thought they loaded was loaded into the other drive.

A socket with a live listener answers `connect(2)`; one left behind by a
crashed emulator answers ECONNREFUSED. So: refuse loudly on a live peer (and
point the operator at `BDEMU_INSTANCE` for running a second emulator), and
only reclaim a socket that is provably dead. This is detect-and-refuse, not a
lock: a competing instance could still bind between our probe and our bind,
but that residual race fails loudly at `bind` (EADDRINUSE) instead of
stealing silently, and the collision it does close — an emulator already
running — is the one that actually happens.
