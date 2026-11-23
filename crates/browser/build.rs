fn main() {
    println!("cargo:rerun-if-changed=../../assets/browser.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            println!("cargo:rustc-link-arg-bin=rust-browser=/STACK:4194304");
        }
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/browser.ico");
        resource.set("ProductName", "Rust Browser");
        resource.set(
            "FileDescription",
            "Rust Browser — independent Rust web engine",
        );
        resource.compile().expect("compile Windows resources");
    }
}
