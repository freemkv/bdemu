// bdemu — Blu-ray Drive Emulator — MIT — freemkv project
// Drive profile loader — directory-based with .bin files + TOML metadata

use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Logical block size of every optical medium bdemu emulates, and the unit the
/// BDSM sector map, the flat legacy dump and every READ(10)/READ(12) transfer are
/// expressed in. It was an inline `2048` in the sector-map parser, the READ
/// handler, the sector-map lookup and the disc-size accounting; a named constant
/// keeps those four in step (and makes it obvious that a `/ 2048` there is a
/// sector count, not an arbitrary block).
pub const SECTOR_SIZE: usize = 2048;

/// Loaded profile with raw bytes ready to serve
pub struct LoadedProfile {
    pub name: String,
    pub inquiry: Vec<u8>,
    pub current_profile: u16,
    pub features: Vec<(u16, Vec<u8>)>,
    pub rpc_state: Vec<u8>,
    pub read_bufs: Vec<(u8, Vec<u8>)>,
    pub mode_2a: Vec<u8>,
    pub disc: Option<DiscProfile>,
}

pub struct DiscProfile {
    pub toc: Vec<u8>,
    pub capacity: Vec<u8>,
    pub disc_info: Vec<u8>,
    pub disc_structures: HashMap<u8, Vec<u8>>, // format_code -> data
    pub sector_data: Vec<u8>,                  // single sector pattern (repeated)
    pub sectors: Vec<u8>,                      // sector data (flat or sparse)
    pub sector_map: Vec<(u32, u32, usize)>,    // (start_lba, count, byte_offset) — empty = flat
}

/// Sector map file format:
///   Magic: "BDSM" (4 bytes)
///   Version: u32 LE (1)
///   Num_ranges: u32 LE
///   Ranges: [start_lba(u32 LE), sector_count(u32 LE)] × num_ranges
///   Sector data: contiguous, in range order
///
/// If magic is NOT "BDSM", the file is a flat sector dump (legacy, LBA = offset/2048).
pub fn parse_sector_file(data: Vec<u8>) -> (Vec<u8>, Vec<(u32, u32, usize)>) {
    if data.len() >= 12 && &data[0..4] == b"BDSM" {
        // Every corrupt/hostile bail-out here returns EMPTY sectors, never `(data,
        // Vec::new())`: the header/range table isn't sector 0, so falling back to
        // "flat" would serve that header as sector 0 at GOOD status.
        let _version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let num_ranges = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;

        // `num_ranges` is untrusted: a value like 0xFFFFFFFF would make
        // `with_capacity` OOM-abort and `12 + num_ranges*8` overflow. Clamp to
        // what the file's own byte length can actually describe (8B/range).
        let max_ranges = data.len().saturating_sub(12) / 8;
        if num_ranges > max_ranges {
            return (Vec::new(), Vec::new()); // corrupt/hostile: serve nothing
        }

        // header_size = 12 + num_ranges*8; bounded by max_ranges above so this
        // cannot overflow, but verify against the buffer anyway.
        let header_size = match num_ranges.checked_mul(8).and_then(|h| h.checked_add(12)) {
            Some(h) if h <= data.len() => h,
            _ => return (Vec::new(), Vec::new()),
        };

        let mut map = Vec::with_capacity(num_ranges);
        let mut data_offset = header_size;
        for i in 0..num_ranges {
            let off = 12 + i * 8;
            let start_lba =
                u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
            let count =
                u32::from_le_bytes([data[off + 4], data[off + 5], data[off + 6], data[off + 7]]);

            // The declared sector bytes must actually exist in `data`, or
            // lookup_sector/READ would slice out of bounds and panic. Bail
            // (serve nothing, per above) on overflow or truncation instead.
            let span = match (count as usize).checked_mul(SECTOR_SIZE) {
                Some(s) => s,
                None => return (Vec::new(), Vec::new()),
            };
            let next_offset = match data_offset.checked_add(span) {
                Some(o) if o <= data.len() => o,
                _ => return (Vec::new(), Vec::new()),
            };

            // Drop zero-count ranges: they capture nothing yet, left in, would
            // slip past the overlap guard (end == start) and leave lookup_sector
            // a non-monotonic map. Dropping them keeps the binary search sound.
            if count > 0 {
                map.push((start_lba, count, data_offset));
            }
            data_offset = next_offset;
        }

        // lookup_sector (scsi.rs) binary-searches this map, requiring ascending
        // start_lba order. Without this sort, out-of-order ranges from a capture
        // tool would make binary_search miss present sectors and zero-fill them.
        map.sort_by_key(|&(start, _, _)| start);

        // Reject OVERLAPPING ranges: lookup_sector assumes DISJOINT ranges, so an
        // overlap (bug or hostile profile) could serve the WRONG range as GOOD.
        // Bail to empty on overlap (touching is OK; `end` uses u64 to avoid wrap).
        for w in map.windows(2) {
            let (start_prev, count_prev, _) = w[0];
            let (start_next, _, _) = w[1];
            let end_prev = start_prev as u64 + count_prev as u64;
            if (start_next as u64) < end_prev {
                return (Vec::new(), Vec::new());
            }
        }

        // A BDSM file declaring zero ranges captured no sectors. Return empty
        // rather than the raw buffer — otherwise scsi.rs mistakes the empty
        // map for a legacy flat dump and serves the header bytes as sector 0.
        if map.is_empty() {
            return (Vec::new(), Vec::new());
        }
        (data, map)
    } else {
        // Legacy flat format
        (data, Vec::new())
    }
}

