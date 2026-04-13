// bdemu — Blu-ray Drive Emulator
// AGPL-3.0 — freemkv project
//
// Smart disc capture — uses libfreemkv to open/unlock the drive,
// parses UDF to find metadata sector ranges, writes sparse BDSM format.
// Only captures sectors disc-info needs (~5MB per disc).

use std::fs;
use std::io::Write;
use std::path::Path;
use libfreemkv::{Drive, scsi};

const SECTOR_SIZE: usize = 2048;

pub fn capture_disc(device: &str, output_dir: &str) -> Result<(), String> {
    let dir = Path::new(output_dir);
    fs::create_dir_all(dir).map_err(|e| format!("create dir: {}", e))?;

    println!("Capturing disc from {} -> {}/", device, output_dir);
    println!();

    let dev_path = Path::new(device);
    let mut session = Drive::open(dev_path)
        .map_err(|e| format!("open drive: {}", e))?;

    println!("  Drive: {} {} {}",
        session.drive_id.vendor_id.trim(),
        session.drive_id.product_id.trim(),
        session.drive_id.product_revision.trim());

    // SCSI metadata
    println!();
    scsi_save(&mut session, dir, "capacity.bin", "READ_CAPACITY",
        &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], 8);
    scsi_save(&mut session, dir, "toc.bin", "READ_TOC",
        &[0x43, 0, 0, 0, 0, 0, 0, 0x10, 0, 0], 4096);
    scsi_save(&mut session, dir, "disc_info.bin", "READ_DISC_INFO",
        &[0x51, 0, 0, 0, 0, 0, 0, 0x01, 0, 0], 256);
    for (fmt, name) in &[(0x00u8,"PFI"),(0x01,"DI"),(0x03,"BCA"),(0x0E,"CR"),(0x0F,"CR")] {
        let cdb = [0xAD, 0x01, 0, 0, 0, 0, 0, *fmt, 0x10, 0x04, 0, 0];
        scsi_save(&mut session, dir, &format!("ds_{:02x}.bin", fmt),
            &format!("DISC_STRUCTURE 0x{:02X} ({})", fmt, name), &cdb, 4100);
    }

    // Parse UDF, discover metadata ranges
    println!();
    print!("  Parsing UDF... ");
    let udf = libfreemkv::read_filesystem(&mut session)
        .map_err(|e| format!("UDF: {}", e))?;
    println!("done");

    print!("  Discovering metadata ranges... ");
    let ranges = udf.metadata_sector_ranges(&mut session)
        .map_err(|e| format!("ranges: {}", e))?;

    let total: u32 = ranges.iter().map(|r| r.1).sum();
    println!("{} ranges, {} sectors ({:.1} MB)",
        ranges.len(), total, total as f64 * SECTOR_SIZE as f64 / 1e6);
    for (i, (s, c)) in ranges.iter().enumerate() {
        println!("    range {}: LBA {}-{} ({} sectors)", i, s, s+c, c);
    }

    // Write BDSM sparse sector map
    println!();
    let mut file = fs::File::create(dir.join("sectors.bin"))
        .map_err(|e| format!("create: {}", e))?;

    // Header
    file.write_all(b"BDSM").map_err(|e| format!("write: {}", e))?;
    file.write_all(&1u32.to_le_bytes()).map_err(|e| format!("write: {}", e))?;
    file.write_all(&(ranges.len() as u32).to_le_bytes()).map_err(|e| format!("write: {}", e))?;
    for (start, count) in &ranges {
        file.write_all(&start.to_le_bytes()).map_err(|e| format!("write: {}", e))?;
        file.write_all(&count.to_le_bytes()).map_err(|e| format!("write: {}", e))?;
    }

    // Read and write each range
    let chunk: u16 = 32;
    let mut done: u32 = 0;
    let mut errors: u32 = 0;

    for (ri, (start, count)) in ranges.iter().enumerate() {
        let mut lba = *start;
        let end = start + count;

        while lba < end {
            let n = ((end - lba) as u16).min(chunk);
            let mut buf = vec![0u8; n as usize * SECTOR_SIZE];

            if let Err(e) = session.read(lba, n, &mut buf) {
                errors += 1;
                if errors <= 3 {
                    eprintln!("\n  Read error at LBA {}: {}", lba, e);
                }
            }
            file.write_all(&buf).map_err(|e| format!("write: {}", e))?;
            lba += n as u32;
            done += n as u32;

            if done % 500 == 0 || done >= total {
                eprint!("\r  Reading: {} / {} sectors (range {}/{})",
                    done, total, ri + 1, ranges.len());
            }
        }
    }
    eprintln!();
    file.flush().map_err(|e| format!("flush: {}", e))?;

    if errors > 0 {
        println!("  {} read errors (zero-filled)", errors);
    }
    let file_size = 12 + ranges.len() * 8 + total as usize * SECTOR_SIZE;
    println!("  Saved {} sectors ({:.1} MB) in {} ranges",
        total, file_size as f64 / 1e6, ranges.len());

    // Eject disc so user can swap
    print!("  Ejecting... ");
    let _ = session.eject();
    println!("done");

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

fn slugify(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn scsi_save(session: &mut Drive, dir: &Path, filename: &str, label: &str,
    cdb: &[u8], size: usize) -> Option<Vec<u8>>
{
    print!("  {}... ", label);
    let mut buf = vec![0u8; size];
    match session.scsi_execute(cdb, scsi::DataDirection::FromDevice, &mut buf, 5000) {
        Ok(r) => {
            let data = buf[..r.bytes_transferred].to_vec();
            let _ = fs::write(dir.join(filename), &data);
            println!("{} bytes", r.bytes_transferred);
            Some(data)
        }
        Err(_) => { println!("n/a"); None }
    }
}
