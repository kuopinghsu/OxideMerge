//! Embeds the application icon from `assets/oxidemerge.ico` and the version details in Windows
//! executables, so Explorer, the taskbar and the file properties show them. The window icon on
//! every platform comes from `assets/oxidemerge.png` at runtime.

fn main() {
    println!("cargo:rerun-if-changed=assets/oxidemerge.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/oxidemerge.ico");
    resource.set("ProductName", "OxideMerge");
    resource.set("FileDescription", "OxideMerge");
    resource.set("OriginalFilename", "oxidemerge.exe");
    resource.set("LegalCopyright", "MIT License");
    // Cross-checking for Windows from another system has no resource compiler; the program still
    // builds, just without the icon.
    if let Err(err) = resource.compile() {
        println!("cargo:warning=The Windows icon was not embedded: {err}");
    }
}
