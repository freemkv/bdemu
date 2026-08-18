// bdemu — Blu-ray Drive Emulator
// MIT — freemkv project
//
// Smart disc capture — uses libfreemkv to open/unlock the drive,
// parses UDF to find metadata sector ranges, writes sparse BDSM format.
// Only captures sectors disc-info needs (~5MB per disc).

use libfreemkv::{Drive, scsi};
use std::fs;
use std::io::Write;
use std::path::Path;

const SECTOR_SIZE: usize = 2048;

/// Print a partial-line progress label and flush it immediately. stdout is
/// line-buffered on a TTY, so a bare `print!("  x... ")` stays buffered until
/// the following (often slow, blocking) op finishes — defeating its purpose as
/// a live "in progress" indicator. Flushing forces the label out now.
fn print_step(msg: &str) {
    print!("{}", msg);
    let _ = std::io::stdout().flush();
}

/// Number of sectors to read in the next chunk: at most `chunk`, never more
/// than what remains (`end - lba`). The clamp happens in u32 *before* the
/// u16 cast so a remaining count that is a nonzero multiple of 65536 can never
/// truncate to 0 (which would make the read loop spin forever).
fn next_chunk(lba: u32, end: u32, chunk: u16) -> u16 {
    (end - lba).min(chunk as u32) as u16
}

/// Zero the tail of a reused read buffer past the bytes actually transferred.
///
/// `buf` is reused across chunks, so bytes a short read did not fill still hold
/// the previous chunk's data. After an `Ok(transferred)` read this zeroes
/// `buf[transferred..]` so the fixture records zeros there, not stale neighbor
/// data — matching the per-iteration `vec![0u8; ..]` semantics the buffer reuse
/// replaced. `transferred` is clamped to `buf.len()` so a transport that
/// over-reports the byte count cannot panic the slice.
fn zero_fill_tail(buf: &mut [u8], transferred: usize) {
    let filled = transferred.min(buf.len());
    buf[filled..].fill(0);
}

