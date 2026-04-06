// bdemu — Blu-ray Drive Emulator
// AGPL-3.0 — freemkv project
//
// Disc profile capture — reads disc data from real hardware

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

pub fn capture_disc(device: &str, output_dir: &str, sector_count: usize) -> io::Result<()> {
    let dir = Path::new(output_dir);
    fs::create_dir_all(dir)?;

    println!("Capturing disc from {} -> {}/", device, output_dir);
    println!();

    // Open the device for SG_IO commands
    let sg_dev = super::scsi_probe::ScsiDevice::open(device)?;

    // READ_CAPACITY
    print!("  READ_CAPACITY... ");
    if let Some(data) = sg_dev.command(&[0x25, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], 8) {
        fs::write(dir.join("capacity.bin"), &data)?;
        let lba = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let blk = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        println!("{} sectors, {} bytes/sector", lba + 1, blk);
    } else {
        println!("failed");
    }

    // READ_TOC
    print!("  READ_TOC... ");
    if let Some(data) = sg_dev.command(&[0x43, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00], 4096) {
        fs::write(dir.join("toc.bin"), &data)?;
        println!("{} bytes", data.len());
    } else {
        println!("failed");
    }

    // READ_DISC_INFO
    print!("  READ_DISC_INFO... ");
    if let Some(data) = sg_dev.command(&[0x51, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00], 256) {
        fs::write(dir.join("disc_info.bin"), &data)?;
        println!("{} bytes", data.len());
    } else {
        println!("failed");
    }

    // READ_DISC_STRUCTURE — try common formats
    let formats: &[(u8, &str)] = &[
        (0x00, "PFI"),
        (0x01, "DI"),
        (0x03, "BCA"),
        (0x0E, "Copyright"),
        (0x0F, "Copyright"),
    ];
    for (fmt, name) in formats {
        print!("  DISC_STRUCTURE format {} ({})... ", fmt, name);
        let cdb = [0xAD, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, *fmt, 0x10, 0x04, 0x00, 0x00];
        if let Some(data) = sg_dev.command(&cdb, 4100) {
            let fname = format!("ds_{:02x}.bin", fmt);
            fs::write(dir.join(&fname), &data)?;
            println!("{} bytes", data.len());
        } else {
            println!("not available");
        }
    }

    // Sector dump — read from block device
    println!();
    println!("  Reading {} sectors ({:.1} MB)...",
             sector_count, sector_count as f64 * 2048.0 / 1_000_000.0);

    // Use the block device (sr0) not sg device for bulk reads
    let sr_device = device.replace("sg", "sr");
    let sr_path = if Path::new(&sr_device).exists() {
        sr_device
    } else {
        // Fall back: find /dev/sr* that matches
        let mut found = String::new();
        for i in 0..16 {
            let p = format!("/dev/sr{}", i);
            if Path::new(&p).exists() {
                found = p;
                break;
            }
        }
        if found.is_empty() {
            eprintln!("  Cannot find block device for sector reads");
            eprintln!("  Skipping sector dump");
            return Ok(());
        }
        found
    };

    let mut file = fs::File::open(&sr_path)?;
    let sector_size = 2048;
    let total_bytes = sector_count * sector_size;
    let mut buf = vec![0u8; total_bytes];

    let bytes_read = file.read(&mut buf)?;
    buf.truncate(bytes_read);

    let sectors_read = bytes_read / sector_size;
    fs::write(dir.join("sectors.bin"), &buf)?;
    println!("  Read {} sectors ({:.1} MB) -> sectors.bin",
             sectors_read, bytes_read as f64 / 1_000_000.0);

    println!();
    println!("Disc profile saved to {}/", output_dir);
    println!("Files:");
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let meta = entry.metadata()?;
            println!("  {}: {} bytes", entry.file_name().to_string_lossy(), meta.len());
        }
    }

    Ok(())
}
