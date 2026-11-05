//! Evidence images (interface screenshots) and attachments.
//!
//! They are stored in the kontext-home, addressed by content:
//! `media/<sha256>.<ext>`. That way they travel with `kontext sync` like the
//! ledger (two machines that upload the same image write the same file, and git
//! does not conflict), and the ledger entry only stores the path, the hash, the
//! original name and the screen (for the report's "before and after").
//!
//! An image over the cap (`[media] max_size`, 2 MB) is refused (exit 2) with a
//! message that says how big it is and how to shrink it. `kontext verify` checks
//! that each image cited exists and that its content matches its hash.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::model::sha256_hex;
use crate::store::Refused;

/// Default cap for each image: 2 MB.
pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// The image extensions accepted as evidence.
pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp"];

/// `"2MB"`, `"500 KB"`, `"1.5MB"` or bytes (`"2097152"`).
pub fn parse_size(s: &str) -> Result<u64> {
    let t = s.trim().to_ascii_lowercase().replace(' ', "");
    let (num, mult) = if let Some(n) = t.strip_suffix("mb") {
        (n, 1024.0 * 1024.0)
    } else if let Some(n) = t.strip_suffix("kb") {
        (n, 1024.0)
    } else if let Some(n) = t.strip_suffix('b') {
        (n, 1.0)
    } else {
        (t.as_str(), 1.0)
    };
    let n: f64 = num
        .parse()
        .map_err(|_| anyhow::anyhow!("{s:?} is not a size (use \"2MB\", \"500KB\" or bytes)"))?;
    if n.is_nan() || n <= 0.0 {
        bail!("{s:?} must be greater than 0");
    }
    Ok((n * mult) as u64)
}

/// `2 MB`, `340 KB`, `812 B`.
pub fn size_text(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        let mb = bytes as f64 / (1024.0 * 1024.0);
        if mb.fract() < 0.05 {
            format!("{} MB", mb.round() as u64)
        } else {
            format!("{mb:.1} MB")
        }
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}

/// Does the path look like an image (by its extension)?
pub fn is_image_path(p: &str) -> bool {
    ext_of(p).is_some_and(|e| IMAGE_EXTS.contains(&e.as_str()))
}

