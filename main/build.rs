fn main() {
    println!("cargo:rustc-link-arg-bins=-Tlinkall.x");
    println!("cargo:rustc-link-arg-bins=-Tapp_desc.x");

    // Make sure we rerun if the linker script changes
    println!("cargo:rerun-if-changed=app_desc.x");
}