impl LoadedProfile {
    pub fn load(path: &str) -> Option<Self> {
        let p = Path::new(path);

        // Support both: directory with drive.toml + .bin files, or single .json
        if p.is_dir() {
            Self::load_dir(p)
        } else if path.ends_with(".json") {
            Self::load_json(path)
        } else {
            eprintln!("bdemu: unknown profile format: {}", path);
            None
        }
    }

    fn load_dir(dir: &Path) -> Option<Self> {
        let toml_path = dir.join("drive.toml");
        let toml_str = fs::read_to_string(&toml_path)
            .map_err(|e| eprintln!("bdemu: cannot read {:?}: {}", toml_path, e))
            .ok()?;

        // Simple TOML parsing — just extract key = value pairs
        let mut name = String::new();
        let mut current_profile: u16 = 0x0043;
        let mut feature_files: Vec<(u16, String)> = Vec::new();
        let mut inquiry_file = String::from("inquiry.bin");
        let mut rpc_file = String::new();
        let mut section = String::new();
        let mut rb_files: Vec<(u8, String)> = Vec::new();
        let mut mode_2a_file = String::new();

        for line in toml_str.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            if line.starts_with('[') {
                section = line.trim_matches(|c| c == '[' || c == ']').to_string();
                continue;
            }
            if let Some((key, val)) = line.split_once('=') {
                let key = key.trim().trim_matches('"');
                let val = parse_toml_value(val);
                let val = val.as_str();

                match section.as_str() {
                    "drive" => {
                        if key == "product" {
                            name = val.to_string();
                        }
                        if key == "current_profile" {
                            // Only override the BD-ROM default on a successful
                            // parse; a malformed value previously silently set
                            // 0x0000 ("No current profile") instead.
                            match parse_u16_opt(val) {
                                Some(v) => current_profile = v,
                                None => eprintln!(
                                    "bdemu: invalid current_profile '{}', keeping default 0x{:04X}",
                                    val, current_profile
                                ),
                            }
                        }
                    }
                    "files" => {
                        if key == "inquiry" {
                            inquiry_file = val.to_string();
                        }
                        if key == "rpc_state" {
                            rpc_file = val.to_string();
                        }
                        if key == "mode_2a" {
                            mode_2a_file = val.to_string();
                        }
                    }
                    "features" => {
                        // A malformed key must NOT fall back to 0x0000 (the real
                        // Profile List feature code) or it would silently
                        // overwrite legitimate data. Skip with a warning instead.
                        match parse_u16_opt(key) {
                            Some(code) => feature_files.push((code, val.to_string())),
                            None => eprintln!("bdemu: invalid feature code '{}', skipping", key),
                        }
                    }
                    "read_buffer" => {
                        // A malformed key must NOT coerce to buffer id 0: that is
                        // legitimate, so a typo'd key would silently shadow a real
                        // buffer-0 entry. Warn and skip, mirroring [features] above.
                        match u8::from_str_radix(
                            key.trim_start_matches("0x").trim_start_matches("0X"),
                            16,
                        ) {
                            Ok(id) => rb_files.push((id, val.to_string())),
                            Err(_) => {
                                eprintln!("bdemu: invalid read_buffer key '{}', skipping", key)
                            }
                        }
                    }
                    "unlock" => {
                        // Unlock handled automatically by bdemu — no config needed
                    }
                    _ => {}
                }
            }
        }

