use super::{
    capture_disc, capture_metadata, capture_summary, clamp_ranges, copy_ranges, fixture_result,
    next_chunk, progress_due, range_line, rename_to_slug, slugify, sparse_file_size,
    write_bdsm_header, zero_fill_tail,
};

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

/// A per-test scratch directory under the crate's `target/`.
fn test_scratch_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-scratch")
        .join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create test scratch dir");
    dir
}

#[test]
fn capture_disc_fails_when_the_device_cannot_be_opened() {
    let dir = test_scratch_dir("capture_no_device");
    let out = dir.join("out");
    let err = capture_disc(
        "/nonexistent/bdemu-no-such-device",
        out.to_str().unwrap(),
        false,
    )
    .expect_err("a missing device must not report a clean capture");
    assert!(err.contains("open drive"), "got: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

const METADATA_FILES: [&str; 8] = [
    "capacity.bin",
    "toc.bin",
    "disc_info.bin",
    "ds_00.bin",
    "ds_01.bin",
    "ds_03.bin",
    "ds_0e.bin",
    "ds_0f.bin",
];

#[test]
fn metadata_capture_writes_every_structure_on_success() {
    let dir = test_scratch_dir("capture_meta_ok");
    let mut exec = |cdb: &[u8], buf: &mut [u8]| -> libfreemkv::Result<usize> {
        buf[..3].copy_from_slice(&[cdb[0], 0xAB, 0xCD]);
        Ok(3)
    };
    assert_eq!(capture_metadata(&mut exec, &dir), 0);
    for f in METADATA_FILES {
        let data = std::fs::read(dir.join(f)).unwrap_or_else(|e| panic!("{f}: {e}"));
        assert_eq!(data.len(), 3, "{f} holds exactly the transferred bytes");
    }
    assert_eq!(std::fs::read(dir.join("capacity.bin")).unwrap()[0], 0x25);
    let _ = std::fs::remove_dir_all(&dir);
}

fn scsi_error(status: u8, sense_key: Option<u8>) -> libfreemkv::Error {
    libfreemkv::Error::ScsiError {
        opcode: 0,
        status,
        sense: sense_key.map(|k| libfreemkv::scsi::ScsiSense {
            sense_key: k,
            asc: 0x24,
            ascq: 0,
        }),
    }
}

#[test]
fn metadata_hard_failures_count_only_required_structures() {
    let dir = test_scratch_dir("capture_meta_fail");
    let mut transport =
        |_: &[u8], _: &mut [u8]| -> libfreemkv::Result<usize> { Err(scsi_error(0xFF, None)) };
    assert_eq!(
        capture_metadata(&mut transport, &dir),
        5,
        "capacity, TOC, disc info, PFI and DI are required"
    );
    let mut medium =
        |_: &[u8], _: &mut [u8]| -> libfreemkv::Result<usize> { Err(scsi_error(0x02, Some(0x03))) };
    assert_eq!(capture_metadata(&mut medium, &dir), 5);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn metadata_absent_structures_are_not_failures() {
    let dir = test_scratch_dir("capture_meta_absent");
    let mut absent =
        |_: &[u8], _: &mut [u8]| -> libfreemkv::Result<usize> { Err(scsi_error(0x02, Some(0x05))) };
    assert_eq!(capture_metadata(&mut absent, &dir), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ranges_are_clamped_to_the_u32_lba_space() {
    assert_eq!(
        clamp_ranges(&[(10, 5), (u32::MAX - 1, 10)]),
        vec![(10, 5), (u32::MAX - 1, 1)]
    );
}

#[test]
fn range_line_reports_the_exclusive_end() {
    assert_eq!(range_line(0, 10, 5), "    range 0: LBA 10-15 (5 sectors)");
    assert!(range_line(1, u32::MAX, 1).contains("LBA 4294967295-4294967296"));
}

#[test]
fn bdsm_header_layout() {
    let mut out = Vec::new();
    write_bdsm_header(&mut out, &[(7, 2), (100, 1)]).unwrap();
    let mut want = b"BDSM".to_vec();
    for v in [1u32, 2, 7, 2, 100, 1] {
        want.extend_from_slice(&v.to_le_bytes());
    }
    assert_eq!(out, want);
}

#[test]
fn progress_is_due_every_500_sectors_and_at_the_end() {
    assert!(progress_due(500, 0, 10_000));
    assert!(!progress_due(499, 0, 10_000));
    assert!(progress_due(700, 100, 10_000));
    assert!(!progress_due(600, 200, 10_000));
    assert!(progress_due(100, 90, 100), "final chunk always prints");
    assert!(!progress_due(99, 90, 100));
}

/// Fake drive read: each sector is filled with its LBA's low byte.
fn lba_fill(lba: u32, n: u16, slice: &mut [u8]) {
    for k in 0..n as usize {
        let b = (lba as usize + k) as u8;
        slice[k * 2048..(k + 1) * 2048].fill(b);
    }
}

#[test]
fn copy_ranges_reads_each_range_in_chunks_and_in_order() {
    let ranges = [(100u32, 40u32), (500, 3)];
    let mut calls = Vec::new();
    let mut out = Vec::new();
    let errors = copy_ranges(&ranges, 43, &mut out, |lba, n, slice| {
        calls.push((lba, n, slice.len()));
        lba_fill(lba, n, slice);
        Ok(slice.len())
    })
    .unwrap();
    assert_eq!(errors, 0);
    assert_eq!(
        calls,
        vec![(100, 32, 32 * 2048), (132, 8, 8 * 2048), (500, 3, 3 * 2048)]
    );
    let lbas: Vec<u32> = (100..140).chain(500..503).collect();
    assert_eq!(out.len(), lbas.len() * 2048);
    for (i, lba) in lbas.iter().enumerate() {
        assert!(
            out[i * 2048..(i + 1) * 2048]
                .iter()
                .all(|&b| b == *lba as u8),
            "sector {i} must hold LBA {lba}"
        );
    }
}

#[test]
fn copy_ranges_zero_fills_failed_and_short_reads() {
    let mut out = Vec::new();
    let errors = copy_ranges(&[(0, 96)], 96, &mut out, |lba, n, slice| {
        lba_fill(lba | 0x80, n, slice);
        match lba {
            0 => Ok(100),
            32 | 64 => Err("boom".to_string()),
            _ => unreachable!(),
        }
    })
    .unwrap();
    assert_eq!(errors, 2);
    assert_eq!(out.len(), 96 * 2048);
    assert!(out[..100].iter().all(|&b| b == 0x80));
    assert!(
        out[100..].iter().all(|&b| b == 0),
        "short/failed reads are zeros"
    );
}

#[test]
fn sparse_file_size_counts_header_table_and_payload() {
    assert_eq!(sparse_file_size(0, 0), 12);
    assert_eq!(sparse_file_size(2, 0), 28);
    assert_eq!(sparse_file_size(3, 10), 12 + 24 + 10 * 2048);
}

#[test]
fn summary_flags_partial_captures() {
    assert_eq!(
        capture_summary(1000, 2, 0, 0),
        vec!["  Saved 1000 sectors (2.0 MB) in 2 ranges".to_string()]
    );
    let read_err = capture_summary(1000, 2, 2, 0);
    assert_eq!(read_err[0], "  2 read errors (zero-filled)");
    assert!(read_err[1].contains("PARTIAL: 2 read errors, 0 metadata errors"));
    let meta_err = capture_summary(1000, 2, 0, 1);
    assert_eq!(meta_err.len(), 1);
    assert!(meta_err[0].contains("PARTIAL: 0 read errors, 1 metadata error "));
}

#[test]
fn rename_to_slug_moves_the_capture_dir() {
    let root = test_scratch_dir("capture_rename_basic");
    let out = root.join("capture_out");
    std::fs::create_dir_all(&out).unwrap();
    let got = rename_to_slug(&out, "My Disc").unwrap();
    assert_eq!(got, root.join("my_disc"));
    assert!(got.is_dir() && !out.exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn rename_to_slug_picks_the_next_free_suffix() {
    let root = test_scratch_dir("capture_rename_collide");
    let out = root.join("capture_out");
    for d in [&out, &root.join("my_disc"), &root.join("my_disc_2")] {
        std::fs::create_dir_all(d).unwrap();
    }
    let got = rename_to_slug(&out, "MY DISC").unwrap();
    assert_eq!(got, root.join("my_disc_3"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn rename_to_slug_keeps_an_already_named_or_unnamed_dir() {
    let root = test_scratch_dir("capture_rename_noop");
    let named = root.join("my_disc");
    std::fs::create_dir_all(&named).unwrap();
    assert_eq!(rename_to_slug(&named, "My Disc").unwrap(), named);
    assert!(named.is_dir());
    assert_eq!(rename_to_slug(&named, "***").unwrap(), named);
    let _ = std::fs::remove_dir_all(&root);
}
