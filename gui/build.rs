//! Finds the bundled controller files (`controllers/<maker>/<maker>.json` and
//! `controllers/<maker>/controllers/<model>.json`) and writes the list that
//! `core::controllers` compiles in, so a new file needs no code. For Windows, also
//! puts the icon and version details in earshot.exe.

use std::fmt::Write;
use std::path::Path;

fn main() {
    windows_resources();
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

/// The icon Explorer, the taskbar and the Start menu show for earshot.exe, and the
/// details in its Properties. (The build machine's resource compiler does it: the
/// Windows SDK's on Windows.)
fn windows_resources() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon = "assets/icons/earshot.ico";
    println!("cargo:rerun-if-changed={icon}");
    winresource::WindowsResource::new()
        .set_icon(icon)
        .set("ProductName", "earshot")
        .set("FileDescription", "earshot")
        .set("OriginalFilename", "earshot.exe")
        .set("LegalCopyright", "Copyright (c) 2026 Lewin Appleton-Fox (MIT licence)")
        .compile()
        .expect("couldn't add the icon and version details to earshot.exe");
}
