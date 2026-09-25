use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../packaging/mantis-cad.ico");

    // Build scripts run on the host, which may differ from the app target.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let crate_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace = crate_dir
        .parent()
        .and_then(|crates| crates.parent())
        .expect("mantis-app must be inside the workspace crates directory");
    let icon = workspace.join("packaging/mantis-cad.ico");
    assert!(icon.is_file(), "missing Windows icon: {}", icon.display());

    // An absolute filename avoids differing RC.EXE/windres search paths.
    // Forward slashes also keep Windows backslashes out of RC string escapes.
    let icon_filename = icon
        .to_str()
        .expect("Windows icon path must be UTF-8")
        .replace('\\', "/")
        .replace('"', "\\\"");
    let resource = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("mantis-cad.rc");
    fs::write(&resource, format!("1 ICON \"{icon_filename}\"\n"))
        .expect("failed to write Windows icon resource");

    embed_resource::compile_for(&resource, ["mantis-app"], embed_resource::NONE)
        .manifest_required()
        .expect("failed to embed the MantisCAD Windows icon");
}
