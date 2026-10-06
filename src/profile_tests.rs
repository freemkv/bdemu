use super::{
    LoadedProfile, is_contained_blob_name, parse_sector_file, parse_toml_value, parse_u16_opt,
    read_bin_reported, safe_disc_dir,
};

// A per-test scratch directory under the crate's `target/` (never /tmp, per
// the project no-/tmp scratch rule), created fresh (stale contents removed)
// at `<crate>/target/test-scratch/<tag>`, which `cargo clean` reclaims.
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
fn safe_disc_dir_rejects_traversal() {
    // Use a base that does not exist; the component checks run before
    // canonicalize, so traversal attempts are rejected regardless.
    let base = std::path::Path::new("/nonexistent/profile/discs");
    assert!(safe_disc_dir(base, "").is_none(), "empty name");
    assert!(safe_disc_dir(base, "..").is_none(), "parent dir");
    assert!(safe_disc_dir(base, ".").is_none(), "current dir");
    assert!(safe_disc_dir(base, "../etc").is_none(), "slash traversal");
    assert!(safe_disc_dir(base, "a/b").is_none(), "embedded slash");
    assert!(safe_disc_dir(base, "a\\b").is_none(), "backslash");
    assert!(safe_disc_dir(base, "a\0b").is_none(), "nul byte");
    // A plain component that simply doesn't exist also yields None (no dir).
    assert!(safe_disc_dir(base, "missing_disc").is_none());
}

#[test]
fn safe_disc_dir_accepts_existing_plain_component() {
    // Scratch under the crate's target/ (via test_scratch_dir) instead of
    // /tmp, per the project no-/tmp scratch rule — `cargo clean` reclaims
    // residue from an interrupted run, the system temp dir would not.
    let tmp = test_scratch_dir(&format!("bdemu_test_{}", std::process::id()));
    let discs = tmp.join("discs");
    let disc = discs.join("my_disc");
    std::fs::create_dir_all(&disc).unwrap();

    let got = safe_disc_dir(&discs, "my_disc");
    assert!(got.is_some(), "valid existing disc must resolve");
    assert!(got.unwrap().ends_with("my_disc"));

    let _ = std::fs::remove_dir_all(&tmp);
}

fn bdsm_header(num_ranges: u32) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"BDSM");
    v.extend_from_slice(&1u32.to_le_bytes());
    v.extend_from_slice(&num_ranges.to_le_bytes());
    v
}

#[test]
fn zero_ranges_serves_nothing_not_the_header() {
    // A BDSM file declaring 0 ranges (with trailing padding) captured no
    // sectors: it must serve nothing, not fall through to a flat dump that
    // hands the header + padding back as sector 0 at GOOD status.
    let mut data = bdsm_header(0);
    data.extend_from_slice(&[0xAB; 2048]);
    let (sectors, map) = parse_sector_file(data);
    assert!(map.is_empty());
    assert!(
        sectors.is_empty(),
        "a 0-range BDSM must serve no sectors, not its header/padding as sector 0",
    );
}

#[test]
fn huge_num_ranges_serves_nothing() {
    // 0xFFFFFFFF ranges declared but no body: must not allocate billions of
    // entries, and since the file DID match BDSM magic, the corrupt bail-out
    // must serve NOTHING, not the header bytes as a flat dump (see parse_sector_file).
    let data = bdsm_header(0xFFFF_FFFF);
    let (sectors, map) = parse_sector_file(data);
    assert!(map.is_empty(), "hostile num_ranges must not build a map");
    assert!(
        sectors.is_empty(),
        "a corrupt BDSM must serve no sectors, not its header as flat sector 0"
    );
}

#[test]
fn truncated_range_body_serves_nothing() {
    // One range claiming 1000 sectors (≈2 MB) but the file has no payload.
    // Building the map unchecked would later slice out of bounds; instead the
    // corrupt BDSM must serve nothing (not its header as a flat dump).
    let mut data = bdsm_header(1);
    data.extend_from_slice(&0u32.to_le_bytes()); // start_lba = 0
    data.extend_from_slice(&1000u32.to_le_bytes()); // count = 1000
    // No sector payload follows.
    let (sectors, map) = parse_sector_file(data);
    assert!(
        map.is_empty(),
        "truncated payload must not build an OOB map"
    );
    assert!(sectors.is_empty(), "a truncated BDSM must serve no sectors");
}