fn ext_of(p: &str) -> Option<String> {
    Path::new(p)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// The image type from its first bytes, or `None` if it is not one that is
/// accepted.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// The `Content-Type` of a stored extension.
pub fn content_type(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "html" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// What was stored: the path relative to the kontext-home and its data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stored {
    /// `media/<sha256>.<ext>`.
    pub rel: String,
    pub sha256: String,
    pub ext: String,
    /// The name of the original file (`pago.png`).
    pub name: String,
    pub bytes: u64,
}

impl Stored {
    /// The evidence or the attachment that goes into the ledger.
    pub fn to_json(&self, kind: &str, screen: Option<&str>) -> Value {
        let mut v = json!({
            "type": kind,
            "value": self.rel,
            "sha256": self.sha256,
            "name": self.name,
            "bytes": self.bytes,
        });
        if let Some(s) = screen.map(str::trim).filter(|s| !s.is_empty()) {
            v["screen"] = json!(s);
        }
        v
    }
}

/// The default screen: the file name without its extension
/// (`screenshots/pago.png` → `pago`). That way two screenshots with the same
/// name are the before and after of the same screen.
pub fn default_screen(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Copies `src` to `media/<sha256>.<ext>` in the kontext-home (if it was not
/// there already). Images: `png`, `jpg`, `webp`, checked by their content; with
/// `html`, also a page (the phase 0 mockup). Refuses what goes over the cap.
pub fn store(home: &Path, src: &Path, max_bytes: u64, allow_html: bool) -> Result<Stored> {
    let shown = src.display().to_string();
    let meta = fs::metadata(src)
        .map_err(|_| Refused(format!("{shown}: does not exist or cannot be read")))?;
    if !meta.is_file() {
        return Err(Refused(format!("{shown}: is not a file")).into());
    }
    if meta.len() > max_bytes {
        return Err(Refused(format!(
            "{shown} is {} and the cap is {} ([media] max_size in project.toml). \
             Crop it to the part that matters, or save it as JPG or WebP at a lower quality",
            size_text(meta.len()),
            size_text(max_bytes)
        ))
        .into());
    }
    let bytes = fs::read(src).with_context(|| format!("could not read {shown}"))?;
    let ext = match sniff(&bytes) {
        Some(e) => e.to_string(),
        None if allow_html && ext_of(&shown).is_some_and(|e| e == "html" || e == "htm") => {
            "html".to_string()
        }
        None => {
            return Err(Refused(format!(
                "{shown}: is not a PNG, JPG or WebP image{}",
                if allow_html {
                    " (or an .html page)"
                } else {
                    ""
                }
            ))
            .into());
        }
    };
    let sha = sha256_hex(&bytes);
    let rel = format!("media/{sha}.{ext}");
    let dest = home.join(&rel);
    if !dest.exists() {
        fs::create_dir_all(home.join("media"))?;
        crate::config::write_atomic(&dest, &bytes)?;
    }
    Ok(Stored {
        rel,
        sha256: sha,
        ext,
        name: src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        bytes: bytes.len() as u64,
    })
}

/// Is it a file name in `media/`? (`<64 hex>.<ext>`)
pub fn valid_media_name(name: &str) -> Option<(&str, &str)> {
    let (sha, ext) = name.split_once('.')?;
    let ok = sha.len() == 64
        && sha.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
        && matches!(ext, "png" | "jpg" | "jpeg" | "webp" | "html");
    ok.then_some((sha, ext))
}

/// The images and attachments an entry cites: its `fields.evidence` and its
/// `fields.attachments` with `type` `image` or `page`.
pub fn refs_of(fields: &serde_json::Map<String, Value>) -> Vec<&Value> {
    ["evidence", "attachments"]
        .iter()
        .filter_map(|k| fields.get(*k).and_then(Value::as_array))
        .flatten()
        .filter(|it| {
            matches!(
                it.get("type").and_then(Value::as_str),
                Some("image" | "page")
            )
        })
        .collect()
}

/// Checks a cited image: that it is in the kontext-home and that its content
/// matches its hash. `Err` with the problem.
pub fn check(home: &Path, item: &Value) -> std::result::Result<(), String> {
    let rel = item.get("value").and_then(Value::as_str).unwrap_or("");
    let name = rel.strip_prefix("media/").unwrap_or("");
    let Some((sha_in_name, _)) = valid_media_name(name) else {
        return Err(format!("{rel:?} is not a media/<sha256>.<ext> path"));
    };
    let sha = item
        .get("sha256")
        .and_then(Value::as_str)
        .unwrap_or(sha_in_name);
    if sha != sha_in_name {
        return Err(format!(
            "{rel}: the stored hash ({}) is not the one in its name",
            &sha[..sha.len().min(12)]
        ));
    }
    let path: PathBuf = home.join(rel);
    let bytes = fs::read(&path).map_err(|_| {
        format!(
            "{rel} is not in the kontext-home (is `kontext sync` missing on the machine that uploaded it?)"
        )
    })?;
    if sha256_hex(&bytes) != sha {
        return Err(format!(
            "{rel}: its content no longer matches its hash (it was changed by hand)"
        ));
    }
    Ok(())
}

/// Standard base64 (with `=`), for the images embedded in the report.
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("2MB").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_size("500 kb").unwrap(), 500 * 1024);
        assert_eq!(parse_size("1234").unwrap(), 1234);
        assert!(parse_size("dos").is_err());
        assert!(parse_size("0").is_err());
        assert_eq!(size_text(2 * 1024 * 1024), "2 MB");
        assert_eq!(size_text(3_565_158), "3.4 MB");
        assert_eq!(size_text(2048), "2 KB");
    }

    #[test]
    fn base64_matches_rfc4648() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn media_names() {
        let sha = "a".repeat(64);
        assert!(valid_media_name(&format!("{sha}.png")).is_some());
        assert!(valid_media_name("../x.png").is_none());
        assert!(valid_media_name(&format!("{sha}.exe")).is_none());
        assert_eq!(default_screen("screenshots/pago.png"), "pago");
    }
}
