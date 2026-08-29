fn main() {
    println!("cargo:rerun-if-changed=assets/alula.rc");
    println!("cargo:rerun-if-changed=assets/alula.exe.manifest");
    println!("cargo:rerun-if-changed=assets/app-icon.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile_for("assets/alula.rc", &["alula"], embed_resource::NONE)
            .manifest_required()
            .expect("failed to embed the Windows application icon and manifest");
    }
}
