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
        "APP_STORE_URL",
        "MQTT_BROKER",
        "MQTT_PORT",
        "CA_CERT_PATH",
        "CLIENT_CERT_PATH",
        "PRIVATE_KEY_PATH",
        "OTA_CA_CERT_PATH",
        "OTA_AES_KEY_PATH",
    ];
    for key in required {
        if !env_vars.contains_key(key) {
            panic!("Missing required .env variable: {key}");
        }
    }

    // Validate certificate / key file paths exist
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    for key in [
        "CA_CERT_PATH",
        "CLIENT_CERT_PATH",
        "PRIVATE_KEY_PATH",
        "OTA_CA_CERT_PATH",
        "OTA_AES_KEY_PATH",
    ] {
        let rel_path = &env_vars[key];
        let full_path = format!("{}/{}", manifest_dir, rel_path);
        if !std::path::Path::new(&full_path).exists() {
            panic!("{key}={rel_path} — file not found at {full_path}");
        }
        // Rebuild if cert files change
        println!("cargo:rerun-if-changed={}", full_path);
    }

    // Validate OTA_AES_KEY_PATH file contents: trimmed must be 64 ASCII hex chars
    // (32 bytes for AES-256-GCM). Done at build time so a malformed key file
    // fails the build with a clear message instead of a runtime panic.
    {
        let key_full_path = format!("{}/{}", manifest_dir, &env_vars["OTA_AES_KEY_PATH"]);
        let key_contents = std::fs::read_to_string(&key_full_path)
            .unwrap_or_else(|e| panic!("Failed to read OTA AES key file {}: {}", key_full_path, e));
        let trimmed = key_contents.trim();
        if trimmed.len() != 64 {
            panic!(
                "OTA AES key file {} must contain 64 hex chars (32 bytes); got {} chars after trim",
                key_full_path,
                trimmed.len()
            );
        }
        if !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            panic!(
                "OTA AES key file {} must contain only ASCII hex chars (0-9, a-f, A-F)",
                key_full_path
            );
        }
    }

    // Rebuild if .env changes
    println!("cargo:rerun-if-changed=.env");
}
