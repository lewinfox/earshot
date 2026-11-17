//! Finds the bundled controller files (`controllers/<maker>/<maker>.json` and
//! `controllers/<maker>/controllers/<model>.json`) and writes the list that
//! `core::controllers` compiles in, so a new file needs no code.

use std::fmt::Write;
use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("controllers");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut entries = vec![];
    let mut makers: Vec<_> = std::fs::read_dir(&root)
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    makers.sort();
    for dir in makers {
        println!("cargo:rerun-if-changed={}", dir.display());
        let maker = dir.file_name().unwrap().to_string_lossy().to_string();
        let own = dir.join(format!("{maker}.json"));
        if own.exists() {
            entries.push((maker.clone(), None, own));
        }
        let models_dir = dir.join("controllers");
        println!("cargo:rerun-if-changed={}", models_dir.display());
        let mut models: Vec<_> = std::fs::read_dir(&models_dir)
            .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default();
        models.sort();
        for path in models {
            if path.extension().is_some_and(|e| e == "json") {
                let model = path.file_stem().unwrap().to_string_lossy().to_string();
                entries.push((maker.clone(), Some(model), path));
            }
        }
    }
    let mut out = String::from("&[\n");
    for (maker, model, path) in entries {
        println!("cargo:rerun-if-changed={}", path.display());
        writeln!(
            out,
            "    ({maker:?}, {model:?}, include_str!({:?})),",
            path.display().to_string()
        )
        .unwrap();
    }
    out.push_str("]\n");
    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("bundled_controllers.rs");
    std::fs::write(dest, out).unwrap();
}