        // Load binary files. Every blob filename below comes from the untrusted
        // drive.toml, so it's resolved through read_blob, which refuses any name
        // that would escape the profile directory — see read_blob for details.
        let inquiry = read_blob(dir, &inquiry_file);

        let mut features: Vec<(u16, Vec<u8>)> = Vec::new();
        for (code, file) in &feature_files {
            let data = read_blob(dir, file);
            if !data.is_empty() {
                features.push((*code, data));
            }
        }
        features.sort_by_key(|(c, _)| *c);

        let rpc_state = if !rpc_file.is_empty() {
            read_blob(dir, &rpc_file)
        } else {
            Vec::new()
        };

        // Load read_buffer responses from TOML [read_buffer] section
        let mut read_bufs = Vec::new();
        for (id, file) in &rb_files {
            let data = read_blob(dir, file);
            if !data.is_empty() {
                read_bufs.push((*id, data));
            }
        }
        // Also scan for rb_*.bin files not listed in TOML
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let fname = entry.file_name().to_string_lossy().to_string();
                if fname.starts_with("rb_") && fname.ends_with(".bin") {
                    let id_str = &fname[3..fname.len() - 4];
                    if let Ok(id) = u8::from_str_radix(id_str, 16)
                        && !read_bufs.iter().any(|(i, _)| *i == id)
                    {
                        let data = read_bin(&entry.path());
                        if !data.is_empty() {
                            read_bufs.push((id, data));
                        }
                    }
                }
            }
        }

        // Load disc if BDEMU_DISC is set. Apply the same path-traversal containment
        // control.rs enforces on the untrusted control-socket peer, so the two
        // disc-selection paths can't diverge (reject `..`, stay under discs/).
        let disc = std::env::var("BDEMU_DISC")
            .ok()
            .and_then(|disc_name| safe_disc_dir(&dir.join("discs"), &disc_name))
            .map(|disc_dir| load_disc(&disc_dir));

        Some(LoadedProfile {
            name,

            inquiry,
            current_profile,
            features,
            rpc_state,
            read_bufs,
            mode_2a: if !mode_2a_file.is_empty() {
                // Also a drive.toml-supplied filename: contain it like every
                // other profile blob. The `else` branch below is a fixed literal
                // basename the loader chooses, so it needs no containment.
                read_blob(dir, &mode_2a_file)
            } else {
                read_bin(&dir.join("mode_2a.bin"))
            },
            disc,
        })
    }

    fn load_json(path: &str) -> Option<Self> {
        // Backward compat: parse JSON profile
        let json = fs::read_to_string(path)
            .map_err(|e| eprintln!("bdemu: cannot read '{}': {}", path, e))
            .ok()?;

        #[derive(serde::Deserialize)]
        struct JsonProfile {
            drive: JsonDrive,
            inquiry: JsonRaw,
            get_config: JsonGetConfig,
            #[serde(default)]
            mode_sense: Option<JsonModeSense>,
            #[serde(default)]
            report_key: Option<JsonReportKey>,
            #[serde(default)]
            read_buffer: HashMap<String, JsonRaw>,
        }

        #[derive(serde::Deserialize)]
        struct JsonDrive {
            #[serde(default)]
            product: String,
        }

        #[derive(serde::Deserialize)]
        struct JsonRaw {
            raw: String,
            #[serde(flatten)]
            _extra: HashMap<String, serde_json::Value>,
        }

        #[derive(serde::Deserialize)]
        struct JsonGetConfig {
            #[serde(default)]
            current_profile: String,
            #[serde(default)]
            features: HashMap<String, JsonRaw>,
        }

        #[derive(serde::Deserialize)]
        struct JsonModeSense {
            page_2a: Option<JsonRaw>,
        }

        #[derive(serde::Deserialize)]
        struct JsonReportKey {
            rpc_state: Option<JsonRaw>,
        }

        let p: JsonProfile = serde_json::from_str(&json)
            .map_err(|e| eprintln!("bdemu: JSON error: {}", e))
            .ok()?;

        let mut features = Vec::new();
        for (code_str, feat) in &p.get_config.features {
            // A malformed key must NOT fall back to 0x0000 (the real MMC Profile
            // List feature code): a typo'd key would silently overwrite legitimate
            // Profile List data. Skip with a warning, mirroring the TOML loader.
            let code = match parse_u16_opt(code_str) {
                Some(c) => c,
                None => {
                    eprintln!("bdemu: invalid feature code '{}', skipping", code_str);
                    continue;
                }
            };
            let bytes = parse_hex(&feat.raw);
            if !bytes.is_empty() {
                features.push((code, bytes));
            }
        }
        features.sort_by_key(|(c, _)| *c);

        let mut read_bufs = Vec::new();
        for (id_str, data) in &p.read_buffer {
            // Same as the TOML path: a malformed key must not coerce to buffer
            // id 0 (a legitimate id) and shadow a real buffer-0 entry. Warn and
            // skip instead.
            let id = match u8::from_str_radix(
                id_str.trim_start_matches("0x").trim_start_matches("0X"),
                16,
            ) {
                Ok(id) => id,
                Err(_) => {
                    eprintln!("bdemu: invalid read_buffer key '{}', skipping", id_str);
                    continue;
                }
            };
            let bytes = parse_hex(&data.raw);
            if !bytes.is_empty() {
                read_bufs.push((id, bytes));
            }
        }

        // A malformed/empty current_profile must not silently become 0x0000 ("No
        // current profile"): warn and fall back to the 0x0043 BD-ROM default so
        // the emulated drive never reports no active profile to libfreemkv.
        let current_profile = match parse_u16_opt(&p.get_config.current_profile) {
            Some(v) => v,
            None => {
                eprintln!(
                    "bdemu: invalid current_profile '{}', keeping default 0x0043",
                    p.get_config.current_profile
                );
                0x0043
            }
        };

        Some(LoadedProfile {
            name: p.drive.product,

            inquiry: parse_hex(&p.inquiry.raw),
            current_profile,
            features,
            rpc_state: p
                .report_key
                .and_then(|rk| rk.rpc_state.map(|d| parse_hex(&d.raw)))
                .unwrap_or_default(),
            read_bufs,
            mode_2a: p
                .mode_sense
                .and_then(|ms| ms.page_2a.map(|d| parse_hex(&d.raw)))
                .unwrap_or_default(),
            disc: None,
        })
    }

    pub fn find_feature(&self, code: u16) -> Option<&[u8]> {
        self.features
            .iter()
            .find(|(c, _)| *c == code)
            .map(|(_, data)| data.as_slice())
    }

    pub fn find_read_buf(&self, buf_id: u8) -> Option<&[u8]> {
        self.read_bufs
            .iter()
            .find(|(id, _)| *id == buf_id)
            .map(|(_, data)| data.as_slice())
    }

    pub fn has_disc(&self) -> bool {
        self.disc.is_some()
    }
}

