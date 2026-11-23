fn main() {
    println!("cargo:rerun-if-changed=../../assets/satin.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/satin.ico");
        resource.set("ProductName", "Satin");
        resource.set("FileDescription", "Satin launcher");
        resource.compile().expect("compile launcher resources");
    }
}
