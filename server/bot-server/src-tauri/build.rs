use std::{path::PathBuf, process::Command};

fn main() {
    let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"))
        .join("../../..");
    let release = std::fs::read_to_string(root.join("VERSION")).expect("release VERSION");
    assert_eq!(release.trim(), std::env::var("CARGO_PKG_VERSION").expect("package version"),
        "VERSION and the Mac package version must match");
    println!("cargo:rerun-if-changed={}", root.join("VERSION").display());
    for directory in ["server/bot-server/src-tauri/src", "server/bot-server/src",
        "server/bot-server/local-models", "server/xiaodai-core/src", "firmware/components", "firmware/main", "shared"] {
        println!("cargo:rerun-if-changed={}", root.join(directory).display());
    }
    for path in [".git/HEAD", ".git/index", ".git/packed-refs", "server/bot-server/src-tauri/Cargo.toml",
        "server/bot-server/src-tauri/tauri.conf.json", "server/bot-server/package.json", "firmware/CMakeLists.txt"] {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    let revision = Command::new("git").current_dir(&root)
        .args(["rev-parse", "--short=12", "HEAD"]).output().ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|output| output.trim().to_owned())
        .filter(|output| !output.is_empty());
    let revision = revision.map(|revision| {
        let dirty = Command::new("git").current_dir(&root)
            .args(["status", "--porcelain", "--untracked-files=normal"]).output().ok()
            .filter(|output| output.status.success())
            .map(|output| !output.stdout.is_empty());
        match dirty { Some(true) => format!("{revision}-dirty"), Some(false) => revision, None => format!("{revision}-unknown") }
    }).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=ESPBOT_SOURCE_REVISION={revision}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rerun-if-changed=src/capture_permission.m");
        cc::Build::new().file("src/capture_permission.m").flag("-fobjc-arc")
            .compile("espbot_capture_permission");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rerun-if-changed=src/harness/physical_network.m");
        cc::Build::new().file("src/harness/physical_network.m").flag("-fobjc-arc")
            .compile("espbot_physical_network");
        println!("cargo:rustc-link-lib=framework=SystemConfiguration");
    }
    tauri_build::build();
}