#[test]
fn count_multiply_overflow_serves_nothing() {
    // count near u32::MAX so count*2048 overflows usize math on 32-bit and
    // certainly exceeds data.len(): must not panic, must serve nothing.
    let mut data = bdsm_header(1);
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&u32::MAX.to_le_bytes());
    let (sectors, map) = parse_sector_file(data);
    assert!(map.is_empty());
    assert!(sectors.is_empty());
}

#[test]
fn valid_bdsm_builds_in_bounds_map() {
    // One range of 2 sectors with a correctly sized payload.
    let mut data = bdsm_header(1);
    data.extend_from_slice(&100u32.to_le_bytes()); // start_lba
    data.extend_from_slice(&2u32.to_le_bytes()); // count
    let payload_start = data.len();
    data.extend_from_slice(&vec![0xABu8; 2 * 2048]);
    let total_len = data.len();

    let (out, map) = parse_sector_file(data);
    assert_eq!(map.len(), 1);
    let (start, count, off) = map[0];
    assert_eq!(start, 100);
    assert_eq!(count, 2);
    assert_eq!(off, payload_start);
    // Every byte the map points at is within bounds.
    assert!(off + count as usize * 2048 <= total_len);
    assert_eq!(out.len(), total_len);
}

#[test]
fn out_of_order_ranges_are_sorted_for_binary_search() {
    // A capture tool emitting ranges out of ascending LBA order must still
    // resolve correctly: without parse_sector_file's sort, lookup_sector's
    // binary search would miss captured sectors and READ would zero-fill them.
    let mut data = bdsm_header(2);
    // Range 0 (file order): LBA 1000, 1 sector, payload 0xCC
    data.extend_from_slice(&1000u32.to_le_bytes());
    data.extend_from_slice(&1u32.to_le_bytes());
    // Range 1 (file order): LBA 100, 1 sector, payload 0xAA
    data.extend_from_slice(&100u32.to_le_bytes());
    data.extend_from_slice(&1u32.to_le_bytes());
    // Payload follows in FILE order: first the LBA-1000 range, then LBA-100.
    let off_1000 = data.len();
    data.extend_from_slice(&[0xCCu8; 2048]);
    let off_100 = data.len();
    data.extend_from_slice(&[0xAAu8; 2048]);

    let (_, map) = parse_sector_file(data);
    assert_eq!(map.len(), 2);

    // After parsing the map must be ascending by start_lba.
    assert!(
        map.windows(2).all(|w| w[0].0 <= w[1].0),
        "sector_map must be sorted ascending by start_lba for binary search"
    );

    // The byte_offset for each range must still point at that range's
    // original bytes (file-order offsets preserved through the sort).
    let r100 = map.iter().find(|&&(s, _, _)| s == 100).unwrap();
    let r1000 = map.iter().find(|&&(s, _, _)| s == 1000).unwrap();
    assert_eq!(r100.2, off_100, "LBA-100 range keeps its file-order offset");
    assert_eq!(
        r1000.2, off_1000,
        "LBA-1000 range keeps its file-order offset"
    );
    // scsi::lookup_sector's binary-search resolution over this sorted map is
    // covered by scsi.rs's lookup_sector_resolves_out_of_order_capture test.
}

