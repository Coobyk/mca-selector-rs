fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=resources/windows/mca-selector.rc");
        println!("cargo:rerun-if-changed=resources/windows/mca-selector.ico");
        embed_resource::compile("resources/windows/mca-selector.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
    }
}
