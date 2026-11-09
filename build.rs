//! Embeds the application icon into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=assets/icons/koeboard.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/icons/koeboard.ico")
            .compile()
            .expect("embed Windows icon");
    }
}