#[test]
fn toml_value_keeps_hash_inside_quotes() {
    // A '#' inside a quoted value is part of the value, not a comment.
    assert_eq!(parse_toml_value(r#""BDR-#1""#), "BDR-#1");
    assert_eq!(parse_toml_value(r#"  "file#2.bin"  "#), "file#2.bin");
    // A trailing comment after a quoted value is still stripped.
    assert_eq!(parse_toml_value(r#""value"  # a comment"#), "value");
    // Plain quoted value.
    assert_eq!(parse_toml_value(r#""plain""#), "plain");
    // Unquoted value: '#' begins a comment.
    assert_eq!(parse_toml_value("0x0043 # profile"), "0x0043");
    assert_eq!(parse_toml_value("  bare  "), "bare");
    // Unterminated quote falls back to comment-stripping.
    assert_eq!(parse_toml_value(r#""oops # trailing"#), "oops");
    // Unquoted value that itself contains quote characters keeps them: the
    // else branch must NOT trim_matches('"') or it would truncate the
    // trailing quote (regression guard for `ACME "Pro"` -> `ACME "Pro`).
    assert_eq!(parse_toml_value(r#"ACME "Pro""#), r#"ACME "Pro""#);
    assert_eq!(parse_toml_value(r#"trailing"  "#), r#"trailing""#);
}

#[test]
fn parse_u16_preserves_default_on_bad_input() {
    assert_eq!(parse_u16_opt("0x0043"), Some(0x0043));
    assert_eq!(parse_u16_opt("0X0043"), Some(0x0043));
    assert_eq!(parse_u16_opt("67"), Some(67));
    assert_eq!(parse_u16_opt("invalid"), None);
    assert_eq!(parse_u16_opt(""), None);
}

#[test]
fn json_invalid_feature_code_is_skipped_not_coerced_to_zero() {
    use std::io::Write;
    // A garbage feature key ("oops") must be SKIPPED, not silently coerced to
    // 0x0000 (the real Profile List feature code), which would inject a bogus
    // Profile List descriptor into GET CONFIGURATION.
    let json = r#"{
            "drive": { "product": "TEST" },
            "inquiry": { "raw": "00" },
            "get_config": {
                "current_profile": "0x0043",
                "features": {
                    "0x010C": { "raw": "010c0200deadbeef" },
                    "oops":   { "raw": "00001234" }
                }
            }
        }"#;
    // Under the crate's target/ (test_scratch_dir), not /tmp — per the
    // project no-/tmp scratch rule.
    let path = test_scratch_dir("bdemu_json_feat").join(format!(
        "feat_{}_{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(json.as_bytes()).unwrap();
    }

    let profile =
        super::LoadedProfile::load_json(path.to_str().unwrap()).expect("valid JSON must load");
    let _ = std::fs::remove_file(&path);

    // The valid feature is present.
    assert!(
        profile.features.iter().any(|(c, _)| *c == 0x010C),
        "valid feature 0x010C must be loaded"
    );
    // The garbage key must NOT have been coerced to 0x0000.
    assert!(
        !profile.features.iter().any(|(c, _)| *c == 0x0000),
        "invalid feature key must be skipped, not coerced to 0x0000"
    );
    // Exactly one feature survived.
    assert_eq!(profile.features.len(), 1);
}
// A genuinely absent blob is the documented "feature not present" case and
// stays silent, but every OTHER read failure (e.g. a directory where a file
// was expected) must return Err, not an indistinguishable empty Vec.
#[test]
fn unreadable_blob_is_reported_while_absent_blob_is_silent() {
    let dir = test_scratch_dir("read_bin_reported");

    // Absent: Ok(empty), no error — profiles legitimately omit optional blobs.
    let missing = dir.join("not_here.bin");
    assert_eq!(
        read_bin_reported(&missing).expect("absent blob must not be an error"),
        Vec::<u8>::new()
    );

    // Present and readable: the bytes come back.
    let good = dir.join("good.bin");
    std::fs::write(&good, b"abc").unwrap();
    assert_eq!(read_bin_reported(&good).unwrap(), b"abc".to_vec());

    // Unreadable (a directory in a file's place — an EISDIR that stat cannot
    // catch): must be an Err naming the path, NOT a silent empty Vec.
    let as_dir = dir.join("sectors.bin");
    std::fs::create_dir_all(&as_dir).unwrap();
    let err = read_bin_reported(&as_dir).expect_err("unreadable blob must report");
    assert!(
        err.contains("sectors.bin"),
        "the message must name the blob: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// A profile directory holding only `drive.toml` — every binary blob missing —
// must still load, with each absent blob presenting as empty rather than
// failing the whole profile or panicking on a missing file.
#[test]
fn profile_loads_with_every_blob_missing() {
    let dir = test_scratch_dir("profile_missing_blobs");
    std::fs::write(
        dir.join("drive.toml"),
        "[drive]\nproduct = \"BDR-S09\"\ncurrent_profile = 0x0043\n\
             [files]\ninquiry = \"inquiry.bin\"\nrpc_state = \"rpc_state.bin\"\n\
             [features]\n0x0108 = \"gc_0108.bin\"\n",
    )
    .unwrap();

    let p = LoadedProfile::load(dir.to_str().unwrap()).expect("must still load");
    assert_eq!(p.name, "BDR-S09");
    assert_eq!(p.current_profile, 0x0043);
    assert!(p.inquiry.is_empty(), "absent inquiry.bin -> empty");
    assert!(p.rpc_state.is_empty(), "absent rpc_state.bin -> empty");
    assert!(p.mode_2a.is_empty(), "absent mode_2a.bin -> empty");
    // A feature whose file is missing must be DROPPED, not registered with an
    // empty payload: GET CONFIGURATION would otherwise answer an existing-but-
    // zero-length feature descriptor for it.
    assert!(p.features.is_empty(), "features with no file are dropped");
    assert!(p.read_bufs.is_empty());
    assert!(!p.has_disc(), "no BDEMU_DISC set -> no disc");

    let _ = std::fs::remove_dir_all(&dir);
}

// The lexical containment guard for profile blob filenames: a plain filename
// (even with dots) is accepted; anything that could escape the profile
// directory is rejected.
#[test]
fn contained_blob_name_accepts_plain_files_rejects_escapes() {
    assert!(is_contained_blob_name("inquiry.bin"));
    assert!(is_contained_blob_name("gc_0108.bin"));
    assert!(is_contained_blob_name("rb_f1.bin"));
    assert!(is_contained_blob_name("file.with.dots.bin"));

    assert!(!is_contained_blob_name(""), "empty");
    assert!(!is_contained_blob_name("."), "current dir");
    assert!(!is_contained_blob_name(".."), "parent dir");
    assert!(!is_contained_blob_name("../secret.bin"), "relative escape");
    assert!(!is_contained_blob_name("/etc/passwd"), "absolute path");
    assert!(!is_contained_blob_name("a/b.bin"), "embedded slash");
    assert!(!is_contained_blob_name("a\\b.bin"), "backslash");
    assert!(!is_contained_blob_name("a\0b.bin"), "nul byte");
}

// A hostile `drive.toml` that points a blob filename OUTSIDE the profile
// directory (`inquiry = "../secret.bin"`) must NOT cause bdemu to read that
// file and serve its bytes as the emulated INQUIRY / feature / rpc response.
#[test]
fn blob_filenames_cannot_escape_the_profile_directory() {
    let root = test_scratch_dir("blob_escape");
    // A secret that lives OUTSIDE the profile directory.
    std::fs::write(root.join("secret.bin"), b"TOPSECRETKEYMATERIAL").unwrap();

    let profile = root.join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(
        profile.join("drive.toml"),
        "[drive]\nproduct = \"X\"\n\
             [files]\ninquiry = \"../secret.bin\"\nrpc_state = \"../secret.bin\"\n\
             mode_2a = \"../secret.bin\"\n\
             [features]\n0x0108 = \"../secret.bin\"\n\
             [read_buffer]\n0xf1 = \"../secret.bin\"\n",
    )
    .unwrap();

    let p = LoadedProfile::load(profile.to_str().unwrap()).expect("profile must still load");
    assert!(
        p.inquiry.is_empty(),
        "a traversal inquiry path must be refused, not read as INQUIRY bytes"
    );
    assert!(p.rpc_state.is_empty(), "traversal rpc_state path refused");
    assert!(p.mode_2a.is_empty(), "traversal mode_2a path refused");
    assert!(
        p.features.is_empty(),
        "traversal feature path refused (not served as a feature descriptor)"
    );
    assert!(p.read_bufs.is_empty(), "traversal read_buffer path refused");

    let _ = std::fs::remove_dir_all(&root);
}

// Two ranges covering the same LBA make lookup_sector's binary search resolve
// to an ARBITRARY one of them, so an overlapping map must be treated as
// corrupt and fall back to flat (empty map), not served as authoritative GOOD.
#[test]
fn overlapping_ranges_serve_nothing() {
    let mut data = bdsm_header(2);
    // Range 0: LBA 100..105.
    data.extend_from_slice(&100u32.to_le_bytes());
    data.extend_from_slice(&5u32.to_le_bytes());
    // Range 1: LBA 103..106 — overlaps range 0 at 103 and 104.
    data.extend_from_slice(&103u32.to_le_bytes());
    data.extend_from_slice(&3u32.to_le_bytes());
    data.extend_from_slice(&vec![0u8; (5 + 3) * 2048]);

    let (sectors, map) = parse_sector_file(data);
    assert!(
        map.is_empty(),
        "overlapping ranges must not build an ambiguous map"
    );
    // The file bytes must be discarded, not kept as a flat dump: keeping them
    // would serve the BDSM header as sector 0 at GOOD (end-to-end proof in
    // scsi::tests::read10_overlapping_capture_serves_nothing_not_header).
    assert!(
        sectors.is_empty(),
        "an overlapping BDSM must serve no sectors, not its header as flat data"
    );
}

// A zero-count range captures nothing yet, left in the map, evades the
// overlap guard (end == start) and makes lookup_sector's search non-
// monotonic. It must be dropped — here a (100,0) masking a (100,3) overlap.
#[test]
fn zero_count_ranges_are_dropped_not_left_to_mask_overlap() {
    let mut data = bdsm_header(2);
    // Range 0: zero-count at LBA 100 (end == start == 100).
    data.extend_from_slice(&100u32.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    // Range 1: LBA 100..103 — shares LBA 100 with range 0's start.
    data.extend_from_slice(&100u32.to_le_bytes());
    data.extend_from_slice(&3u32.to_le_bytes());
    data.extend_from_slice(&vec![0u8; 3 * 2048]);

    let (_sectors, map) = parse_sector_file(data);
    // The zero-count range is gone; only the real range remains, so the map
    // is strictly monotonic and binary search is well-defined.
    assert_eq!(map.len(), 1, "zero-count range must be dropped");
    assert_eq!(map[0].0, 100, "surviving range starts at 100");
    assert_eq!(map[0].1, 3, "surviving range has count 3");
}

// `LoadedProfile::load` on a path that is neither a directory nor a `.json`
// file must reject it explicitly, not silently return `None` via some other
// path.
#[test]
fn load_rejects_unknown_format() {
    let dir = test_scratch_dir("unknown_format");
    let path = dir.join("profile.xyz");
    std::fs::write(&path, b"irrelevant").unwrap();
    assert!(
        LoadedProfile::load(path.to_str().unwrap()).is_none(),
        "a path that is neither a directory nor .json must be refused"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// One `drive.toml` exercising every remaining loader branch not covered elsewhere:
// malformed entries, present blobs, and the loose rb_*.bin scan.
#[test]
fn toml_loader_covers_malformed_entries_and_positive_blob_loads() {
    let dir = test_scratch_dir("toml_full_coverage");
    std::fs::write(
        dir.join("drive.toml"),
        "# a leading comment\n\
             \n\
             [drive]\n\
             product = \"BDR-COVER\"\n\
             current_profile = \"not-a-number\"\n\
             \n\
             [files]\n\
             inquiry = \"inquiry.bin\"\n\
             \n\
             [features]\n\
             0x0108 = \"feat_0108.bin\"\n\
             bogus_key = \"ignored.bin\"\n\
             \n\
             [read_buffer]\n\
             0xf1 = \"rb_f1.bin\"\n\
             zz = \"ignored.bin\"\n\
             \n\
             [unlock]\n\
             foo = \"bar\"\n\
             \n\
             [weird_section]\n\
             x = \"y\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("feat_0108.bin"), b"featdata").unwrap();
    std::fs::write(dir.join("rb_f1.bin"), b"rbdata1").unwrap();
    // Not referenced in drive.toml at all: picked up by the rb_*.bin scan.
    std::fs::write(dir.join("rb_f2.bin"), b"rbdata2").unwrap();

    let p = LoadedProfile::load(dir.to_str().unwrap()).expect("must load");
    assert_eq!(p.name, "BDR-COVER");
    assert_eq!(
        p.current_profile, 0x0043,
        "an invalid current_profile must keep the default, not become 0x0000"
    );
    assert_eq!(
        p.features,
        vec![(0x0108, b"featdata".to_vec())],
        "the bogus feature key must be skipped, the valid one loaded"
    );
    assert!(
        p.rpc_state.is_empty(),
        "no rpc_state key at all -> Vec::new() default"
    );
    let mut bufs = p.read_bufs.clone();
    bufs.sort_by_key(|(id, _)| *id);
    assert_eq!(
        bufs,
        vec![(0xf1, b"rbdata1".to_vec()), (0xf2, b"rbdata2".to_vec()),],
        "the TOML-listed buffer and the loose rb_*.bin scan hit must both load"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// The JSON loader's remaining branches: a valid `[read_buffer]` entry
// alongside an invalid key, an invalid `current_profile`, and the optional
// `mode_sense.page_2a` / `report_key.rpc_state` blobs actually present.
#[test]
fn json_loader_covers_read_buffer_and_optional_blobs() {
    use std::io::Write;
    let json = r#"{
            "drive": { "product": "JSONDRV" },
            "inquiry": { "raw": "00" },
            "get_config": {
                "current_profile": "garbage",
                "features": {}
            },
            "mode_sense": { "page_2a": { "raw": "2a00" } },
            "report_key": { "rpc_state": { "raw": "0102" } },
            "read_buffer": {
                "0xf1": { "raw": "aabb" },
                "zz":   { "raw": "cc" }
            }
        }"#;
    let path = test_scratch_dir("json_full_coverage").join(format!(
        "full_{}_{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(json.as_bytes()).unwrap();
    }

    // Through the public `load()` dispatcher (not `load_json` directly) so
    // the `.json`-extension routing branch itself is exercised too.
    let p = LoadedProfile::load(path.to_str().unwrap()).expect("must load");
    let _ = std::fs::remove_file(&path);

    assert_eq!(
        p.current_profile, 0x0043,
        "an invalid current_profile must keep the 0x0043 default"
    );
    assert_eq!(
        p.read_bufs,
        vec![(0xf1, vec![0xaa, 0xbb])],
        "the invalid 'zz' key must be skipped, not coerced to id 0"
    );
    assert_eq!(p.mode_2a, vec![0x2a, 0x00]);
    assert_eq!(p.rpc_state, vec![0x01, 0x02]);
}

// `safe_disc_dir` when the base itself resolves (an existing `discs/`
// directory) but the requested name does not exist under it: must reject
// via the disc_dir-canonicalize-failure arm, not the base-failure arm.
#[test]
fn safe_disc_dir_rejects_missing_target_under_existing_base() {
    let base = test_scratch_dir("safe_disc_dir_existing_base");
    assert!(safe_disc_dir(&base, "does_not_exist").is_none());
    let _ = std::fs::remove_dir_all(&base);
}

/// `load_disc` must pick up `ds_*.bin` disc-structure files keyed by their
/// hex format-code suffix. Nothing previously exercised this scan.
#[test]
fn load_disc_picks_up_disc_structure_files() {
    let dir = test_scratch_dir("load_disc_structures");
    std::fs::write(dir.join("ds_08.bin"), b"structdata").unwrap();
    let disc = super::load_disc(&dir);
    assert_eq!(
        disc.disc_structures.get(&0x08).map(|v| v.as_slice()),
        Some(b"structdata".as_slice())
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// A blob past `MAX_BIN_BYTES` must be refused with an `Err` naming the size,
// not silently truncated or OOM-ing the process. A sparse file (via
// `set_len`) reports the oversized length to `stat` without using disk.
#[test]
fn oversized_blob_is_rejected() {
    let dir = test_scratch_dir("oversized_blob");
    let path = dir.join("huge.bin");
    {
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(super::MAX_BIN_BYTES + 1).unwrap();
    }
    let err = read_bin_reported(&path).expect_err("oversized blob must be refused");
    assert!(err.contains("exceeds"), "got: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `read_bin` (the logging wrapper, not `read_bin_reported`) must log and
/// return empty on a genuine read failure, not panic or silently succeed.
#[test]
fn read_bin_wrapper_logs_and_returns_empty_on_error() {
    let dir = test_scratch_dir("read_bin_wrapper_unreadable");
    // A directory where a file was expected: EISDIR on read.
    let as_dir = dir.join("blob.bin");
    std::fs::create_dir_all(&as_dir).unwrap();
    assert!(
        super::read_bin(&as_dir).is_empty(),
        "an unreadable blob must yield empty, not panic"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `parse_hex`: odd-length input drops the trailing unpaired nibble instead
/// of panicking or padding, and non-hex characters are filtered out before
/// pairing.
#[test]
fn parse_hex_handles_odd_length_and_non_hex_noise() {
    assert_eq!(super::parse_hex("deadbeef"), vec![0xde, 0xad, 0xbe, 0xef]);
    assert_eq!(
        super::parse_hex("abc"),
        vec![0xab],
        "a trailing unpaired hex digit must be dropped, not padded"
    );
    assert_eq!(
        super::parse_hex("de:ad be-ef"),
        vec![0xde, 0xad, 0xbe, 0xef],
        "non-hex punctuation/whitespace must be filtered before pairing"
    );
    assert_eq!(super::parse_hex(""), Vec::<u8>::new());
}

// The other half of the overlap check: ranges that merely TOUCH (prev end
// == next start) are disjoint and must be KEPT, not wrongly discarded.
#[test]
fn adjacent_non_overlapping_ranges_are_kept() {
    let mut data = bdsm_header(2);
    // Range 0: LBA 100..105.
    data.extend_from_slice(&100u32.to_le_bytes());
    data.extend_from_slice(&5u32.to_le_bytes());
    // Range 1: LBA 105..108 — touches range 0 but does not overlap.
    data.extend_from_slice(&105u32.to_le_bytes());
    data.extend_from_slice(&3u32.to_le_bytes());
    data.extend_from_slice(&vec![0u8; (5 + 3) * 2048]);

    let (_, map) = parse_sector_file(data);
    assert_eq!(map.len(), 2, "touching-but-disjoint ranges must be kept");
}
