fn main() {
    println!("cargo:rerun-if-changed=../../assets/satin.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            println!("cargo:rustc-link-arg-bin=rust-browser=/STACK:8388608");
        }
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/satin.ico");
        resource.set("ProductName", "Satin");
        resource.set("FileDescription", "Satin — independent Rust web browser");
        resource.compile().expect("compile Windows resources");
    }
}
