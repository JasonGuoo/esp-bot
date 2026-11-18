fn main() {
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
