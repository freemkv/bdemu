use super::{fixture_result, next_chunk, slugify, zero_fill_tail};

#[test]
fn short_read_zeroes_stale_tail() {
    // A reused buffer pre-loaded with a prior chunk's bytes.
    let mut buf = vec![0xABu8; 16];
    // Short read: only 6 bytes transferred this iteration.
    zero_fill_tail(&mut buf, 6);
    // First 6 bytes are left as the read filled them (untouched here)...
    assert_eq!(&buf[..6], &[0xAB; 6]);
    // ...and the stale tail is zeroed, not left holding neighbor data.
    assert!(
        buf[6..].iter().all(|&b| b == 0),
        "tail not zeroed: {:?}",
        buf
    );
}

#[test]
fn full_read_leaves_buffer_untouched() {
    // A full transfer fills the whole slice; nothing should be zeroed.
    let mut buf = vec![0xCDu8; 8];
    zero_fill_tail(&mut buf, 8);
    assert!(buf.iter().all(|&b| b == 0xCD));
}

#[test]
fn overreported_transfer_does_not_panic() {
    // A transport that claims more bytes than the slice holds must clamp,
    // not panic on an out-of-bounds slice index.
    let mut buf = vec![0xEEu8; 4];
    zero_fill_tail(&mut buf, 999);
    assert!(buf.iter().all(|&b| b == 0xEE));
}

#[test]
fn clean_capture_is_ok_errors_fail() {
    // No errors of either kind -> Ok (clean fixture).
    assert!(fixture_result(0, 0).is_ok());
    // Any sector read error -> Err so capture-disc exits non-zero instead
    // of silently shipping a zero-filled (incomplete) fixture.
    let e = fixture_result(1, 0).unwrap_err();
    assert!(e.contains("1 sector read error"), "got: {e}");
    // ...and must NOT pluralize the singular case.
    assert!(!e.contains("1 sector read errors"), "got: {e}");
    assert!(fixture_result(42, 0).is_err());

    // A required SCSI structure that failed for a hard reason must also
    // fail the fixture, even with zero sector read errors — a transport
    // fault on capacity/TOC/PFI/DI used to be hidden behind "n/a".
    let e = fixture_result(0, 1).unwrap_err();
    assert!(e.contains("1 required SCSI structure failed"), "got: {e}");
    // Plural form for >1.
    let e = fixture_result(0, 3).unwrap_err();
    assert!(e.contains("3 required SCSI structures failed"), "got: {e}");
}

#[test]
fn chunk_progresses_on_65536_multiple_range() {
    // Regression: a range whose remaining sector count is a nonzero
    // multiple of 65536 must NOT yield a chunk of 0, otherwise the read
    // loop never advances and spins forever.
    let chunk: u16 = 32;
    let start: u32 = 0;
    let end: u32 = 65536; // exactly 65536 — `as u16` of this is 0

    let n = next_chunk(start, end, chunk);
    assert!(n > 0, "chunk must advance, got 0 (infinite loop)");
    assert_eq!(n, chunk);

    // Full walk must terminate and cover exactly `end` sectors.
    let mut lba = start;
    let mut total: u64 = 0;
    let mut iters = 0u32;
    while lba < end {
        let n = next_chunk(lba, end, chunk);
        assert!(n > 0);
        lba += n as u32;
        total += n as u64;
        iters += 1;
        assert!(iters < 10_000, "loop did not terminate");
    }
    assert_eq!(total, end as u64);
}

#[test]
fn chunk_clamps_to_remaining() {
    // Fewer than `chunk` sectors remain.
    assert_eq!(next_chunk(100, 110, 32), 10);
    // Exactly `chunk` remain.
    assert_eq!(next_chunk(0, 32, 32), 32);
    // A large remaining count clamps to `chunk`.
    assert_eq!(next_chunk(0, 1_000_000, 32), 32);
}
// slugify() maps a hostile untrusted UDF volume ID to the rename target
// dir, so every non-alphanumeric char must become `_` (traversal impossible
// by construction). This pins that against a "allow `.`/`-`" regression.
#[test]
fn slugify_cannot_produce_a_traversing_name() {
    // The traversal attempt collapses to a plain component.
    assert_eq!(slugify("../../etc"), "etc");
    assert_eq!(slugify("/etc/passwd"), "etc_passwd");
    assert_eq!(slugify(".."), "");
    assert_eq!(slugify("a/../b"), "a____b");
    // No separator or dot ever survives, whatever the input.
    for hostile in ["../x", "..\\x", "a/b", "a.b", "~/x", "$HOME", "a\0b"] {
        let s = slugify(hostile);
        assert!(
            !s.contains('/') && !s.contains('\\') && !s.contains('.') && !s.contains('\0'),
            "slugify({hostile:?}) = {s:?} must not carry a path character"
        );
    }
}

// The everyday rename behaviour: lowercasing, `_` for spaces/punctuation,
// no leading/trailing underscores. Catches a dropped trim or lowercasing.
#[test]
fn slugify_normalises_ordinary_volume_ids() {
    assert_eq!(slugify("SAMPLE FILM"), "sample_film");
    assert_eq!(slugify("Amélie 2001"), "am_lie_2001");
    assert_eq!(slugify("  Spaced  "), "spaced");
    assert_eq!(slugify("THX-1138"), "thx_1138");
    // An all-punctuation volume ID slugifies to nothing, which capture_disc
    // treats as "no usable name" and skips the rename for.
    assert_eq!(slugify("***"), "");
    assert_eq!(slugify(""), "");
}