/// Capture a disc's emulation fixture from `device` (e.g. `/dev/sr0`) into
/// `output_dir`, which is created if absent. On success the directory holds the
/// SCSI response fixtures (INQUIRY, GET_CONFIGURATION feature dumps, MODE SENSE,
/// READ_TOC, optional vendor pages) plus the sparse metadata sectors disc-info
/// needs — enough for the emulator to replay the disc without the physical media.
///
/// When `eject` is set the drive is ejected after capture so the user can swap
/// discs (gated because eject is irreversible on slot-loading drives).
///
/// On a clean capture the output directory is renamed to a slugified form of the
/// disc's UDF volume ID (with a `_N` suffix to avoid collisions).
///
/// An `Err` means the capture was partial or failed (bad/zero-filled sectors or a
/// lost SCSI structure). The bytes read so far are still written to `output_dir`
/// for inspection, but the slugified rename is skipped so an incomplete capture
/// does not claim a clean disc name.
pub fn capture_disc(device: &str, output_dir: &str, eject: bool) -> Result<(), String> {
    let dir = Path::new(output_dir);
    fs::create_dir_all(dir).map_err(|e| format!("create dir: {}", e))?;

    println!("Capturing disc from {} -> {}/", device, output_dir);
    println!();

    let dev_path = Path::new(device);
    let mut session = Drive::open(dev_path).map_err(|e| format!("open drive: {}", e))?;

    println!(
        "  Drive: {} {} {}",
        session.drive_id.vendor_id.trim(),
        session.drive_id.product_id.trim(),
        session.drive_id.product_revision.trim()
    );

    // SCSI metadata. `metadata_errors` counts hard failures (transport / non-
    // ILLEGAL-REQUEST) on *required* structures, so a capture that silently
    // dropped capacity/TOC/PFI/DI to a drive fault is reported as incomplete
    // rather than clean.
    println!();
    let mut metadata_errors: u32 = 0;
    // (filename, label, cdb, size, required)
    metadata_errors += scsi_save(
        &mut session,
        dir,
        "capacity.bin",
        "READ_CAPACITY",
        &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        8,
        true,
    ) as u32;
    metadata_errors += scsi_save(
        &mut session,
        dir,
        "toc.bin",
        "READ_TOC",
        &[0x43, 0, 0, 0, 0, 0, 0, 0x10, 0, 0],
        4096,
        true,
    ) as u32;
    metadata_errors += scsi_save(
        &mut session,
        dir,
        "disc_info.bin",
        "READ_DISC_INFO",
        &[0x51, 0, 0, 0, 0, 0, 0, 0x01, 0, 0],
        256,
        true,
    ) as u32;
    // PFI (0x00) and DI (0x01) are required; BCA (0x03) and the disc control /
    // read-only physical-format structures are optional and may legitimately be
    // absent (-> "n/a").
    for (fmt, name, required) in &[
        (0x00u8, "PFI", true),
        (0x01, "DI", true),
        (0x03, "BCA", false),
        // 0x0E and 0x0F are distinct DISC_STRUCTURE formats — they must not
        // share a label, or the capture log and ds_0e.bin / ds_0f.bin
        // annotations are ambiguous.
        (0x0E, "DCB", false),    // Disc Control Block
        (0x0F, "PFI_RO", false), // Physical Format Information (read-only)
    ] {
        let cdb = [0xAD, 0x01, 0, 0, 0, 0, 0, *fmt, 0x10, 0x04, 0, 0];
        metadata_errors += scsi_save(
            &mut session,
            dir,
            &format!("ds_{:02x}.bin", fmt),
            &format!("DISC_STRUCTURE 0x{:02X} ({})", fmt, name),
            &cdb,
            4100,
            *required,
        ) as u32;
    }

    // Parse UDF, discover metadata ranges
    println!();
    print_step("  Parsing UDF... ");
    let udf = libfreemkv::read_filesystem(&mut session).map_err(|e| format!("UDF: {}", e))?;
    println!("done");

    print_step("  Discovering metadata ranges... ");
    let raw_ranges = udf
        .metadata_sector_ranges(&mut session)
        .map_err(|e| format!("ranges: {}", e))?;

    // Single source of truth for "how many sectors this range actually
    // contributes". The read loop below only emits `end - start` sectors where
    // `end = start.saturating_add(count)`, so for a hostile/corrupt range with
    // `start + count > u32::MAX` the saturated `written` is smaller than the
    // raw `count`. Normalize once here and use `written` for the BDSM header
    // count field, the `total`, the file_size, and the read loop alike — so the
    // table, the total, and the bytes actually on disk can never disagree (a
    // consumer reading by the table would otherwise run past real bytes into
    // the next range).
    let ranges: Vec<(u32, u32)> = raw_ranges
        .iter()
        .map(|(start, count)| (*start, start.saturating_add(*count) - start))
        .collect();

    // Saturating sum so a hostile set of ranges cannot overflow-panic here.
    let total: u32 = ranges.iter().fold(0u32, |acc, r| acc.saturating_add(r.1));
    println!(
        "{} ranges, {} sectors ({:.1} MB)",
        ranges.len(),
        total,
        total as f64 * SECTOR_SIZE as f64 / 1e6
    );
    for (i, (s, written)) in ranges.iter().enumerate() {
        // u64-cast the end so a hostile start+count near u32::MAX does not
        // overflow this display arithmetic.
        let end = *s as u64 + *written as u64;
        println!("    range {}: LBA {}-{} ({} sectors)", i, s, end, written);
    }

    // Write BDSM sparse sector map
    println!();
    let mut file =
        fs::File::create(dir.join("sectors.bin")).map_err(|e| format!("create: {}", e))?;

    // Header
    file.write_all(b"BDSM")
        .map_err(|e| format!("write: {}", e))?;
    file.write_all(&1u32.to_le_bytes())
        .map_err(|e| format!("write: {}", e))?;
    file.write_all(&(ranges.len() as u32).to_le_bytes())
        .map_err(|e| format!("write: {}", e))?;
    for (start, written) in &ranges {
        // `written` (saturated) — never the raw count — so the header never
        // claims more sectors than the read loop emits below.
        file.write_all(&start.to_le_bytes())
            .map_err(|e| format!("write: {}", e))?;
        file.write_all(&written.to_le_bytes())
            .map_err(|e| format!("write: {}", e))?;
    }

    // Read and write each range
    let chunk: u16 = 32;
    // One reusable read buffer for the whole capture instead of a fresh
    // `vec![0u8; ...]` per chunk (~160 malloc+memset per 5 MB capture). We slice
    // it to the chunk size each iteration and explicitly zero the used slice on
    // a read error to preserve the prior zero-fill-on-error behavior.
    let mut buf = vec![0u8; chunk as usize * SECTOR_SIZE];
    let mut done: u32 = 0;
    let mut errors: u32 = 0;

    let mut last_print: u32 = 0;
    for (ri, (start, written)) in ranges.iter().enumerate() {
        let mut lba = *start;
        // `written` is already the saturated span (end - start) computed once
        // above, so `start + written` is exactly the clamped end and matches
        // what the header advertised; saturating_add stays as belt-and-braces.
        let end = start.saturating_add(*written);

        while lba < end {
            // Clamp the chunk size entirely in u32 BEFORE truncating to u16.
            // `(end - lba) as u16` alone truncates to 0 when the remaining
            // sector count is a nonzero multiple of 65536 (e.g. exactly
            // 65536), so `lba += 0` / `done += 0` would spin forever.
            let n = next_chunk(lba, end, chunk);
            let slice = &mut buf[..n as usize * SECTOR_SIZE];

            match session.read(lba, n, slice, true) {
                // The buffer is reused across iterations, so any bytes the read
                // does not fill retain the previous chunk's contents. On a short
                // Ok read (`bytes < slice.len()`) zero the untouched tail so the
                // fixture records zeros there, not stale neighbor data — matching
                // the per-iteration `vec![0u8; ..]` semantics this reuse replaced.
                // Clamp `bytes` defensively in case the transport over-reports.
                Ok(bytes) => zero_fill_tail(slice, bytes),
                Err(e) => {
                    errors += 1;
                    // Preserve zero-fill-on-error: zero the whole slice we are
                    // about to write so a bad sector is recorded as zeros, not
                    // stale data from a prior chunk.
                    slice.fill(0);
                    if errors <= 3 {
                        eprintln!("\n  Read error at LBA {}: {}", lba, e);
                    }
                }
            }
            file.write_all(slice).map_err(|e| format!("write: {}", e))?;
            lba += n as u32;
            done += n as u32;

            // `done` steps by up to `chunk` (32), so `done % 500` would almost
            // never line up; track the last printed count instead.
            if done - last_print >= 500 || done >= total {
                last_print = done;
                eprint!(
                    "\r  Reading: {} / {} sectors (range {}/{})",
                    done,
                    total,
                    ri + 1,
                    ranges.len()
                );
            }
        }
    }
    eprintln!();
    file.flush().map_err(|e| format!("flush: {}", e))?;

    if errors > 0 {
        println!("  {} read errors (zero-filled)", errors);
    }
    // Compute in u64 to match the surrounding overflow-safe display math (the
    // range-table and progress lines above already u64-cast); usize arithmetic
    // here could overflow on 32-bit targets.
    let file_size = 12u64 + ranges.len() as u64 * 8 + total as u64 * SECTOR_SIZE as u64;
    // Qualify the summary on a partial capture: read errors zero-fill sectors and
    // a hard metadata failure drops a required SCSI structure, so this line must
    // not read as a clean success when fixture_result (below) will return Err.
    if errors > 0 || metadata_errors > 0 {
        println!(
            "  Saved {} sectors ({:.1} MB) in {} ranges (PARTIAL: {} read error{}, {} metadata error{} — see above)",
            total,
            file_size as f64 / 1e6,
            ranges.len(),
            errors,
            if errors == 1 { "" } else { "s" },
            metadata_errors,
            if metadata_errors == 1 { "" } else { "s" },
        );
    } else {
        println!(
            "  Saved {} sectors ({:.1} MB) in {} ranges",
            total,
            file_size as f64 / 1e6,
            ranges.len()
        );
    }

    // Eject disc so user can swap. Gated behind an explicit flag: eject is
    // irreversible on slot-loading drives (the user must physically reload),
    // so we do not fire it automatically on every capture.
    if eject {
        print_step("  Ejecting... ");
        // Don't discard the result: reporting "done" while the disc is still
        // seated (eject failed) is misleading on a slot-loading drive.
        // Keep the result on the same stream as the "  Ejecting... " label
        // (stdout, printed without a trailing newline above): a split
        // println!/eprintln! pair leaves the stdout label dangling and the
        // error on a different stream, which is visibly broken under
        // redirection/piping. fixture_result-driven exit status still reports
        // the overall capture outcome to scripts/CI.
        match session.eject() {
            Ok(()) => println!("done"),
            Err(e) => println!("eject failed: {} (disc may still be seated)", e),
        }
    }

    // A fixture with zero-filled bad sectors — or a required SCSI structure
    // lost to a hard drive/transport fault — is silently corrupt: callers
    // (scripts/CI) must be able to tell a partial capture from a clean one.
    // The bytes are already written above so the partial dump is preserved for
    // inspection, but we surface a non-Ok result and skip the slugified rename
    // (an incomplete capture should not claim a clean disc name).
    fixture_result(errors, metadata_errors)?;

    // Rename output dir to slugified volume ID
    let slug = slugify(&udf.volume_id);
    if !slug.is_empty() {
        let parent = dir.parent().unwrap_or(Path::new("."));
        let mut final_dir = parent.join(&slug);
        let mut n = 2;
        while final_dir.exists() && final_dir != dir {
            final_dir = parent.join(format!("{}_{}", slug, n));
            n += 1;
        }
        if final_dir != dir {
            fs::rename(dir, &final_dir).map_err(|e| format!("rename: {}", e))?;
            println!();
            println!("Capture complete: {}/", final_dir.display());
            return Ok(());
        }
    }
    println!();
    println!("Capture complete: {}/", output_dir);
    Ok(())
}

