// bdemu — Blu-ray Drive Emulator
// AGPL-3.0 — freemkv project
//
// Drive profile loader — directory-based with .bin files + TOML metadata

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Loaded profile with raw bytes ready to serve
pub struct LoadedProfile {
    pub name: String,
    pub base_dir: PathBuf,
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
    pub disc_structures: HashMap<u8, Vec<u8>>,  // format_code -> data
    pub sector_data: Vec<u8>,  // single sector pattern (repeated)
    pub sectors: Vec<u8>,      // full sector dump (LBA-addressable, 2048 per sector)
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
                let val = val.trim().trim_matches('"').split('#').next().unwrap().trim().trim_matches('"');

                match section.as_str() {
                    "drive" => {
                        if key == "product" { name = val.to_string(); }
                        if key == "current_profile" {
                            current_profile = parse_u16(val);
                        }
                    }
                    "files" => {
                        if key == "inquiry" { inquiry_file = val.to_string(); }
                        if key == "rpc_state" { rpc_file = val.to_string(); }
                        if key == "mode_2a" { mode_2a_file = val.to_string(); }
                    }
                    "features" => {
                        let code = parse_u16(key);
                        feature_files.push((code, val.to_string()));
                    }
                    "read_buffer" => {
                        let id = u8::from_str_radix(key.trim_start_matches("0x").trim_start_matches("0X"), 16).unwrap_or(0);
                        rb_files.push((id, val.to_string()));
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
                    if let Ok(id) = u8::from_str_radix(id_str, 16) {
                        if !read_bufs.iter().any(|(i, _)| *i == id) {
                            let data = read_bin(&entry.path());
                            if !data.is_empty() {
                                read_bufs.push((id, data));
                            }
                        }
                    }
                }
            }
        }

        // Load disc if BDEMU_DISC is set
        let disc = std::env::var("BDEMU_DISC").ok().and_then(|disc_name| {
            let disc_dir = dir.join("discs").join(&disc_name);
            if disc_dir.is_dir() {
                // Load disc structure files: ds_00.bin, ds_01.bin, etc.
                let mut disc_structures = HashMap::new();
                if let Ok(entries) = fs::read_dir(&disc_dir) {
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
                Some(DiscProfile {
                    toc: read_bin(&disc_dir.join("toc.bin")),
                    capacity: read_bin(&disc_dir.join("capacity.bin")),
                    disc_info: read_bin(&disc_dir.join("disc_info.bin")),
                    disc_structures,
                    sector_data: read_bin(&disc_dir.join("sector_data.bin")),
                    sectors: read_bin(&disc_dir.join("sectors.bin")),
                })
            } else {
                None
            }
        });

        Some(LoadedProfile {
            name,
            base_dir: dir.to_path_buf(),
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
            let code = parse_u16(code_str);
            let bytes = parse_hex(&feat.raw);
            if !bytes.is_empty() {
                features.push((code, bytes));
            }
        }
        features.sort_by_key(|(c, _)| *c);

        let mut read_bufs = Vec::new();
        for (id_str, data) in &p.read_buffer {
            let id = u8::from_str_radix(id_str.trim_start_matches("0x"), 16).unwrap_or(0);
            let bytes = parse_hex(&data.raw);
            if !bytes.is_empty() {
                read_bufs.push((id, bytes));
            }
        }

        Some(LoadedProfile {
            name: p.drive.product,
            base_dir: PathBuf::from("."),
            inquiry: parse_hex(&p.inquiry.raw),
            current_profile: parse_u16(&p.get_config.current_profile),
            features,
            rpc_state: p.report_key
                .and_then(|rk| rk.rpc_state.map(|d| parse_hex(&d.raw)))
                .unwrap_or_default(),
            read_bufs,
            mode_2a: p.mode_sense
                .and_then(|ms| ms.page_2a.map(|d| parse_hex(&d.raw)))
                .unwrap_or_default(),
            disc: None,
        })
    }

    pub fn find_feature(&self, code: u16) -> Option<&[u8]> {
        self.features.iter()
            .find(|(c, _)| *c == code)
            .map(|(_, data)| data.as_slice())
    }

    pub fn find_read_buf(&self, buf_id: u8) -> Option<&[u8]> {
        self.read_bufs.iter()
            .find(|(id, _)| *id == buf_id)
            .map(|(_, data)| data.as_slice())
    }

    pub fn has_disc(&self) -> bool {
        self.disc.is_some()
    }
}

fn read_bin(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_default()
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

fn parse_u16(s: &str) -> u16 {
    let s = s.trim();
    if s.starts_with("0x") || s.starts_with("0X") {
        u16::from_str_radix(&s[2..], 16).unwrap_or(0)
    } else {
        s.parse().unwrap_or(0)
    }
}
