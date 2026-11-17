// bdemu — Blu-ray Drive Emulator, MIT — freemkv project
// Smart disc capture: opens/unlocks via libfreemkv, parses UDF for metadata
// ranges, and writes only the sparse BDSM sectors disc-info needs (~5MB/disc).

use libfreemkv::{Drive, scsi};
use std::fs;
use std::io::Write;
use std::path::Path;

// Duplicates profile::SECTOR_SIZE: this binary's cdylib sibling crate-type
// means capture.rs can't `use crate::profile` like the lib side does.
const SECTOR_SIZE: usize = 2048;

// Print a partial-line progress label and flush it immediately: stdout is
// line-buffered on a TTY, so an unflushed print! would stay hidden until the
// following (often slow) op finishes, defeating the "in progress" indicator.
fn print_step(msg: &str) {
    print!("{}", msg);
    let _ = std::io::stdout().flush();
}

// Sectors to read next: at most `chunk`, never more than remains (end - lba).
// Clamped in u32 before the u16 cast so a remaining count that's a nonzero
// multiple of 65536 can't truncate to 0 (which would spin the read loop).
fn next_chunk(lba: u32, end: u32, chunk: u16) -> u16 {
    (end - lba).min(chunk as u32) as u16
}

fn zero_fill_tail(buf: &mut [u8], transferred: usize) {
    let filled = transferred.min(buf.len());
    buf[filled..].fill(0);
}

/// Capture a disc's emulation fixture from `device` (e.g. `/dev/sr0`) into
/// `output_dir`, which is created if absent.
///
/// On success the directory holds the SCSI response fixtures plus the sparse
/// metadata sectors disc-info needs, enough to replay the disc without the
/// physical media. If `eject` is set, the drive is ejected after capture.
/// On a clean capture, `output_dir` is renamed to a slugified form of the
/// disc's UDF volume ID. Returns `Err` if the capture was partial.
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

    // SCSI metadata. `metadata_errors` counts hard (transport/non-ILLEGAL-
    // REQUEST) failures on *required* structures, so a drive fault that drops
    // capacity/TOC/PFI/DI is reported as incomplete, not clean.
    println!();
    let mut exec = |cdb: &[u8], buf: &mut [u8]| {
        session
            .scsi_execute(cdb, scsi::DataDirection::FromDevice, buf, 5000)
            .map(|r| r.bytes_transferred)
    };
    let metadata_errors = capture_metadata(&mut exec, dir);

    // Parse UDF, discover metadata ranges
    println!();
    print_step("  Parsing UDF... ");
    let udf = libfreemkv::read_filesystem(&mut session).map_err(|e| format!("UDF: {}", e))?;
    println!("done");

    print_step("  Discovering metadata ranges... ");
    let raw_ranges = udf
        .metadata_sector_ranges(&mut session)
        .map_err(|e| format!("ranges: {}", e))?;

    let ranges = clamp_ranges(&raw_ranges);

    // Saturating sum so a hostile set of ranges cannot overflow-panic here.
    let total: u32 = ranges.iter().fold(0u32, |acc, r| acc.saturating_add(r.1));
    println!(
        "{} ranges, {} sectors ({:.1} MB)",
        ranges.len(),
        total,
        total as f64 * SECTOR_SIZE as f64 / 1e6
    );
    for (i, (s, written)) in ranges.iter().enumerate() {
        println!("{}", range_line(i, *s, *written));
    }

    // Write BDSM sparse sector map
    println!();
    let mut file =
        fs::File::create(dir.join("sectors.bin")).map_err(|e| format!("create: {}", e))?;
    write_bdsm_header(&mut file, &ranges).map_err(|e| format!("write: {}", e))?;

    let errors = copy_ranges(&ranges, total, &mut file, |lba, n, slice| {
        session.read(lba, n, slice, true).map_err(|e| e.to_string())
    })?;
    eprintln!();
    file.flush().map_err(|e| format!("flush: {}", e))?;

    for line in capture_summary(total, ranges.len(), errors, metadata_errors) {
        println!("{}", line);
    }

    // Eject disc so user can swap. Gated behind an explicit flag: eject is
    // irreversible on slot-loading drives (the user must physically reload),
    // so we do not fire it automatically on every capture.
    if eject {
        print_step("  Ejecting... ");
        // Don't discard the result: reporting "done" while the disc is still
        // seated is misleading. Print on stdout (same stream as the label,
        // no trailing newline above) so redirection/piping doesn't split them.
        match session.eject() {
            Ok(()) => println!("done"),
            Err(e) => println!("eject failed: {} (disc may still be seated)", e),
        }
    }

    // A fixture with zero-filled sectors or a lost required SCSI structure is
    // silently corrupt: callers (scripts/CI) must tell a partial capture from
    // a clean one. Bytes stay written for inspection, but skip the slug rename.
    fixture_result(errors, metadata_errors)?;

    // Rename output dir to slugified volume ID
    let final_dir = rename_to_slug(dir, &udf.volume_id)?;
    println!();
    println!("Capture complete: {}/", final_dir.display());
    Ok(())
}

