use fanqie_core::api::Client;

// Run with: cargo test --test live -- --ignored --nocapture
#[test]
#[ignore]
fn live_anonymous_chain() {
    let mut c = Client::new();
    let book_id = "7362803513664998424";
    let info = c.book_info(book_id).unwrap();
    println!("book: {}", info.get("bookName").unwrap_or(&serde_json::Value::Null));

    let dir = c.directory(book_id).unwrap();
    println!("chapters: {}", dir.len());
    assert!(!dir.is_empty());

    let first = &dir[0];
    let ch = c.chapter(first).unwrap();
    println!("title: {} order: {} len: {}", ch.title, ch.order, ch.text.len());
    assert!(ch.text.chars().count() > 100, "chapter too short");
    assert!(
        !ch.text.chars().any(|x| (0xE000..=0xF8FF).contains(&x as u32)),
        "residual PUA found"
    );
    println!("decoded head: {}", ch.text.chars().take(80).collect::<String>());

    let results = c.search("剑来", 0, 3).unwrap();
    println!("search hits: {}", results.len());
    assert!(!results.is_empty());
}

#[test]
#[ignore]
fn live_login_chain() {
    let mut c = Client::new();
    let vault = fanqie_core::vault::Vault::new(cookie_vault_path());
    if let Some(json) = vault.load().unwrap() {
        c.restore_cookies(&json);
    }
    let me = c.user_info().expect("not logged in — run the vault import first");
    println!("user: {:?}", me.get("name"));
    let shelf = c.bookshelf().unwrap();
    println!("bookshelf entries: {}", shelf.len());
    let prog = c.progress().unwrap();
    println!("progress: {}", prog.to_string().chars().take(120).collect::<String>());
}

fn cookie_vault_path() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("APPDATA").unwrap()).join("fanqie-desktop").join("cookies.bin")
}
