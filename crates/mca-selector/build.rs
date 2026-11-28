fn main() {
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=MCA_GIT_SHA={sha}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=resources/windows/mca-selector.rc");
        println!("cargo:rerun-if-changed=resources/windows/mca-selector.ico");
        embed_resource::compile("resources/windows/mca-selector.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