// Capture every SCSI metadata structure; returns the count of hard failures
// on required structures.
fn capture_metadata(exec: &mut Exec<'_>, dir: &Path) -> u32 {
    let mut metadata_errors: u32 = 0;
    // (filename, label, cdb, size, required)
    metadata_errors += scsi_save(
        exec,
        dir,
        "capacity.bin",
        "READ_CAPACITY",
        &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        8,
        true,
    ) as u32;
    metadata_errors += scsi_save(
        exec,
        dir,
        "toc.bin",
        "READ_TOC",
        &[0x43, 0, 0, 0, 0, 0, 0, 0x10, 0, 0],
        4096,
        true,
    ) as u32;
    metadata_errors += scsi_save(
        exec,
        dir,
        "disc_info.bin",
        "READ_DISC_INFO",
        &[0x51, 0, 0, 0, 0, 0, 0, 0x01, 0, 0],
        256,
        true,
    ) as u32;
    // PFI (0x00) and DI (0x01) are required; BCA (0x03) and the disc control /
    // read-only physical-format structures are optional (-> "n/a").
    for (fmt, name, required) in &[
        (0x00u8, "PFI", true),
        (0x01, "DI", true),
        (0x03, "BCA", false),
        // 0x0E and 0x0F are distinct formats and must not share a label.
        (0x0E, "DCB", false),    // Disc Control Block
        (0x0F, "PFI_RO", false), // Physical Format Information (read-only)
    ] {
        let cdb = [0xAD, 0x01, 0, 0, 0, 0, 0, *fmt, 0x10, 0x04, 0, 0];
        metadata_errors += scsi_save(
            exec,
            dir,
            &format!("ds_{:02x}.bin", fmt),
            &format!("DISC_STRUCTURE 0x{:02X} ({})", fmt, name),
            &cdb,
            4100,
            *required,
        ) as u32;
    }
    metadata_errors
}

// Single source of truth for sectors each range contributes: `end`
// saturates, so a hostile range can make raw `count` overstate `written`.
fn clamp_ranges(raw: &[(u32, u32)]) -> Vec<(u32, u32)> {
    raw.iter()
        .map(|(start, count)| (*start, start.saturating_add(*count) - start))
        .collect()
}

fn range_line(i: usize, start: u32, written: u32) -> String {
    // u64 so a hostile start+count near u32::MAX cannot overflow.
    let end = start as u64 + written as u64;
    format!(
        "    range {}: LBA {}-{} ({} sectors)",
        i, start, end, written
    )
}

// BDSM header: magic, version, range count, then (start, written) pairs.
fn write_bdsm_header<W: Write>(w: &mut W, ranges: &[(u32, u32)]) -> std::io::Result<()> {
    w.write_all(b"BDSM")?;
    w.write_all(&1u32.to_le_bytes())?;
    w.write_all(&(ranges.len() as u32).to_le_bytes())?;
    for (start, written) in ranges {
        w.write_all(&start.to_le_bytes())?;
        w.write_all(&written.to_le_bytes())?;
    }
    Ok(())
}

// Progress is due every 500 sectors and on the final chunk.
fn progress_due(done: u32, last_print: u32, total: u32) -> bool {
    done - last_print >= 500 || done >= total
}

