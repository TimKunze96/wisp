fn main() {
    println!("cargo:rerun-if-changed=assets/icons/wisp.ico");
    // icon and version info for the Windows exe (resource 1 is what GPUI uses
    // for the window); needs the Windows resource compiler, so only on Windows
    let host = std::env::var("HOST").unwrap_or_default();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") && host.contains("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icons/wisp.ico").set("ProductName", "Wisp").set("FileDescription", "Wisp");
        res.compile().expect("Windows resources");
    }
}
