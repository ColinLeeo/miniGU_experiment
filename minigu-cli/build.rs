fn main() {
    // Embed build timestamp so we can verify remote binary version.
    let output = std::process::Command::new("date")
        .arg("+%Y-%m-%d %H:%M:%S")
        .output()
        .expect("failed to run date");
    let timestamp = String::from_utf8_lossy(&output.stdout).trim().to_string();
    println!("cargo:rustc-env=MINIGU_BUILD_TIME={}", timestamp);
    println!("cargo:rerun-if-changed=src/");
}
