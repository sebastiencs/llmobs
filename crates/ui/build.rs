//! Embeds `web/dist` (D-G-35). A build script reports to Cargo on stdout and fails the build
//! with a panic, so the workspace's print and unwrap lints do not apply here.
#![allow(clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={}", dist.display());

    if !dist.join("index.html").exists() {
        fs::write(
            out.join("assets.rs"),
            "const BUILT: bool = false;\nstatic ASSETS: &[Asset] = &[];\n",
        )
        .expect("write assets.rs");
        return;
    }

    fs::create_dir_all(out.join("gz")).expect("create the gzip directory");
    let mut files = Vec::new();
    collect(&dist, &mut files);
    files.sort();

    let mut rows = String::new();
    for (n, file) in files.iter().enumerate() {
        let rel = file.strip_prefix(&dist).expect("file is under web/dist");
        let path = format!(
            "/{}",
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        );
        let ext = file
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        let content_type = content_type(&ext);
        let bytes = fs::read(file).expect("read a web/dist file");
        let etag = format!("\"{}\"", hex::encode(&Sha256::digest(&bytes)[..8]));

        let gzip = if compressible(content_type) {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::new(6));
            encoder.write_all(&bytes).expect("gzip a web/dist file");
            let packed = encoder.finish().expect("finish a gzip stream");
            if packed.len() < bytes.len() {
                let gz_path = out.join("gz").join(format!("{n}.gz"));
                fs::write(&gz_path, &packed).expect("write a gzip copy");
                format!("Some(include_bytes!({:?}))", gz_path)
            } else {
                "None".to_owned()
            }
        } else {
            "None".to_owned()
        };

        writeln!(
            rows,
            "    Asset {{ path: {path:?}, content_type: {content_type:?}, etag: {etag:?}, body: include_bytes!({file:?}), gzip: {gzip} }},"
        )
        .expect("write a row");
    }
    fs::write(
        out.join("assets.rs"),
        format!("const BUILT: bool = true;\nstatic ASSETS: &[Asset] = &[\n{rows}];\n"),
    )
    .expect("write assets.rs");
}

/// Every file under `dir`, recursively.
fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read web/dist") {
        let path = entry.expect("read a web/dist entry").path();
        if path.is_dir() {
            collect(&path, files);
        } else {
            files.push(path);
        }
    }
}

fn content_type(ext: &str) -> &'static str {
    match ext {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" => "text/plain; charset=utf-8",
        "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

fn compressible(content_type: &str) -> bool {
    content_type.starts_with("text/")
        || matches!(
            content_type,
            "application/json" | "image/svg+xml" | "application/manifest+json"
        )
}
