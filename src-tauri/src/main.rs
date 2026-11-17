#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use fanqie_core::api::Client;

#[tauri::command]
fn core_ping() -> String {
    let mut c = Client::new();
    let vault = fanqie_core::vault::Vault::new(cookie_vault_path());
    if let Some(json) = vault.load().unwrap_or(None) {
        c.restore_cookies(&json);
    }
    match c.user_info() {
        Ok(v) => format!("ok: {:?}", v.get("name")),
        Err(e) => format!("err: {e}"),
    }
}

fn cookie_vault_path() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("APPDATA").unwrap())
        .join("fanqie-desktop")
        .join("cookies.bin")
}

fn main() {
    // FQ_SMOKE=1 → create hidden, move fully off-screen, then show: the
    // webview renders (PrintWindow-capturable) but the user sees nothing.
    let smoke = std::env::var("FQ_SMOKE").as_deref() == Ok("1");
    tauri::Builder::default()
        .setup(move |app| {
            let mut w = tauri::WebviewWindowBuilder::new(
                app, "main", tauri::WebviewUrl::App("index.html".into()))
                .title("番茄小说")
                .inner_size(1180.0, 760.0)
                .min_inner_size(900.0, 600.0);
            if smoke {
                w = w.visible(false).skip_taskbar(true);
            }
            let win = w.build()?;
            if smoke {
                win.set_position(tauri::PhysicalPosition::new(-20000i32, -20000i32))?;
                win.show()?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![core_ping])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
