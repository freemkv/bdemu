// End-to-end tests of the `bdemu` CLI binary: exit codes and output of every
// subcommand, with a fake control socket standing in for a running emulator.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_bdemu");

/// A per-test scratch directory under the crate's `target/`.
fn scratch(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-scratch")
        .join(format!("cli_{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Run `bdemu` with `runtime` as XDG_RUNTIME_DIR (or unset), in `cwd`.
fn bdemu_in(args: &[&str], runtime: Option<&Path>, cwd: &Path) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args).current_dir(cwd).env_remove("BDEMU_INSTANCE");
    match runtime {
        Some(dir) => cmd.env("XDG_RUNTIME_DIR", dir),
        None => cmd.env_remove("XDG_RUNTIME_DIR"),
    };
    cmd.output().expect("spawn bdemu")
}

fn bdemu(args: &[&str]) -> Output {
    bdemu_in(args, None, Path::new(env!("CARGO_MANIFEST_DIR")))
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn no_subcommand_is_a_usage_error() {
    let o = bdemu(&[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).contains("Commands:"), "usage must be shown");
}

#[test]
fn help_prints_usage_and_succeeds() {
    for flag in ["help", "-h", "--help"] {
        let o = bdemu(&[flag]);
        assert_eq!(o.status.code(), Some(0), "{flag}");
        assert!(stdout(&o).contains("Commands:"), "{flag}");
    }
}

#[test]
fn unknown_subcommand_fails() {
    let o = bdemu(&["bogus"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("Unknown command: bogus"));
}

// --- validate ---

/// A complete profile: 96-byte INQUIRY, the three required features (the
/// serial a bare 4-byte header, the firmware feature carrying a date).
fn write_profile(dir: &Path, inquiry_len: usize) {
    std::fs::write(dir.join("drive.toml"), "[drive]\nproduct = \"X\"\n").unwrap();
    let mut inq = vec![b' '; inquiry_len];
    if inquiry_len >= 32 {
        inq[8..12].copy_from_slice(b"ACME");
        inq[16..21].copy_from_slice(b"DRIVE");
    }
    std::fs::write(dir.join("inquiry.bin"), inq).unwrap();
    std::fs::write(dir.join("gc_0000.bin"), [0u8; 8]).unwrap();
    std::fs::write(dir.join("gc_0108.bin"), [0x01, 0x08, 0x00, 0x00]).unwrap();
    std::fs::write(dir.join("gc_010c.bin"), b"\x01\x0c\x00\x0820240131").unwrap();
}

#[test]
fn validate_without_a_dir_is_a_usage_error() {
    let o = bdemu(&["validate"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("Usage: bdemu validate"));
}

#[test]
fn validate_reports_a_complete_profile() {
    let dir = scratch("validate_good");
    write_profile(&dir, 96);
    let o = bdemu(&["validate", dir.to_str().unwrap()]);
    let out = stdout(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(
        out.contains("✓ inquiry.bin (96 bytes) — ACME DRIVE"),
        "{out}"
    );
    assert!(
        out.contains("✓ gc_0108.bin (0x0108 Serial Number, 4 bytes)\n"),
        "a header-only serial gets no annotation: {out}"
    );
    assert!(
        out.contains("(0x010C Firmware Information, 12 bytes) — date: 20240131"),
        "{out}"
    );
    assert!(out.contains("✓ 3 total features"), "{out}");
    assert!(out.contains("Profile OK (1 warnings)"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn validate_warns_on_a_short_inquiry() {
    let dir = scratch("validate_short_inq");
    write_profile(&dir, 10);
    let o = bdemu(&["validate", dir.to_str().unwrap()]);
    let out = stdout(&o);
    assert_eq!(o.status.code(), Some(0), "{out}");
    assert!(
        out.contains("⚠ inquiry.bin (10 bytes, expected 96)"),
        "{out}"
    );
    assert!(out.contains("Profile OK (2 warnings)"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn validate_fails_an_incomplete_profile() {
    let dir = scratch("validate_bad");
    let o = bdemu(&["validate", dir.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).contains("Profile INCOMPLETE"));
    let _ = std::fs::remove_dir_all(&dir);
}

// --- run ---

#[test]
fn run_requires_a_profile() {
    let o = bdemu(&["run", "--", "true"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("--profile <dir> is required"));
}

#[test]
fn run_rejects_a_flag_missing_its_value() {
    let o = bdemu(&["run", "--profile"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("missing --profile value"));
}

#[test]
fn run_requires_a_command() {
    for args in [
        &["run", "--profile", "p"][..],
        &["run", "--profile", "p", "--"][..],
    ] {
        let o = bdemu(args);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        assert!(
            stderr(&o).contains("no command specified"),
            "{args:?}: {}",
            stderr(&o)
        );
    }
}

/// Removes a placeholder libbdemu.so this test created, on drop.
struct Placeholder(Option<PathBuf>);

impl Drop for Placeholder {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

// The child gets exactly the args after `--`, and its exit code is passed
// through. A placeholder .so is ignored by ld.so if the real one isn't built.
#[test]
fn run_passes_the_child_args_and_exit_code_through() {
    let lib = Path::new(BIN).with_file_name("libbdemu.so");
    let _placeholder = if lib.exists() {
        Placeholder(None)
    } else {
        std::fs::write(&lib, b"").unwrap();
        Placeholder(Some(lib.clone()))
    };
    let cwd = scratch("run_child");
    let o = bdemu_in(
        &["run", "--profile", "p", "--", "sh", "-c", "exit 7"],
        None,
        &cwd,
    );
    assert_eq!(o.status.code(), Some(7), "{}", stderr(&o));
    let _ = std::fs::remove_dir_all(&cwd);
}

// --- capture-disc ---

#[test]
fn capture_disc_needs_a_device_and_an_output_dir() {
    let o = bdemu(&["capture-disc", "/dev/null"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("Usage: bdemu capture-disc"));
}

#[test]
fn capture_disc_accepts_eject_anywhere_and_reports_open_failure() {
    let dir = scratch("capture_open_fail");
    let out = dir.join("out");
    let o = bdemu(&[
        "capture-disc",
        "--eject",
        "/nonexistent/bdemu-no-such-device",
        out.to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("Error: open drive"), "{}", stderr(&o));
    let _ = std::fs::remove_dir_all(&dir);
}

// --- control verbs ---

/// Accept one control connection on `dir/bdemu.sock`, send `reply`, and
/// return the command line received ("" if no client came within 10s).
fn serve_once(dir: &Path, reply: &'static str) -> std::thread::JoinHandle<String> {
    let listener = UnixListener::bind(dir.join("bdemu.sock")).expect("bind fake emulator");
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let stream = loop {
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(_) => return String::new(),
            }
        };
        stream.set_nonblocking(false).unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        (&stream).write_all(reply.as_bytes()).unwrap();
        line.trim_end().to_string()
    })
}

fn control(tag: &str, args: &[&str], reply: &'static str) -> (Output, String) {
    let dir = scratch(tag);
    let server = serve_once(&dir, reply);
    let o = bdemu_in(args, Some(&dir), &dir);
    let got = server.join().expect("fake emulator thread");
    let _ = std::fs::remove_dir_all(&dir);
    (o, got)
}

#[test]
fn control_verbs_send_their_command_and_print_the_reply() {
    for (args, wire) in [
        (&["status"][..], "status"),
        (&["eject"][..], "eject"),
        (&["list-discs"][..], "list-discs"),
        (&["load", "disc_a"][..], "load disc_a"),
    ] {
        let (o, got) = control(
            &format!("ctl_{wire}").replace(' ', "_"),
            args,
            "OK\nline two\n.\n",
        );
        assert_eq!(got, wire, "{args:?}");
        assert_eq!(o.status.code(), Some(0), "{args:?}: {}", stderr(&o));
        assert_eq!(stdout(&o), "OK\nline two\n", "{args:?}");
    }
}

#[test]
fn control_error_reply_exits_nonzero() {
    let (o, _) = control("ctl_err", &["status"], "ERR nope\n.\n");
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("ERR nope"));
}

#[test]
fn control_reply_without_terminator_is_truncated() {
    let (o, _) = control("ctl_trunc", &["status"], "OK\n");
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("truncated"), "{}", stderr(&o));
}

#[test]
fn control_without_an_emulator_fails() {
    let dir = scratch("ctl_none");
    let o = bdemu_in(&["status"], Some(&dir), &dir);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("Cannot connect to bdemu"));
    let o = bdemu(&["status"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("XDG_RUNTIME_DIR"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_validates_its_disc_name() {
    let o = bdemu(&["load"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("Usage: bdemu load"));
    let o = bdemu(&["load", "a\nb"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("illegal control character"));
}
