#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use fanqie_core::api::Client;
use fanqie_core::store::{BookRow, Store};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tokio::sync::oneshot;

const SIGNER_JS: &str = r#"
(function () {
  function inv(cmd, payload) {
    return window.__TAURI_INTERNALS__.invoke(cmd, payload);
  }
  function ready() {
    inv('signer_ready', { href: location.href }).catch(function () {});
  }
  window.__fqFetch = async function (id, url) {
    var out = { id: id, ok: false, status: 0, body: '', zhal: '' };
    try {
      var r = await fetch(url, { credentials: 'include' });
      out.ok = r.ok;
      out.status = r.status;
      out.body = await r.text();
      try { out.zhal = r.headers.get('x-tt-zhal') || ''; } catch (e) {}
    } catch (e) {
      out.body = String(e);
    }
    inv('bridge_deliver', { p: out }).catch(function () {});
  };
  window.addEventListener('load', ready);
  document.addEventListener('DOMContentLoaded', ready);
  setTimeout(ready, 0);
})();
"#;

struct Bridge {
    pending: Mutex<HashMap<String, oneshot::Sender<SignPayload>>>,
}

struct AppState {
    client: Mutex<Client>,
    store: Mutex<Store>,
    bridge: Bridge,
    signer_ready: AtomicBool,
    seq: AtomicU64,
}

#[derive(serde::Deserialize)]
struct SignPayload {
    id: String,
    ok: bool,
    status: u16,
    #[serde(default)]
    body: String,
    #[serde(default)]
    zhal: String,
}

#[derive(Serialize, Clone)]
struct SignedBody {
    status: u16,
    body: String,
    zhal: String,
}

fn data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("APPDATA").unwrap()).join("fanqie-desktop")
}

fn cookie_vault_path() -> std::path::PathBuf {
    data_dir().join("cookies.bin")
}

fn font_hash_from_zhal(zhal: &str) -> Option<String> {
    let idx = zhal.find("f=")?;
    let rest = &zhal[idx + 2..];
    let end = rest.find([';', ' ', ',']).unwrap_or(rest.len());
    let hash = rest[..end].trim_matches('"');
    if hash.is_empty() {
        None
    } else {
        Some(hash.to_string())
    }
}