/// Resolve a disc name to a directory under `discs_base`, applying path-traversal
/// containment. Returns `Some(dir)` only when the name is a single plain path
/// component AND the canonicalized result stays under `discs_base` AND it is an
/// existing directory.
///
/// This is the single source of truth for that guard: both the `BDEMU_DISC` env
/// path (in `load_dir`) and the control-socket `load` command (`control.rs`'s
/// `cmd_load`) call it, so the two disc-selection paths cannot drift apart.
pub fn safe_disc_dir(discs_base: &Path, name: &str) -> Option<std::path::PathBuf> {
    // Reject anything that is not a single plain path component (separators / dot
    // components would let `join` escape discs/). Same predicate as `read_blob`.
    if !is_contained_blob_name(name) {
        return None;
    }

    let disc_dir = discs_base.join(name);

    // Belt-and-suspenders: canonicalize and assert the result stays under discs/;
    // a canonicalize failure is a reject, not a skip (defense-in-depth on top of
    // the lexical pre-filter). Return the CANONICAL path, closing a symlink-swap TOCTOU.
    match discs_base.canonicalize() {
        Ok(base) => match disc_dir.canonicalize() {
            Ok(resolved) if resolved.starts_with(&base) && resolved.is_dir() => Some(resolved),
            // disc_dir resolved but escaped the base, failed to resolve, or is not
            // a directory.
            _ => None,
        },
        // The discs base itself does not resolve (no discs dir yet): nothing to
        // contain against, fall back to the is_dir() existence check on the lexical
        // path, which will reject a non-existent target anyway.
        Err(_) => {
            if disc_dir.is_dir() {
                Some(disc_dir)
            } else {
                None
            }
        }
    }
}

