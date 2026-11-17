use fanqie_core::{api::CookieJar, vault::Vault};

// usage: cargo run --example import_cookies -- <cookies.json path>
fn main() {
    let path = std::env::args().nth(1).expect("cookies.json path");
    let raw = std::fs::read_to_string(path).expect("read");
    let jar = CookieJar::from_json(&raw);
    let vault_path = std::path::PathBuf::from(std::env::var("APPDATA").unwrap())
        .join("fanqie-desktop")
        .join("cookies.bin");
    Vault::new(vault_path).save(&jar.to_json()).expect("save");
    println!("imported {} cookies", jar.cookies.len());
}
