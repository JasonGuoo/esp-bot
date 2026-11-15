fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rerun-if-changed=src/capture_permission.m");
        cc::Build::new().file("src/capture_permission.m").flag("-fobjc-arc")
            .compile("espbot_capture_permission");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=Foundation");
    }
    tauri_build::build();
}
