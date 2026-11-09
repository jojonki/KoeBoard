//! Model catalog and verified download.
//!
//! To add a model or language, add one `ModelSpec` entry to `CATALOG`.
//! Only list models whose weights have a permissive license; see THIRD_PARTY.md.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub struct FileSpec {
    pub name: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

pub struct ModelSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub language: &'static str,
    pub license: &'static str,
    /// The license text, shown before anything is downloaded.
    pub license_url: &'static str,
    pub model_url: &'static str,
    /// Anything the user should know before accepting.
    pub license_note: &'static str,
    pub repo: &'static str,
    pub revision: &'static str,
    pub encoder: &'static str,
    pub decoder: &'static str,
    pub joiner: &'static str,
    pub tokens: &'static str,
    /// sherpa-onnx `model_type`; empty lets sherpa-onnx detect it.
    pub model_type: &'static str,
    pub feature_dim: i32,
    /// Inserted between split segments of one long recording.
    pub segment_joiner: &'static str,
    pub files: &'static [FileSpec],
}

pub const DEFAULT_MODEL: &str = "haas-ja-v1.0-q";

pub const CATALOG: &[ModelSpec] = &[
    ModelSpec {
        id: "haas-ja-v1.0-q",
        label: "Haas-ja v1.0 · Most accurate, with punctuation",
        language: "ja",
        license: "Apache-2.0",
        license_url: "https://huggingface.co/oboroge0/Haas-ja-v1.0-onnx-quantized/blob/7f5ab0a4b1bf39db78afd6691eeecbc4f9d42e39/LICENSE",
        model_url: "https://huggingface.co/third-intelligence/Haas-ja-v1.0",
        license_note: "Unofficial ONNX conversion of Third Intelligence's Haas-ja-v1.0. Training data has its own terms; check them for commercial use.",
        repo: "oboroge0/Haas-ja-v1.0-onnx-quantized",
        revision: "7f5ab0a4b1bf39db78afd6691eeecbc4f9d42e39",
        encoder: "encoder.quantized.onnx",
        decoder: "decoder.onnx",
        joiner: "joiner.onnx",
        tokens: "tokens.txt",
        model_type: "nemo_transducer",
        feature_dim: 128,
        segment_joiner: "",
        files: &[
            FileSpec { name: "encoder.quantized.onnx", size: 42261252, sha256: "aba291d61d3d594bc6fb8c7f90acbd980e7bc7bf6b7caaf3965ddc25e6f42fac" },
            FileSpec { name: "encoder.data", size: 410744064, sha256: "30befbf6aeb75383e6551f5e58de53efdb76972ddc7d0f76070c517899873586" },
            FileSpec { name: "decoder.onnx", size: 15661602, sha256: "2947cb5512699cbba753fe45fe221428aadaa9856597debbb7aabe19cc253622" },
            FileSpec { name: "joiner.onnx", size: 12158641, sha256: "8a68320e750890ff9e944a794b41ae1c42c6f439ac4c273503458a6b42adc12d" },
            FileSpec { name: "tokens.txt", size: 28704, sha256: "ec78b13c5d9f156cf0d5fdbd6b5aec5785680f319c92630c11d5d11b14b83a84" },
            FileSpec { name: "LICENSE", size: 11358, sha256: "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30" },
            FileSpec { name: "NOTICE", size: 783, sha256: "279784164ddd8a16f0b42aff954d34bc10485c0064a96c7a852419d95eca5cad" },
        ],
    },
    ModelSpec {
        id: "reazonspeech-k2-v2-int8",
        label: "ReazonSpeech k2 v2 · Lightest and fastest, no punctuation",
        language: "ja",
        license: "Apache-2.0",
        license_url: "https://www.apache.org/licenses/LICENSE-2.0",
        model_url: "https://huggingface.co/reazon-research/reazonspeech-k2-v2",
        license_note: "License stated on the model card. Training data has its own terms; check them for commercial use.",
        repo: "reazon-research/reazonspeech-k2-v2",
        revision: "291488c8151be24d7da4bf7af26e533fad96e407",
        encoder: "encoder-epoch-99-avg-1.int8.onnx",
        decoder: "decoder-epoch-99-avg-1.int8.onnx",
        joiner: "joiner-epoch-99-avg-1.int8.onnx",
        tokens: "tokens.txt",
        model_type: "",
        feature_dim: 80,
        segment_joiner: "",
        files: &[
            FileSpec { name: "encoder-epoch-99-avg-1.int8.onnx", size: 154670139, sha256: "2c7bd08a8a99f9ddd0d9e458456577b1f6279214e51426f114f9eced44c54e1d" },
            FileSpec { name: "decoder-epoch-99-avg-1.int8.onnx", size: 2959337, sha256: "8f0bff94d38797b03b762634ed03211a8e303d06cc4603cdd0cf4199d6eb1485" },
            FileSpec { name: "joiner-epoch-99-avg-1.int8.onnx", size: 2696970, sha256: "49cc7ea1d3d35a40a27442db5e89996da64bf0e683a903dce76e99e57a12e4de" },
            FileSpec { name: "tokens.txt", size: 45754, sha256: "2c3ac659818a48a0c04010e0593bbc4d7c8a24a054340b01131499c05fd52def" },
        ],
    },
];

