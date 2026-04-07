// bdemu — Blu-ray Drive Emulator
// AGPL-3.0 — freemkv project
//
// Smart disc capture — uses libfreemkv to open/unlock the drive,
// parses UDF to find metadata locations, captures 0 → max_metadata + 1000.
// Result: flat sectors.bin that bdemu serves as a virtual disc.

use std::fs;
use std::io::Write;
use std::path::Path;
use libfreemkv::{DriveSession, scsi};

const SECTOR_SIZE: usize = 2048;
const PADDING_SECTORS: u32 = 1000;
/// Ranges below this LBA are metadata. Above = video region outliers.
const METADATA_CUTOFF: u32 = 500_000;

pub fn capture_disc(device: &str, output_dir: &str, manual_sectors: usize) -> Result<(), String> {
    let dir = Path::new(output_dir);
    fs::create_dir_all(dir).map_err(|e| format!("create dir: {}", e))?;

    println!("Capturing disc from {} -> {}/", device, output_dir);
    println!();

    let dev_path = Path::new(device);
    let mut session = DriveSession::open(dev_path)
        .map_err(|e| format!("open drive: {}", e))?;

    println!("  Drive: {} {} {}",
        session.drive_id.vendor_id.trim(),
        session.drive_id.product_id.trim(),
        session.drive_id.product_revision.trim());
    println!("  Unlocked: {}", session.is_unlocked());

    // SCSI metadata
    println!();
    let cap_data = scsi_save(&mut session, dir, "capacity.bin", "READ_CAPACITY",
        &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], 8);
    let disc_capacity = cap_data.map(|d|
        u32::from_be_bytes([d[0], d[1], d[2], d[3]]) + 1
    ).unwrap_or(0);

    scsi_save(&mut session, dir, "toc.bin", "READ_TOC",
        &[0x43, 0, 0, 0, 0, 0, 0, 0x10, 0, 0], 4096);
    scsi_save(&mut session, dir, "disc_info.bin", "READ_DISC_INFO",
        &[0x51, 0, 0, 0, 0, 0, 0, 0x01, 0, 0], 256);
    for (fmt, name) in &[(0x00u8,"PFI"),(0x01,"DI"),(0x03,"BCA"),(0x0E,"CR"),(0x0F,"CR")] {
        let cdb = [0xAD, 0x01, 0, 0, 0, 0, 0, *fmt, 0x10, 0x04, 0, 0];
        scsi_save(&mut session, dir, &format!("ds_{:02x}.bin", fmt),
            &format!("DISC_STRUCTURE 0x{:02X} ({})", fmt, name), &cdb, 4100);
    }

    // Determine sector count
    println!();
    let sector_count = if manual_sectors > 0 {
        println!("  Manual: {} sectors", manual_sectors);
        manual_sectors as u32
    } else {
        print!("  Parsing UDF... ");
        let udf = libfreemkv::udf::read_filesystem(&mut session)
            .map_err(|e| format!("UDF: {}", e))?;
        println!("done");

        print!("  Finding metadata extent... ");
        let ranges = udf.metadata_sector_ranges(&mut session)
            .map_err(|e| format!("ranges: {}", e))?;

        // Max of all ranges in the metadata region (< 500K sectors)
        let max_end = ranges.iter()
            .filter(|(start, _)| *start < METADATA_CUTOFF)
            .map(|(start, count)| start + count)
            .max()
            .unwrap_or(10000);

        let count = (max_end + PADDING_SECTORS).min(disc_capacity);
        println!("sector {} + {} = {}", max_end, PADDING_SECTORS, count);
        println!("  Capture size: {:.1} MB", count as f64 * SECTOR_SIZE as f64 / 1e6);

        // Report skipped outliers
        let skipped: Vec<_> = ranges.iter()
            .filter(|(start, _)| *start >= METADATA_CUTOFF)
            .collect();
        if !skipped.is_empty() {
            println!("  Skipping {} high-LBA ranges (Content*.cer, JAR PNGs — AACS 2.0 future):", skipped.len());
            for (s, c) in &skipped {
                println!("    LBA {}-{} ({} sectors)", s, s+c, c);
            }
        }
        count
    };

    // Read sectors — incremental write
    println!();
    let mut file = fs::File::create(dir.join("sectors.bin"))
        .map_err(|e| format!("create: {}", e))?;
    let chunk: u16 = 32;
    let mut lba: u32 = 0;
    let mut errors: u32 = 0;

    while lba < sector_count {
        let n = ((sector_count - lba) as u16).min(chunk);
        let mut buf = vec![0u8; n as usize * SECTOR_SIZE];

        if let Err(e) = session.read_disc(lba, n, &mut buf) {
            errors += 1;
            if errors <= 3 {
                eprintln!("\n  Read error at LBA {}: {}", lba, e);
            }
        }
        file.write_all(&buf).map_err(|e| format!("write: {}", e))?;
        lba += n as u32;

        if lba % 5000 == 0 || lba >= sector_count {
            eprint!("\r  Reading: {} / {} sectors ({:.0}%)",
                lba, sector_count, lba as f64 / sector_count as f64 * 100.0);
        }
    }
    eprintln!();
    file.flush().map_err(|e| format!("flush: {}", e))?;

    if errors > 0 {
        println!("  {} read errors (zero-filled)", errors);
    }
    println!("  Saved {:.1} MB", sector_count as f64 * SECTOR_SIZE as f64 / 1e6);
    println!();
    println!("Capture complete: {}/", output_dir);
    Ok(())
}

fn scsi_save(session: &mut DriveSession, dir: &Path, file: &str, label: &str,
    cdb: &[u8], size: usize) -> Option<Vec<u8>>
{
    print!("  {}... ", label);
    let mut buf = vec![0u8; size];
    match session.scsi_execute(cdb, scsi::DataDirection::FromDevice, &mut buf, 5000) {
        Ok(r) => {
            let data = buf[..r.bytes_transferred].to_vec();
            let _ = fs::write(dir.join(file), &data);
            println!("{} bytes", r.bytes_transferred);
            Some(data)
        }
        Err(_) => { println!("n/a"); None }
    }
}
