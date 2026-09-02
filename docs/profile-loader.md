# Profile blob loading — design notes

## `read_bin_reported` (src/profile.rs)

Returns `Ok(empty)` for a blob that is genuinely ABSENT — callers treat an
empty Vec as "this feature is not present in the profile", and profiles
legitimately omit rpc_state.bin, mode_2a.bin, sector_data.bin and so on.
Every OTHER failure (EACCES, EIO, a directory where a file was expected, an
oversized blob) returns `Err` with a message the caller logs.

That split is the whole point. The previous `_ => fs::read(path)
.unwrap_or_default()` collapsed EVERY error into an empty Vec with no log, so
an unreadable `discs/<n>/sectors.bin` was indistinguishable from a profile
that simply has no sectors — and READ(10) then served 2048 zero bytes per
sector at GOOD status. A rip could complete "successfully" against a profile
the process could not actually read. Only the >16 GiB case was ever logged.

Stat first so an oversized (or hostile) file is rejected before the
allocation, rather than `fs::read` growing an unbounded Vec to OOM.

## `read_blob` (src/profile.rs)

Reads a profile blob whose FILENAME came from the (untrusted) `drive.toml`,
enforcing that it names a file directly inside the profile directory.

`drive.toml`'s `[files]`, `[features]` and `[read_buffer]` values are
filenames chosen by whoever authored the profile — and profiles are
THIRD-PARTY UNTRUSTED artifacts shared through the documented GitHub-issue
workflow (SCHEMA.md / profile-from-issue.yml turns an issue body into one of
these directories). The previous `read_bin(&dir.join(&name))` joined them raw,
so a hostile profile could set `inquiry = "../../../../etc/shadow"`
(or any absolute path) and bdemu would read that file and serve its bytes back
as the emulated INQUIRY / feature / read-buffer response — an arbitrary
local-file read surfaced over SCSI and into the logs. Round 1 added
`safe_disc_dir` containment for the disc DIRECTORY name but left this
blob-name sibling raw; this is the same containment for it.

A profile blob is always a plain filename beside `drive.toml` (SCHEMA.md shows
flat `inquiry.bin`, `gc_0108.bin`, … and the loader itself writes fixed
basenames such as `mode_2a.bin`), so a name that is empty, absolute, contains
a separator/NUL, or is a `.`/`..` component is rejected. A rejected name is
LOGGED (absence of a log is a bug) and mapped to the empty/"not present" blob
every caller already understands — it is never opened.

## `toml_loader_covers_malformed_entries_and_positive_blob_loads` test

One `drive.toml` exercising every remaining loader branch that
`profile_loads_with_every_blob_missing` and
`blob_filenames_cannot_escape_the_profile_directory` do not: comment/blank
lines, an invalid `current_profile` (kept at default, not coerced to
0x0000), an invalid `[features]` key (skipped, not coerced to 0x0000), an
invalid `[read_buffer]` key (skipped, not coerced to id 0), the no-op
`[unlock]` section, an unrecognised section (`_ => {}`), a *present*
feature/read_buffer blob (the positive load path, not just the
missing-file path), no `rpc_state` key at all (the `Vec::new()` default,
not the read-and-empty path), and a loose `rb_*.bin` file picked up by the
directory scan even though it is not listed in `drive.toml`.
