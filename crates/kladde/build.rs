//! Embeds the application icon into the Windows executable (Explorer, taskbar, Alt+Tab).

fn main() {
    println!("cargo:rerun-if-changed=assets/kladde.rc");
    println!("cargo:rerun-if-changed=assets/kladde.ico");
    // The build script runs on the host; the target decides whether a resource is needed.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/kladde.rc", embed_resource::NONE).manifest_required().unwrap();
    }
}
