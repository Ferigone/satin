fn main() {
    println!("cargo:rerun-if-changed=../../assets/browser.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/browser.ico");
        resource.set("ProductName", "Rust Browser");
        resource.set("FileDescription", "Rust Browser launcher");
        resource.compile().expect("compile launcher resources");
    }
}