/// Map the capture's error counts to a result. A non-zero `read_errors` means
/// the fixture has zero-filled (missing) sectors; a non-zero `metadata_errors`
/// means a required SCSI structure (capacity/TOC/PFI/DI) failed for a hard
/// reason (transport/drive fault, not "absent"). Either makes the fixture
/// incomplete, so callers must see a non-zero exit.
fn fixture_result(read_errors: u32, metadata_errors: u32) -> Result<(), String> {
    if read_errors > 0 {
        Err(format!(
            "{} sector read error{} — fixture incomplete (bad sectors zero-filled)",
            read_errors,
            if read_errors == 1 { "" } else { "s" }
        ))
    } else if metadata_errors > 0 {
        Err(format!(
            "{} required SCSI structure{} failed — fixture incomplete",
            metadata_errors,
            if metadata_errors == 1 { "" } else { "s" }
        ))
    } else {
        Ok(())
    }
}

fn slugify(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

/// Capture one SCSI metadata structure to `filename`.
///
/// Returns `true` when a *required* structure failed for a reason that is NOT
/// "structure genuinely absent". A bare `Err(_) => "n/a"` cannot tell an
/// optional structure being missing (fine for BCA/DCB) from a hard transport
/// or drive failure that would also break the required structures — so a
/// capture missing required SCSI metadata used to report clean. Here:
///   - ILLEGAL REQUEST / not-found sense  -> "n/a"   (structure absent; ok)
///   - transport failure / other hard error -> distinct message, and for a
///     required structure the failure is reported back so the caller can fold
///     it into the fixture-completeness result.
fn scsi_save(
    session: &mut Drive,
    dir: &Path,
    filename: &str,
    label: &str,
    cdb: &[u8],
    size: usize,
    required: bool,
) -> bool {
    print_step(&format!("  {}... ", label));
    let mut buf = vec![0u8; size];
    match session.scsi_execute(cdb, scsi::DataDirection::FromDevice, &mut buf, 5000) {
        Ok(r) => {
            // Clamp to the requested size: a buggy/hostile transport that
            // over-reports bytes_transferred would otherwise panic the slice.
            let n = r.bytes_transferred.min(size);
            // Write straight from the buffer slice — every caller discards the
            // bytes, so the intermediate Vec clone was pure waste.
            if let Err(e) = fs::write(dir.join(filename), &buf[..n]) {
                // Keep the label and its result on a single stream (the label
                // was already printed to stdout without a newline): one message,
                // not a split eprintln!/println! pair.
                println!("write error: {}", e);
                // A required structure we couldn't persist is a fixture failure.
                return required;
            }
            println!("{} bytes", n);
            false
        }
        Err(e) => {
            // "structure absent" is the only benign failure (optional BCA/DCB,
            // and even required structures the drive simply doesn't expose).
            let absent = e
                .scsi_sense()
                .map(|s| s.is_illegal_request())
                .unwrap_or(false);
            if absent {
                println!("n/a");
                false
            } else if e.is_scsi_transport_failure() {
                println!("TRANSPORT FAILURE: {}", e);
                // A hard transport error breaks every structure on this drive,
                // not just this one — count it for required structures.
                required
            } else {
                println!("error: {}", e);
                required
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
    /// `slugify` turns a disc's UDF volume ID — a string read off an untrusted
    /// disc — into the directory name the capture is renamed to, so it is the one
    /// place a hostile volume ID could try to escape the output directory. It maps
    /// every non-alphanumeric character to `_`, which makes traversal impossible
    /// by construction (`.` and `/` cannot survive), and this test pins that
    /// property: it catches any mutation that starts preserving separators or dots
    /// (e.g. "allow `.` and `-` for nicer names"), which would turn
    /// `../../etc/cron.d` into a real path again.
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

    /// The everyday behaviour the rename depends on: lowercasing, `_` for spaces
    /// and punctuation, and no leading/trailing underscores (so `sample_film`, not
    /// `_sample_film_`). Catches a mutation that drops the trim or the lowercasing
    /// and silently changes every captured directory's name.
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
}
