fn main() {
    println!("cargo:rerun-if-changed=assets/icon/windows/mdrdp.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon/windows/mdrdp.ico");
        if let Err(e) = res.compile() {
            // A cross type-check from macOS has no Windows resource compiler; the icon
            // is cosmetic, so a missing toolchain must not fail that mandated check.
            // Native Windows builds have rc.exe and do embed the icon.
            println!("cargo:warning=windows icon not embedded: {e}");
        }
    }
}
