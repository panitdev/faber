use std::io::Write;
use std::path::PathBuf;

const EXTENSION_CRATES: &[&str] = &[
    "deno_webidl",
    "deno_web",
    "deno_crypto",
    "deno_net",
    "deno_telemetry",
    "deno_fetch",
];

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let dest = out_dir.join("embedded_ext_sources.rs");

    let metadata_output = std::process::Command::new(std::env::var("CARGO").unwrap())
        .args(["metadata", "--format-version", "1"])
        .output()
        .expect("cargo metadata failed");

    let metadata: serde_json::Value =
        serde_json::from_slice(&metadata_output.stdout).expect("cargo metadata is not valid JSON");

    let packages = metadata["packages"].as_array().expect("no packages array");

    let mut f = std::fs::File::create(&dest).expect("cannot create embedded_ext_sources.rs");
    writeln!(f, "static EMBEDDED_EXT_SOURCES: &[(&str, &str)] = &[").unwrap();
    let mut count = 0usize;

    for pkg in packages {
        let name = pkg["name"].as_str().unwrap_or("");
        if !EXTENSION_CRATES.contains(&name) {
            continue;
        }
        let manifest_path = pkg["manifest_path"].as_str().unwrap();
        let crate_dir = PathBuf::from(manifest_path)
            .parent()
            .unwrap()
            .to_path_buf();

        for entry in std::fs::read_dir(&crate_dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                continue;
            };
            if ext != "js" && ext != "ts" {
                continue;
            }
            let file_name = path.file_name().unwrap().to_str().unwrap();
            if file_name.ends_with(".d.ts") {
                continue;
            }
            let abs = path.to_str().unwrap();
            writeln!(f, r#"  ({abs:?}, include_str!({abs:?})),"#).unwrap();
            count += 1;
            println!("cargo::rerun-if-changed={abs}");
        }
    }

    writeln!(f, "];").unwrap();
    assert!(
        count > 0,
        "build.rs found no extension JS/TS sources — cargo metadata may not list them"
    );
}
