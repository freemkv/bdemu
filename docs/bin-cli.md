# bdemu CLI (`src/bin.rs`) — internal notes

Design rationale for internal helpers that back the `bdemu` CLI, kept out of
the source so inline comments stay within the project's comment-guard caps.

## `flag_value`

The value a flag requires, or `None` if it is missing or looks like another
flag. The `!v.starts_with('-')` guard is the point: the old
`args.get(i).cloned()` silently swallowed the following token, so
`bdemu run --profile --disc x` set profile="--disc" and then complained about
a missing command — a confusing diagnostic for a simple typo. Kept pure (no
exit, no printing) so `parse_run_args` can report the specific flag and the
guard is unit-testable.

## `parse_run_args`

Scan `bdemu run` arguments (from index 2) into a `RunArgs`. Returns
`Err(flag_name)` when a value-taking flag is missing its value, so the caller
can print the diagnostic and exit — the scan itself neither prints nor exits,
which is what lets it be unit-tested. Behaviour matches the previous inline
loop exactly: `--`/first-positional stop the scan, and a flag whose value
looks like another flag is a missing-value error.

## `exit_code_for`

Map a finished child's status to a process exit code. A child killed by a
signal has no exit code (`status.code()` is None); reporting that as a plain
`1` makes a crash (SIGSEGV, SIGABRT) indistinguishable from an ordinary
non-zero exit. Follow the shell convention and return `128 + signum` so CI can
tell a crash from a normal failure, falling back to 1 only if neither is set.

## `BlobState`

State of a per-disc blob (`toc.bin`, `sectors.bin`) as `validate` reports it.

`validate` used to answer this question with a bare `Path::exists()` and print
`sectors=true`. A `sectors.bin` left behind by an interrupted capture — the
file created, the process killed before a byte was written — exists, so the
profile validated clean and `bdemu validate` exited 0 on a fixture the
emulator cannot serve a single sector from. Size is the thing that matters, so
report it, and treat a zero-byte blob as a failure rather than a success.

A blob that is genuinely ABSENT stays non-fatal: a metadata-only disc fixture
(TOC/capacity but no captured sectors) is a legitimate thing to have, and the
emulator now answers reads against it with CHECK CONDITION rather than
pretending zeros are content.

## `validate_profile`

Validate a profile directory, printing a report. Returns `true` when the
profile is complete enough to emulate (`bdemu validate` exits 0) and `false`
when a required file is missing/empty/unreadable (exit 1). The exit itself
lives in the caller so the pass/fail decision is unit-testable without
terminating the test process — a CI profile gate depends on this returning
false for a broken profile, and nothing exercised that before.

## `send_control`

Send one control-socket command and relay the reply.

`slow_read` selects the read timeout: fast commands (status/eject/list-discs)
reply instantly and use a short read timeout so a hung emulator can't stall
the CLI forever. `load` is synchronous and gigabyte-scale on the emulator
side (a full `fs::read` of sectors.bin before it replies), so it needs a
generous read timeout — a short one would fire mid-load and spuriously fail a
successful load. The write timeout (tiny request payload) is short either way.

## `classify_response`

Classify a reply from its lines and whether the terminator was seen. Pulled
out of `send_control` so the truncation/error policy is unit-testable without a
socket. Truncation takes precedence over content: a reply missing its
terminator is untrustworthy even if the bytes that did arrive start with "OK".

## `response_is_error`

True when a control-socket response should be treated as a failure: any
"ERR " line, or a response whose first line does not start with "OK"
(including an empty/closed response). Pulled out of `send_control` so the
exit-code policy is unit-testable without spawning a socket.

## Test: `cli_socket_path_uses_the_shared_policy`

The CLI's connect side and the emulator's bind side must agree on the
socket path; both now derive it from the one shared `socket_name` module,
so this pins that the CLI really uses that policy (catches a mutation that
reintroduces a locally-computed path here).

## Test: `zero_byte_blob_is_a_failure_not_a_pass`

`validate` reported `sectors=true` from a bare `Path::exists()`, so a
zero-byte `sectors.bin` left by an interrupted capture validated clean and
exited 0 — a fixture the emulator cannot serve one sector from, blessed by
CI. Catches the mutation that goes back to existence-only checking: a
zero-length blob must be BROKEN (and so fail the profile), a non-empty one
must pass, an absent one stays a non-fatal "missing", and a blob we cannot
even stat is not a pass either.

## Test: `missing_terminator_is_truncation_even_if_it_starts_ok`

MED (round-2): the control protocol has no terminator, so a reply cut short
by a read timeout after "OK\n" was indistinguishable from a complete one and
`bdemu list-discs` printed a partial list and exited 0. classify_response
treats a MISSING terminator as truncation regardless of what did arrive.
Catches the mutation that ignores `terminated` (the old behaviour).

## Test: `validate_returns_true_only_for_a_complete_profile`

MED (round-2): `bdemu validate` is a CI profile gate, but nothing drove its
pass/fail verdict. A complete profile must validate (true), and a broken one
— a required blob missing, or a zero-byte sectors.bin from an interrupted
capture — must fail (false, which the caller turns into exit 1). Catches a
mutation that drops an `ok = false` assignment (so a broken profile passes).
