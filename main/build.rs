fn main() {
    println!("cargo:rustc-link-arg-bins=-Tlinkall.x");
    println!("cargo:rustc-link-arg-bins=-Tapp_desc.x");

    // Make sure we rerun if the linker script changes
    println!("cargo:rerun-if-changed=app_desc.x");

    // Load .env file and expose vars at compile time via env!()
    let mut env_vars = std::collections::HashMap::new();
    if let Ok(iter) = dotenvy::dotenv_iter() {
        for item in iter {
            if let Ok((key, val)) = item {
                println!("cargo:rustc-env={}={}", key, val);
                env_vars.insert(key, val);
            }
        }
    } else {
        panic!("Missing .env file — copy .env.example to .env and fill in your values");
    }

    // Validate required env vars are present
    let required = [
        "GET_STARTED_URL",
        "SUPPORT_URL",
        "MQTT_BROKER",
        "MQTT_PORT",
        "MQTT_CLIENT_ID",
        "CA_CERT_PATH",
        "CLIENT_CERT_PATH",
        "PRIVATE_KEY_PATH",
        "OTA_CA_CERT_PATH",
    ];
    for key in required {
        if !env_vars.contains_key(key) {
            panic!("Missing required .env variable: {key}");
        }
    }

    // Validate certificate file paths exist
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    for key in ["CA_CERT_PATH", "CLIENT_CERT_PATH", "PRIVATE_KEY_PATH", "OTA_CA_CERT_PATH"] {
        let rel_path = &env_vars[key];
        let full_path = format!("{}/{}", manifest_dir, rel_path);
        if !std::path::Path::new(&full_path).exists() {
            panic!("{key}={rel_path} — file not found at {full_path}");
        }
        // Rebuild if cert files change
        println!("cargo:rerun-if-changed={}", full_path);
    }

    // Rebuild if .env changes
    println!("cargo:rerun-if-changed=.env");
}
