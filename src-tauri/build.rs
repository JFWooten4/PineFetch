fn main() {
    println!("cargo:rerun-if-changed=../native/vision-ocr.swift");
    println!("cargo:rerun-if-changed=../scripts/prepare-ocr-runtime.sh");
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    println!("cargo:rerun-if-env-changed=DEVELOPER_DIR");
    println!("cargo:rerun-if-env-changed=SDKROOT");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let manifest = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let status = std::process::Command::new("bash")
            .arg(manifest.join("../scripts/prepare-ocr-runtime.sh"))
            .env(
                "PINEFETCH_OCR_TARGET_ARCH",
                std::env::var("CARGO_CFG_TARGET_ARCH").unwrap(),
            )
            .status()
            .expect("Could not run the build-time Apple Vision helper compiler");
        assert!(status.success(), "Apple Vision helper compilation failed");
    }
    tauri_build::build()
}