// Read every range in `chunk`-sized pieces and append the sectors to `out`;
// returns the read-error count (bad chunks are written as zeros).
fn copy_ranges<W: Write>(
    ranges: &[(u32, u32)],
    total: u32,
    out: &mut W,
    mut read: impl FnMut(u32, u16, &mut [u8]) -> Result<usize, String>,
) -> Result<u32, String> {
    let chunk: u16 = 32;
    // One reusable buffer, sliced per chunk and zeroed on error.
    let mut buf = vec![0u8; chunk as usize * SECTOR_SIZE];
    let mut done: u32 = 0;
    let mut errors: u32 = 0;
    let mut last_print: u32 = 0;
    for (ri, (start, written)) in ranges.iter().enumerate() {
        let mut lba = *start;
        let end = start.saturating_add(*written);

        while lba < end {
            // next_chunk clamps in u32 so the u16 chunk can never be 0.
            let n = next_chunk(lba, end, chunk);
            let slice = &mut buf[..n as usize * SECTOR_SIZE];

            match read(lba, n, slice) {
                // Reused buffer: zero any tail a short read left stale.
                Ok(bytes) => zero_fill_tail(slice, bytes),
                Err(e) => {
                    errors += 1;
                    // Record a bad chunk as zeros, not a prior chunk's data.
                    slice.fill(0);
                    if errors <= 3 {
                        eprintln!("\n  Read error at LBA {}: {}", lba, e);
                    }
                }
            }
            out.write_all(slice).map_err(|e| format!("write: {}", e))?;
            lba += n as u32;
            done += n as u32;

            if progress_due(done, last_print, total) {
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
    Ok(errors)
}

// Bytes the BDSM sectors.bin occupies: header, range table, sector payload.
// u64 so the size cannot overflow on 32-bit targets.
fn sparse_file_size(n_ranges: usize, total: u32) -> u64 {
    12u64 + n_ranges as u64 * 8 + total as u64 * SECTOR_SIZE as u64
}

// The end-of-capture summary lines. A partial capture must not read as clean.
fn capture_summary(total: u32, n_ranges: usize, errors: u32, metadata_errors: u32) -> Vec<String> {
    let mut lines = Vec::new();
    if errors > 0 {
        lines.push(format!("  {} read errors (zero-filled)", errors));
    }
    let mb = sparse_file_size(n_ranges, total) as f64 / 1e6;
    if errors > 0 || metadata_errors > 0 {
        lines.push(format!(
            "  Saved {} sectors ({:.1} MB) in {} ranges (PARTIAL: {} read error{}, {} metadata error{} — see above)",
            total,
            mb,
            n_ranges,
            errors,
            if errors == 1 { "" } else { "s" },
            metadata_errors,
            if metadata_errors == 1 { "" } else { "s" },
        ));
    } else {
        lines.push(format!(
            "  Saved {} sectors ({:.1} MB) in {} ranges",
            total, mb, n_ranges
        ));
    }
    lines
}

// Rename `dir` to the slugified volume ID (with _2, _3... on collision);
// returns the final directory, unchanged when the slug is empty.
fn rename_to_slug(dir: &Path, volume_id: &str) -> Result<std::path::PathBuf, String> {
    let slug = slugify(volume_id);
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
            return Ok(final_dir);
        }
    }
    Ok(dir.to_path_buf())
}
// Map the capture's error counts to a result: a non-zero `read_errors` means
// zero-filled sectors, a non-zero `metadata_errors` means a required SCSI
// structure failed for a hard (not "absent") reason. Either is incomplete.
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

// Runs one CDB into a buffer, returning the bytes transferred.
type Exec<'a> = dyn FnMut(&[u8], &mut [u8]) -> libfreemkv::Result<usize> + 'a;

// Capture one SCSI metadata structure to `filename`; returns true on a hard failure of a
// *required* structure.
fn scsi_save(
    exec: &mut Exec<'_>,
    dir: &Path,
    filename: &str,
    label: &str,
    cdb: &[u8],
    size: usize,
    required: bool,
) -> bool {
    print_step(&format!("  {}... ", label));
    let mut buf = vec![0u8; size];
    match exec(cdb, &mut buf) {
        Ok(transferred) => {
            // Clamp to the requested size: a buggy/hostile transport that
            // over-reports bytes_transferred would otherwise panic the slice.
            let n = transferred.min(size);
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
#[path = "capture_tests.rs"]
mod tests;
