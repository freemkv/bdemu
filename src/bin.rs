// bdemu — Blu-ray Drive Emulator CLI
// AGPL-3.0 — freemkv project
//
// Usage:
//   bdemu capture-disc /dev/sg4 profiles/bu40n/discs/my_disc/
//   bdemu capture-disc /dev/sg4 profiles/bu40n/discs/my_disc/ --sectors 50000
//   bdemu validate profiles/bu40n/

mod scsi_probe;
mod capture;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        usage();
        return;
    }

    match args[1].as_str() {
        "capture-disc" => {
            if args.len() < 4 {
                eprintln!("Usage: bdemu capture-disc <device> <output_dir> [--sectors N]");
                std::process::exit(1);
            }
            let device = &args[2];
            let output = &args[3];
            let mut sectors = 10000; // default 20MB

            let mut i = 4;
            while i < args.len() {
                if args[i] == "--sectors" && i + 1 < args.len() {
                    sectors = args[i + 1].parse().unwrap_or(10000);
                    i += 1;
                }
                i += 1;
            }

            if let Err(e) = capture::capture_disc(device, output, sectors) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }

        "validate" => {
            if args.len() < 3 {
                eprintln!("Usage: bdemu validate <profile_dir>");
                std::process::exit(1);
            }
            validate_profile(&args[2]);
        }

        "--help" | "-h" | "help" => usage(),

        _ => {
            eprintln!("Unknown command: {}", args[1]);
            usage();
            std::process::exit(1);
        }
    }
}

fn usage() {
    println!("bdemu — Blu-ray Drive Emulator");
    println!();
    println!("Commands:");
    println!("  capture-disc <device> <dir> [--sectors N]  Capture disc profile from hardware");
    println!("  validate <profile_dir>                     Check profile completeness");
    println!();
    println!("Emulation (LD_PRELOAD):");
    println!("  BDEMU_PROFILE=profiles/bu40n BDEMU_DISC=test_disc \\");
    println!("    LD_PRELOAD=target/release/libbdemu.so makemkvcon ...");
}

fn validate_profile(dir: &str) {
    use std::path::Path;
    let p = Path::new(dir);

    println!("Validating profile: {}", dir);
    println!();

    let mut ok = true;
    let mut warnings = 0;

    // Check drive.toml
    let toml_path = p.join("drive.toml");
    if toml_path.exists() {
        println!("  ✓ drive.toml");
    } else {
        println!("  ✗ drive.toml MISSING");
        ok = false;
    }

    // Check inquiry
    let inq_path = p.join("inquiry.bin");
    if inq_path.exists() {
        let sz = std::fs::metadata(&inq_path).map(|m| m.len()).unwrap_or(0);
        if sz == 96 {
            let data = std::fs::read(&inq_path).unwrap_or_default();
            let vendor = std::str::from_utf8(&data[8..16]).unwrap_or("?").trim();
            let product = std::str::from_utf8(&data[16..32]).unwrap_or("?").trim();
            println!("  ✓ inquiry.bin ({} bytes) — {} {}", sz, vendor, product);
        } else {
            println!("  ⚠ inquiry.bin ({} bytes, expected 96)", sz);
            warnings += 1;
        }
    } else {
        println!("  ✗ inquiry.bin MISSING");
        ok = false;
    }

    // Check key features
    let features: &[(&str, u16, &str)] = &[
        ("gc_0000.bin", 0x0000, "Profile List"),
        ("gc_0108.bin", 0x0108, "Serial Number"),
        ("gc_010c.bin", 0x010C, "Firmware Information"),
    ];
    for (file, code, name) in features {
        let fp = p.join(file);
        if fp.exists() {
            let sz = std::fs::metadata(&fp).map(|m| m.len()).unwrap_or(0);
            let mut extra = String::new();
            if *code == 0x010C {
                let data = std::fs::read(&fp).unwrap_or_default();
                if data.len() > 4 {
                    let date = std::str::from_utf8(&data[4..16.min(data.len())]).unwrap_or("?");
                    extra = format!(" — date: {}", date);
                }
            }
            if *code == 0x0108 {
                let data = std::fs::read(&fp).unwrap_or_default();
                if data.len() > 4 {
                    let serial = std::str::from_utf8(&data[4..]).unwrap_or("?").trim();
                    extra = format!(" — serial: {}", serial);
                }
            }
            println!("  ✓ {} (0x{:04X} {}, {} bytes){}", file, code, name, sz, extra);
        } else {
            println!("  ✗ {} (0x{:04X} {}) MISSING", file, code, name);
            ok = false;
        }
    }

    // Count total features
    let feat_count = std::fs::read_dir(p)
        .map(|entries| entries.flatten().filter(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.starts_with("gc_") && n.ends_with(".bin")
        }).count())
        .unwrap_or(0);
    println!("  ✓ {} total features", feat_count);

    // Check optional files
    for (file, desc) in &[
        ("rpc_state.bin", "REPORT KEY RPC"),
        ("mode_2a.bin", "MODE SENSE 2A"),
        ("rb_f1.bin", "READ_BUFFER 0xF1 (Pioneer)"),
    ] {
        if p.join(file).exists() {
            let sz = std::fs::metadata(p.join(file)).map(|m| m.len()).unwrap_or(0);
            println!("  ✓ {} ({}, {} bytes)", file, desc, sz);
        } else {
            println!("  — {} ({}) not present", file, desc);
        }
    }

    // Check discs
    let discs_dir = p.join("discs");
    if discs_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&discs_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let has_sectors = entry.path().join("sectors.bin").exists();
                    let has_toc = entry.path().join("toc.bin").exists();
                    println!("  ✓ disc: {} (toc={}, sectors={})", name, has_toc, has_sectors);
                }
            }
        }
    } else {
        println!("  — No disc profiles");
        warnings += 1;
    }

    println!();
    if ok {
        println!("Profile OK ({} warnings)", warnings);
    } else {
        println!("Profile INCOMPLETE — missing required files");
    }
}