/// Load a disc profile from a directory containing captured SCSI responses.
pub fn load_disc(dir: &Path) -> DiscProfile {
    let mut disc_structures = HashMap::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if fname.starts_with("ds_") && fname.ends_with(".bin") {
                let fmt_str = &fname[3..fname.len() - 4];
                if let Ok(fmt) = u8::from_str_radix(fmt_str, 16) {
                    let data = read_bin(&entry.path());
                    if !data.is_empty() {
                        disc_structures.insert(fmt, data);
                    }
                }
            }
        }
    }
    let (sectors, sector_map) = parse_sector_file(read_bin(&dir.join("sectors.bin")));
    DiscProfile {
        toc: read_bin(&dir.join("toc.bin")),
        capacity: read_bin(&dir.join("capacity.bin")),
        disc_info: read_bin(&dir.join("disc_info.bin")),
        disc_structures,
        sector_data: read_bin(&dir.join("sector_data.bin")),
        sectors,
        sector_map,
    }
}

// Cap a single profile blob read into memory: bdemu profiles are test
// fixtures, not full-disc images, so a sectors.bin near real-disc size is a
// mistake (or hostile fixture) that `fs::read` would otherwise OOM on.
const MAX_BIN_BYTES: u64 = 16 * 1024 * 1024 * 1024; // 16 GiB

// Read a profile blob, refusing files past MAX_BIN_BYTES. Ok(empty) means "genuinely absent";
// every other failure is Err so it gets logged.
fn read_bin_reported(path: &Path) -> Result<Vec<u8>, String> {
    match fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_BIN_BYTES => Err(format!(
            "refusing to load {} ({} bytes exceeds the {}-byte cap)",
            path.display(),
            meta.len(),
            MAX_BIN_BYTES
        )),
        // Stat failed: fall through to fs::read so the error the caller sees is
        // the one that actually matters (and so a file created between the two
        // calls is still read).
        _ => match fs::read(path) {
            Ok(data) => Ok(data),
            // The one benign failure: the blob is simply not part of this
            // profile. Silent, because profiles legitimately omit optional files
            // and logging every absence would drown the real diagnostics.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(format!("cannot read {}: {}", path.display(), e)),
        },
    }
}

// `read_bin_reported` with the failure logged and mapped to the empty-Vec
// "not present" convention every caller already understands, so a non-absence
// failure leaves a trace instead of looking like genuine disc content.
fn read_bin(path: &Path) -> Vec<u8> {
    match read_bin_reported(path) {
        Ok(data) => data,
        Err(msg) => {
            eprintln!("bdemu: {}", msg);
            Vec::new()
        }
    }
}

// True when `name` is a single plain path component safe to join onto the
// profile directory: non-empty, no separator/NUL, not a `.`/`..` traversal
// component. (A `.` inside a filename, e.g. `inquiry.bin`, is fine.)
fn is_contained_blob_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && name != "."
        && name != ".."
}

// Read a profile blob whose FILENAME came from the untrusted `drive.toml`, enforcing it names a
// plain file inside the profile directory, not a path escape.
fn read_blob(dir: &Path, name: &str) -> Vec<u8> {
    if is_contained_blob_name(name) {
        read_bin(&dir.join(name))
    } else {
        eprintln!(
            "bdemu: refusing profile blob path {:?}: it must be a plain filename \
             inside the profile directory, not a path that escapes it",
            name
        );
        Vec::new()
    }
}

fn parse_hex(hex: &str) -> Vec<u8> {
    let clean: String = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    (0..clean.len())
        .step_by(2)
        .filter_map(|i| {
            if i + 2 <= clean.len() {
                u8::from_str_radix(&clean[i..i + 2], 16).ok()
            } else {
                None
            }
        })
        .collect()
}

// Parse the right-hand side of a `key = value` TOML line: strips an optional
// surrounding pair of double quotes and a trailing `#` comment, but only a
// `#` OUTSIDE the quotes — so `product = "BDR-#1"` keeps its `#1`.
fn parse_toml_value(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix('"') {
        // Quoted: take everything up to the next double quote verbatim (a `#`
        // before the closing quote is part of the value, not a comment).
        match rest.find('"') {
            Some(end) => rest[..end].to_string(),
            // Unterminated quote: fall back to comment-stripping the remainder.
            None => rest.split('#').next().unwrap_or("").trim().to_string(),
        }
    } else {
        // Unquoted: a `#` begins a comment. Do NOT trim_matches('"') here — any
        // `"` present is part of the value itself (e.g. `ACME "Pro"`), and
        // stripping it would silently truncate trailing quote characters.
        trimmed.split('#').next().unwrap_or("").trim().to_string()
    }
}

/// Parse a u16 in hex (`0x`/`0X` prefix) or decimal, returning None on failure
/// so callers can preserve a meaningful default instead of silently using 0.
fn parse_u16_opt(s: &str) -> Option<u16> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