#[tauri::command]
async fn signer_ready(st: State<'_, AppState>) -> Result<(), String> {
    st.signer_ready.store(true, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
fn bridge_deliver(st: State<'_, AppState>, p: SignPayload) -> Result<(), String> {
    let tx = st.bridge.pending.lock().unwrap().remove(&p.id);
    if let Some(tx) = tx {
        let _ = tx.send(p);
    }
    Ok(())
}

async fn signed_fetch(
    app: &AppHandle,
    st: &AppState,
    url: &str,
) -> Result<SignedBody, String> {
    for _ in 0..120 {
        if st.signer_ready.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if !st.signer_ready.load(Ordering::SeqCst) {
        return Err("签名页尚未就绪".into());
    }
    let Some(win) = app.get_webview_window("signer") else {
        return Err("签名窗口不存在".into());
    };
    let id = format!("r{}", st.seq.fetch_add(1, Ordering::SeqCst));
    let (tx, rx) = oneshot::channel::<SignPayload>();
    st.bridge.pending.lock().unwrap().insert(id.clone(), tx);
    let js = format!(
        "window.__fqFetch({}, {})",
        serde_json::to_string(&id).unwrap(),
        serde_json::to_string(url).unwrap()
    );
    if let Err(e) = win.eval(&js) {
        st.bridge.pending.lock().unwrap().remove(&id);
        return Err(format!("eval: {e}"));
    }
    match tokio::time::timeout(Duration::from_secs(40), rx).await {
        Ok(Ok(p)) => {
            if p.ok {
                Ok(SignedBody { status: p.status, body: p.body, zhal: p.zhal })
            } else {
                Err(format!("签名请求失败 HTTP {}: {}", p.status, &p.body[..p.body.len().min(160)]))
            }
        }
        Ok(Err(_)) => Err("签名通道关闭".into()),
        Err(_) => {
            st.bridge.pending.lock().unwrap().remove(&id);
            Err("签名超时".into())
        }
    }
}

/// reqwest's blocking client builds/drops its own runtime, which tokio forbids
/// on async workers — every network/DB command runs on the blocking pool.
async fn blocking<T, F>(app: &AppHandle, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&AppState) -> Result<T, String> + Send + 'static,
{
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        f(st.inner())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn core_ping(app: AppHandle) -> Result<String, String> {
    blocking(&app, |st| {
        let mut c = st.client.lock().unwrap();
        match c.user_info() {
            Ok(v) => Ok(format!("ok: {:?}", v.get("name"))),
            Err(e) => Ok(format!("err: {e}")),
        }
    })
    .await
}

#[tauri::command]
async fn account(app: AppHandle) -> Result<serde_json::Value, String> {
    blocking(&app, |st| {
        let mut c = st.client.lock().unwrap();
        let v = c.user_info()?;
        Ok(serde_json::json!({
            "name": v.get("name").cloned().unwrap_or_default(),
            "avatar": v.get("avatar_url").or_else(|| v.get("avatar").or_else(|| v.get("avatarUri"))).cloned().unwrap_or_default(),
            "raw": v,
        }))
    })
    .await
}

#[tauri::command]
async fn shelf(app: AppHandle) -> Result<serde_json::Value, String> {
    blocking(&app, |st| {
        let mut c = st.client.lock().unwrap();
        let books = c.shelf_books()?;
        Ok(serde_json::to_value(books).unwrap_or_default())
    })
    .await
}

#[tauri::command]
async fn search_books(
    app: AppHandle,
    query: String,
) -> Result<serde_json::Value, String> {
    let url = Client::search_url(&query, 0, 10);
    let resp = {
        let state = app.state::<AppState>();
        signed_fetch(&app, &state, &url).await?
    };
    let hash = font_hash_from_zhal(&resp.zhal);
    let books = blocking(&app, move |st| {
        let mut c = st.client.lock().unwrap();
        c.search_from_body(&resp.body, hash.as_deref())
    })
    .await?;
    Ok(serde_json::to_value(books).unwrap_or_default())
}

#[tauri::command]
async fn book_detail(app: AppHandle, book_id: String) -> Result<serde_json::Value, String> {
    blocking(&app, move |st| {
        let mut c = st.client.lock().unwrap();
        Ok(serde_json::to_value(c.book_detail(&book_id)?).unwrap_or_default())
    })
    .await
}

#[tauri::command]
async fn directory(app: AppHandle, book_id: String) -> Result<serde_json::Value, String> {
    blocking(&app, move |st| {
        let mut c = st.client.lock().unwrap();
        Ok(serde_json::to_value(c.directory_meta(&book_id)?).unwrap_or_default())
    })
    .await
}

#[tauri::command]
async fn chapter(
    app: AppHandle,
    book_id: String,
    item_id: String,
) -> Result<serde_json::Value, String> {
    blocking(&app, move |st| {
        if let Some((title, text)) = st.store.lock().unwrap().get_chapter(&item_id)? {
            return Ok(serde_json::json!({
                "item_id": item_id, "book_id": book_id, "title": title, "text": text,
                "need_pay": false, "is_lock": false, "cached": true
            }));
        }
        let ch = {
            let mut c = st.client.lock().unwrap();
            c.chapter(&item_id)?
        };
        st.store
            .lock()
            .unwrap()
            .cache_chapter(&ch.item_id, &book_id, &ch.title, &ch.text)?;
        Ok(serde_json::to_value(&ch).unwrap_or_default())
    })
    .await
}

#[tauri::command]
async fn remember_book(app: AppHandle, book_id: String) -> Result<(), String> {
    blocking(&app, move |st| {
        let detail = {
            let mut c = st.client.lock().unwrap();
            c.book_detail(&book_id)?
        };
        st.store.lock().unwrap().remember_book(&BookRow {
            book_id: detail.book_id,
            name: detail.name,
            author: detail.author,
            thumb: detail.thumb,
            abstract_text: detail.abstract_text,
            last_item_id: None,
            chapter_order: 0,
        })
    })
    .await
}

#[tauri::command]
async fn local_books(app: AppHandle) -> Result<serde_json::Value, String> {
    blocking(&app, |st| {
        let books = st.store.lock().unwrap().list_books()?;
        Ok(serde_json::to_value(books).unwrap_or_default())
    })
    .await
}

#[tauri::command]
async fn set_progress(
    app: AppHandle,
    book_id: String,
    item_id: String,
    chapter_order: i64,
) -> Result<(), String> {
    blocking(&app, move |st| {
        st.store
            .lock()
            .unwrap()
            .set_progress(&book_id, &item_id, chapter_order)
    })
    .await
}

#[tauri::command]
fn win_min(window: tauri::WebviewWindow) -> Result<(), String> {
    window.minimize().map_err(|e| e.to_string())
}

#[tauri::command]
fn win_max_toggle(window: tauri::WebviewWindow) -> Result<bool, String> {
    if window.is_maximized().map_err(|e| e.to_string())? {
        window.unmaximize().map_err(|e| e.to_string())?;
        Ok(false)
    } else {
        window.maximize().map_err(|e| e.to_string())?;
        Ok(true)
    }
}

#[tauri::command]
fn win_close(window: tauri::WebviewWindow) -> Result<(), String> {
    window.close().map_err(|e| e.to_string())
}

fn main() {
    // FQ_SMOKE=1 → main window off-screen; signer window stays invisible.
    let smoke = std::env::var("FQ_SMOKE").as_deref() == Ok("1");

    let mut client = Client::new();
    let vault = fanqie_core::vault::Vault::new(cookie_vault_path());
    if let Ok(Some(json)) = vault.load() {
        client.restore_cookies(&json);
    }
    std::fs::create_dir_all(data_dir()).ok();
    let store = Store::open(data_dir().join("library.sqlite")).expect("open library");

    tauri::Builder::default()
        .manage(AppState {
            client: Mutex::new(client),
            store: Mutex::new(store),
            bridge: Bridge { pending: Mutex::new(HashMap::new()) },
            signer_ready: AtomicBool::new(false),
            seq: AtomicU64::new(0),
        })
        .setup(move |app| {
            let mut w = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("番茄小说")
            .inner_size(1180.0, 760.0)
            .min_inner_size(900.0, 600.0)
            .decorations(false);
            if smoke {
                w = w.visible(false).skip_taskbar(true);
            }
            let win = w.build()?;
            if smoke {
                win.set_position(tauri::PhysicalPosition::new(-20000i32, -20000i32))?;
                win.show()?;
            }

            let signer_url = tauri::Url::parse("https://fanqienovel.com/")?;
            let mut sb = tauri::WebviewWindowBuilder::new(
                app,
                "signer",
                tauri::WebviewUrl::External(signer_url),
            )
            .title("signer")
            .inner_size(420.0, 320.0)
            .visible(false)
            .skip_taskbar(true)
            .initialization_script(SIGNER_JS);
            if std::env::var("FQ_SIGNER_OFFSCREEN").as_deref() == Ok("1") {
                sb = sb.visible(false);
            }
            let signer = sb.build()?;
            if std::env::var("FQ_SIGNER_OFFSCREEN").as_deref() == Ok("1") {
                signer.set_position(tauri::PhysicalPosition::new(-20000i32, -20000i32))?;
                signer.show()?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            core_ping,
            signer_ready,
            bridge_deliver,
            account,
            shelf,
            search_books,
            book_detail,
            directory,
            chapter,
            remember_book,
            local_books,
            set_progress,
            win_min,
            win_max_toggle,
            win_close
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