pub fn find(id: &str) -> Option<&'static ModelSpec> {
    CATALOG.iter().find(|m| m.id == id)
}

pub fn model_dir(root: &Path, spec: &ModelSpec) -> PathBuf {
    root.join("models").join(spec.id)
}

pub fn allows_url(url: &str) -> bool {
    CATALOG.iter().any(|m| m.license_url == url || m.model_url == url)
}

pub fn download_size(spec: &ModelSpec) -> u64 {
    spec.files.iter().map(|f| f.size).sum()
}

/// Deletes a downloaded model. Only catalog ids reach this path.
pub fn remove(root: &Path, spec: &ModelSpec) -> Result<(), String> {
    let dir = model_dir(root, spec);
    match fs::remove_dir_all(&dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(()),
    }
}

pub fn is_installed(dir: &Path, spec: &ModelSpec) -> bool {
    spec.files.iter().all(|f| {
        fs::metadata(dir.join(f.name)).map(|m| m.len() == f.size).unwrap_or(false)
    })
}

pub fn id_is_installed(root: &Path, id: &str) -> bool {
    find(id).is_some_and(|spec| is_installed(&model_dir(root, spec), spec))
}

/// Downloads any missing files, verifying size and SHA-256.
/// `progress` receives overall completion in percent.
pub fn ensure_installed(
    root: &Path,
    spec: &ModelSpec,
    mut progress: impl FnMut(u8),
) -> Result<PathBuf, String> {
    let dir = model_dir(root, spec);
    if is_installed(&dir, spec) {
        return Ok(dir);
    }
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let total: u64 = spec.files.iter().map(|f| f.size).sum();
    let mut done = 0u64;
    for f in spec.files {
        let dest = dir.join(f.name);
        let complete = fs::metadata(&dest).map(|m| m.len() == f.size).unwrap_or(false);
        if !complete {
            let url = format!(
                "https://huggingface.co/{}/resolve/{}/{}",
                spec.repo, spec.revision, f.name
            );
            download(&url, &dest, f, |n| {
                progress(((done + n) * 100 / total.max(1)) as u8)
            })?;
        }
        done += f.size;
        progress((done * 100 / total.max(1)) as u8);
    }
    Ok(dir)
}

fn download(url: &str, dest: &Path, spec: &FileSpec, mut on_bytes: impl FnMut(u64)) -> Result<(), String> {
    let part = dest.with_extension("part");
    let result = (|| -> Result<(), String> {
        let resp = ureq::get(url).call().map_err(|e| format!("{}: {e}", spec.name))?;
        let mut reader = resp.into_body().into_reader();
        let mut file = fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut written = 0u64;
        loop {
            let n = reader.read(&mut buf).map_err(|e| format!("{}: {e}", spec.name))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            written += n as u64;
            on_bytes(written);
        }
        file.flush().map_err(|e| e.to_string())?;
        let digest = to_hex(&hasher.finalize());
        if written != spec.size || digest != spec.sha256 {
            return Err(format!("{}: verification failed (size={written}, sha256={digest})", spec.name));
        }
        drop(file);
        fs::rename(&part, dest).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_catalog_urls_may_be_opened() {
        assert!(!allows_url("https://example.com/"));
        assert!(!allows_url("file:///etc/passwd"));
    }

    #[test]
    fn catalog_is_consistent() {
        assert!(find(DEFAULT_MODEL).is_some());
        for m in CATALOG {
            assert_eq!(m.license, "Apache-2.0", "{} needs a license review", m.id);
            assert_eq!(m.revision.len(), 40, "{} must pin a commit", m.id);
            assert!(m.license_url.starts_with("https://"), "{} needs a license URL", m.id);
            assert!(m.model_url.starts_with("https://"), "{} needs a model URL", m.id);
            assert!(allows_url(m.license_url));
            for required in [m.encoder, m.decoder, m.joiner, m.tokens] {
                assert!(m.files.iter().any(|f| f.name == required), "{required} missing");
            }
            for f in m.files {
                assert_eq!(f.sha256.len(), 64, "{}", f.name);
            }
        }
    }
}
