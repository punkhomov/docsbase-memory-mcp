// Debug formatting deliberately emits quoted, escaped Rust path literals.
#![allow(clippy::unnecessary_debug_formatting)]
use std::fmt::Write as _;
use std::{env, fs, path::Path};
fn main() {
    println!("cargo:rerun-if-changed=web/dist");
    if env::var_os("CARGO_FEATURE_WEB").is_none() {
        return;
    }
    let root = Path::new("web/dist");
    assert!(
        root.join("index.html").is_file(),
        "Web assets missing. Run: cd web && npm ci && npm run build (or build without --features web/cli-full)"
    );
    let mut files = Vec::new();
    collect(root, &mut files);
    files.sort();
    let mut output = String::from(
        "pub fn get(path: &str) -> Option<(&'static str, &'static [u8])> { match path {\n",
    );
    let mut bytes = 0;
    for file in files {
        let relative = file
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let mime = match file.extension().and_then(|x| x.to_str()) {
            Some("html") => "text/html; charset=utf-8",
            Some("js") => "text/javascript; charset=utf-8",
            Some("css") => "text/css; charset=utf-8",
            Some("svg") => "image/svg+xml",
            _ => "application/octet-stream",
        };
        let route = if relative == "index.html" {
            "/".to_owned()
        } else {
            format!("/{relative}")
        };
        writeln!(
            &mut output,
            "{route:?} => Some(({mime:?}, include_bytes!({:?}))),\n",
            file.canonicalize().unwrap()
        )
        .unwrap();
        bytes += fs::metadata(file).unwrap().len();
    }
    output.push_str("_ => None } }\n");
    println!("cargo:warning=Embedded frontend: {bytes} bytes");
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("web_assets.rs"),
        output,
    )
    .unwrap();
}
fn collect(root: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(&path, files);
        } else {
            files.push(path);
        }
    }
}
