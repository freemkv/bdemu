#![no_main]
#![allow(dead_code)]
use libfuzzer_sys::fuzz_target;
#[path = "../../src/profile.rs"]
mod profile;
fuzz_target!(|data: &[u8]| {
    if data.len() > 1024 * 1024 {
        return;
    }
    let (bytes, ranges) = profile::parse_sector_file(data.to_vec());
    for (_, count, offset) in ranges {
        assert!(offset <= bytes.len());
        assert!(count as usize <= (bytes.len() - offset) / profile::SECTOR_SIZE);
    }
});
