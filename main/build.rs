fn main() {
    println!("cargo:rustc-link-arg-bins=-Tlinkall.x");
    println!("cargo:rustc-link-arg-bins=-Tapp_desc.x");

    // Make sure we rerun if the linker script changes
    println!("cargo:rerun-if-changed=app_desc.x");

    // Load .env file and expose vars at compile time via env!()
    if let Ok(iter) = dotenvy::dotenv_iter() {
        for item in iter {
            if let Ok((key, val)) = item {
                println!("cargo:rustc-env={}={}", key, val);
            }
        }
    }

    // Rebuild if .env changes
    println!("cargo:rerun-if-changed=.env");
}
