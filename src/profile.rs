// bdemu — Blu-ray Drive Emulator
// MIT — freemkv project
//
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
        let _version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let num_ranges = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;

        // `num_ranges` is untrusted file data: a value like 0xFFFFFFFF would
        // make `with_capacity` request a multi-GB allocation that aborts the
        // process, and `12 + num_ranges*8` would overflow. The file cannot
        // describe more ranges than its own bytes allow (8 bytes each after
        // the 12-byte header), so clamp to that bound first.
        let max_ranges = data.len().saturating_sub(12) / 8;
        if num_ranges > max_ranges {
            return (data, Vec::new()); // corrupt/hostile, treat as flat
        }

        // header_size = 12 + num_ranges*8; bounded by max_ranges above so this
        // cannot overflow, but verify against the buffer anyway.
        let header_size = match num_ranges.checked_mul(8).and_then(|h| h.checked_add(12)) {
            Some(h) if h <= data.len() => h,
            _ => return (data, Vec::new()),
        };

        let mut map = Vec::with_capacity(num_ranges);
        let mut data_offset = header_size;
        for i in 0..num_ranges {
            let off = 12 + i * 8;
            let start_lba =
                u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
            let count =
                u32::from_le_bytes([data[off + 4], data[off + 5], data[off + 6], data[off + 7]]);

            // The declared sector bytes for this range must actually exist in
            // `data`; otherwise lookup_sector + the READ handler would slice
            // out of bounds and panic mid-session. Bail to the flat fallback
            // on overflow or truncation rather than building a poisoned map.
            let span = match (count as usize).checked_mul(SECTOR_SIZE) {
                Some(s) => s,
                None => return (data, Vec::new()),
            };
            let next_offset = match data_offset.checked_add(span) {
                Some(o) if o <= data.len() => o,
                _ => return (data, Vec::new()),
            };

            map.push((start_lba, count, data_offset));
            data_offset = next_offset;
        }

        // lookup_sector (scsi.rs) binary-searches this map and so REQUIRES it to
        // be sorted ascending by start_lba. The byte_offset stored per range is
        // computed above from file order, so sorting the (start_lba, count,
        // byte_offset) tuples by start_lba reorders the search keys without
        // disturbing where each range's bytes live. A capture tool that emits
        // ranges in non-ascending LBA order would otherwise make binary_search
        // return Err for sectors that ARE present, silently zero-filling them
        // (data corruption in the emulated disc). Matches the features-list sort
        // precedent in load_dir/load_json.
        map.sort_by_key(|&(start, _, _)| start);

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
                        // A malformed key must NOT fall back to 0x0000: that is
                        // the real MMC Profile List feature code, so a typo'd
                        // key coerced to 0 would silently overwrite legitimate
                        // Profile List data. Skip it with a warning instead,
                        // mirroring the current_profile handling above.
                        match parse_u16_opt(key) {
                            Some(code) => feature_files.push((code, val.to_string())),
                            None => eprintln!("bdemu: invalid feature code '{}', skipping", key),
                        }
                    }
                    "read_buffer" => {
                        // A malformed key must NOT coerce to buffer id 0: that is
                        // a legitimate buffer id, so a typo'd key would silently
                        // overwrite/shadow a real buffer-0 entry (which
                        // find_read_buf would then serve wrong). Warn and skip,
                        // mirroring the [features] and current_profile handling.
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

        // Load binary files
        let inquiry = read_bin(&dir.join(&inquiry_file));

        let mut features: Vec<(u16, Vec<u8>)> = Vec::new();
        for (code, file) in &feature_files {
            let data = read_bin(&dir.join(file));
            if !data.is_empty() {
                features.push((*code, data));
            }
        }
        features.sort_by_key(|(c, _)| *c);

        let rpc_state = if !rpc_file.is_empty() {
            read_bin(&dir.join(&rpc_file))
        } else {
            Vec::new()
        };

        // Load read_buffer responses from TOML [read_buffer] section
        let mut read_bufs = Vec::new();
        for (id, file) in &rb_files {
            let data = read_bin(&dir.join(file));
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

        // Load disc if BDEMU_DISC is set. BDEMU_DISC is operator-supplied, but
        // apply the same path-traversal containment that control.rs enforces on
        // the (untrusted) control-socket peer, so the two disc-selection paths
        // can't diverge: reject separators/dot components and assert the
        // canonicalized target stays under the discs/ base.
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
                read_bin(&dir.join(&mode_2a_file))
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
            // A malformed key must NOT fall back to 0x0000: that is the real MMC
            // Profile List feature code, so a typo'd JSON feature key would
            // silently coerce to 0 and inject/overwrite legitimate Profile List
            // data in the GET CONFIGURATION response. Skip it with a warning,
            // mirroring the TOML [features] loader and the read_buffer handling.
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

        // A malformed/empty current_profile must not silently become 0x0000
        // ("No current profile"): warn and fall back to the 0x0043 BD-ROM
        // default, matching the TOML loader so the emulated drive never reports
        // no active profile to libfreemkv's GET CONFIGURATION.
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
    // Reject anything that is not a single, plain path component: separators or
    // dot components would let `join` walk out of (or, for an absolute path,
    // replace) the discs directory.
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name == "."
        || name == ".."
        || name.contains('\0')
    {
        return None;
    }

    let disc_dir = discs_base.join(name);

    // Belt-and-suspenders: canonicalize and assert the result stays under the
    // discs directory. Canonicalization only succeeds for existing paths.
    //
    // Treat a canonicalize failure as a reject rather than silently skipping the
    // containment check: if the base canonicalizes but disc_dir does not (e.g. a
    // symlink whose target was transiently removed), we cannot prove containment,
    // so we refuse. The lexical pre-filter above already blocks component
    // escapes, so this is defense-in-depth — but it must never be silently
    // disabled.
    //
    // Return the CANONICAL resolved path (not the unresolved lexical join) so the
    // caller's subsequent load_disc reads target the same inode we just verified —
    // closing the TOCTOU window where a symlink swap between this check and the
    // read could redirect load_disc to a different file.
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

/// Hard ceiling on a single profile blob slurped into memory. sectors.bin is the
/// largest file (a whole disc image) and a real UHD BD-ROM tops out near 100 GB,
/// but bdemu profiles are test fixtures, not full-disc images — a profile that
/// large is a mistake (or a hostile/corrupt fixture), and `fs::read` would slurp
/// the whole thing into one Vec and OOM the process. Cap it.
const MAX_BIN_BYTES: u64 = 16 * 1024 * 1024 * 1024; // 16 GiB

/// Read a profile blob into memory, refusing files past `MAX_BIN_BYTES`.
///
/// Returns `Ok(empty)` for a blob that is genuinely ABSENT — callers treat an
/// empty Vec as "this feature is not present in the profile", and profiles
/// legitimately omit rpc_state.bin, mode_2a.bin, sector_data.bin and so on.
/// Every OTHER failure (EACCES, EIO, a directory where a file was expected, an
/// oversized blob) returns `Err` with a message the caller logs.
///
/// That split is the whole point. The previous `_ => fs::read(path)
/// .unwrap_or_default()` collapsed EVERY error into an empty Vec with no log, so
/// an unreadable `discs/<n>/sectors.bin` was indistinguishable from a profile
/// that simply has no sectors — and READ(10) then served 2048 zero bytes per
/// sector at GOOD status. A rip could complete "successfully" against a profile
/// the process could not actually read. Only the >16 GiB case was ever logged.
///
/// Stat first so an oversized (or hostile) file is rejected before the
/// allocation, rather than `fs::read` growing an unbounded Vec to OOM.
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

/// `read_bin_reported` with the failure logged and mapped to the empty-Vec
/// "not present" convention every caller already understands. The log is the
/// contract: a blob that failed to load for any reason other than absence must
/// leave a trace, so a zero-filled read later in the session can be traced back
/// to its cause instead of looking like genuine disc content.
fn read_bin(path: &Path) -> Vec<u8> {
    match read_bin_reported(path) {
        Ok(data) => data,
        Err(msg) => {
            eprintln!("bdemu: {}", msg);
            Vec::new()
        }
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

/// Parse the right-hand side of a `key = value` TOML line into the bare value.
///
/// Strips an optional surrounding pair of double quotes and a trailing `#`
/// comment — but only a `#` that appears OUTSIDE the quoted value. The previous
/// inline `split('#').next()` truncated unconditionally, so a legitimate value
/// like `product = "BDR-#1"` or `inquiry = "file#2.bin"` was silently cut to
/// `BDR-` / `file`. Here a quoted value keeps everything up to its closing quote
/// verbatim; an unquoted value is comment-stripped at the first `#`.
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
        // Unquoted: a `#` begins a comment. Do NOT trim_matches('"') here — this
        // branch only runs when the value does not start with a quote, so any
        // `"` present is part of the value (e.g. `ACME "Pro"`) and stripping it
        // would silently truncate trailing quote characters.
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
mod tests {
    use super::{
        LoadedProfile, parse_sector_file, parse_toml_value, parse_u16_opt, read_bin_reported,
        safe_disc_dir,
    };

    /// A per-test scratch directory under the crate's `target/` (never /tmp, per
    /// the project no-/tmp scratch rule). CARGO_MANIFEST_DIR is always set at
    /// compile time, so this resolves to `<crate>/target/test-scratch/<tag>`,
    /// which `cargo clean` reclaims. The directory is created (and any stale
    /// prior contents removed) before returning.
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
    fn huge_num_ranges_does_not_allocate_or_panic() {
        // 0xFFFFFFFF ranges declared but no body: must not try to allocate
        // billions of entries — falls back to flat (empty map).
        let data = bdsm_header(0xFFFF_FFFF);
        let (_, map) = parse_sector_file(data);
        assert!(map.is_empty(), "hostile num_ranges must fall back to flat");
    }

    #[test]
    fn truncated_range_body_falls_back_to_flat() {
        // One range claiming 1000 sectors (≈2 MB) but the file has no payload.
        // Building the map unchecked would later slice out of bounds; instead
        // we must drop to the flat fallback.
        let mut data = bdsm_header(1);
        data.extend_from_slice(&0u32.to_le_bytes()); // start_lba = 0
        data.extend_from_slice(&1000u32.to_le_bytes()); // count = 1000
        // No sector payload follows.
        let (_, map) = parse_sector_file(data);
        assert!(
            map.is_empty(),
            "truncated payload must fall back to flat, not build an OOB map"
        );
    }

    #[test]
    fn count_multiply_overflow_falls_back_to_flat() {
        // count near u32::MAX so count*2048 overflows usize math on 32-bit and
        // certainly exceeds data.len(): must not panic, must fall back.
        let mut data = bdsm_header(1);
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&u32::MAX.to_le_bytes());
        let (_, map) = parse_sector_file(data);
        assert!(map.is_empty());
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
        // A capture tool that emits ranges in non-ascending LBA order must still
        // resolve correctly: parse_sector_file sorts the map by start_lba so
        // lookup_sector's binary search finds every present sector. Without the
        // sort, binary_search would return Err for sectors that ARE captured and
        // READ would silently zero-fill them.
        //
        // Two ranges declared OUT of LBA order (range starting at LBA 1000 first,
        // then range starting at LBA 100). Payloads tagged with distinct bytes so
        // we can prove each lookup lands on the right range's data.
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
        // A JSON profile whose feature map has one valid key (0x010C) and one
        // garbage key ("oops"). The garbage key must be SKIPPED, not silently
        // coerced to 0x0000 (the real Profile List feature code), which would
        // inject a bogus Profile List descriptor into GET CONFIGURATION.
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
    /// A blob that is genuinely absent is the documented "feature not present"
    /// case and stays silent, but every OTHER read failure must be reported.
    ///
    /// Catches the mutation that restores `_ => fs::read(path).unwrap_or_default()`:
    /// with it, an unreadable `sectors.bin` (EACCES, EIO, or — as exercised here —
    /// a directory where a file was expected) returns an empty Vec with no log and
    /// is indistinguishable from a profile that simply has no sectors. That was
    /// the direct enabler of READ(10) serving zeros at GOOD status.
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

    /// A profile directory holding only `drive.toml` — every binary blob missing —
    /// must still load, with each absent blob presenting as empty rather than
    /// failing the whole profile or panicking on a missing file. This is the
    /// coverage gap for `LoadedProfile::load` on an incomplete profile (the shape
    /// `bdemu validate` exists to warn about).
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
}
