fn main() {
    println!("cargo:rerun-if-changed=res/FeatherSend.rc");
    println!("cargo:rerun-if-changed=res/FeatherSend.ico");
    println!("cargo:rerun-if-changed=res/FeatherSend.manifest");
    embed_resource::compile("res/FeatherSend.rc", embed_resource::NONE)
        .manifest_required()
        .expect("compiling the Windows resources (icon, manifest, version)");
}
