#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use fanqie_core::api::Client;
mod accounts;
use accounts::Accounts;
use fanqie_core::store::BookRow;
use serde::{Deserialize, Serialize};
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
    accounts: Mutex<Accounts>,
    login: Mutex<Option<std::path::PathBuf>>,
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
    std::env::var_os("FQ_DATA_DIR").map(std::path::PathBuf::from).unwrap_or_else(||
        std::path::PathBuf::from(std::env::var("APPDATA").unwrap()).join("fanqie-desktop"))
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
        st.accounts.lock().unwrap().current()
    })
    .await
}

#[tauri::command]
async fn shelf(app: AppHandle) -> Result<serde_json::Value, String> {
    blocking(&app, |st| {
        Ok(st.accounts.lock().unwrap().books(true)?["books"].clone())
    })
    .await
}

#[tauri::command]
async fn home_rails(app: AppHandle) -> Result<serde_json::Value, String> {
    blocking(&app, |st| {
        let mut c = st.client.lock().unwrap();
        Ok(serde_json::to_value(c.home_rails()?).unwrap_or_default())
    })
    .await
}

#[tauri::command]
async fn community_posts(app: AppHandle) -> Result<serde_json::Value, String> {
    blocking(&app, |st| {
        let mut c = st.client.lock().unwrap();
        Ok(serde_json::to_value(c.community_posts()?).unwrap_or_default())
    })
    .await
}

#[tauri::command]
async fn post_detail(app: AppHandle, book_id: String, post_id: String) -> Result<serde_json::Value, String> {
    blocking(&app, move |_| {
        let mut c = Client::new();
        serde_json::to_value(c.post_detail(&book_id, &post_id)?).map_err(|e| e.to_string())
    }).await
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
async fn directory(app: AppHandle, book_id: String, account_id: Option<String>) -> Result<serde_json::Value, String> {
    blocking(&app, move |st| {
        let mut a = st.accounts.lock().unwrap();
        let id = a.route(&book_id, account_id.as_deref())?;
        a.with_session(&id, |c, _| Ok(serde_json::to_value(c.directory_meta(&book_id)?).unwrap_or_default()))
    }).await
}

#[tauri::command]
async fn chapter(
    app: AppHandle,
    book_id: String,
    item_id: String,
    account_id: Option<String>,
) -> Result<serde_json::Value, String> {
    blocking(&app, move |st| {
        let mut a = st.accounts.lock().unwrap();
        let id = a.route(&book_id, account_id.as_deref())?;
        if let Some((title, text)) = a.sessions[&id].store.get_chapter_for_book(&book_id, &item_id)? {
            return Ok(serde_json::json!({"item_id":item_id,"book_id":book_id,"title":title,"text":text,"need_pay":false,"is_lock":false,"cached":true}));
        }
        a.with_session(&id, |c, store| {
            let ch = c.chapter(&item_id)?;
            if ch.book_id != book_id { return Err("章节与图书不匹配".into()); }
            if !ch.need_pay && !ch.is_lock { store.cache_chapter(&ch.item_id, &book_id, &ch.title, &ch.text)?; }
            Ok(serde_json::to_value(ch).unwrap_or_default())
        })
    }).await
}

#[tauri::command]
async fn remember_book(app: AppHandle, book_id: String, account_id: Option<String>) -> Result<(), String> {
    blocking(&app, move |st| {
        let mut a = st.accounts.lock().unwrap();
        let id = a.route(&book_id, account_id.as_deref())?;
        a.with_session(&id, |c, store| {
            let d = c.book_detail(&book_id)?;
            store.remember_book(&BookRow { book_id:d.book_id,name:d.name,author:d.author,thumb:d.thumb,abstract_text:d.abstract_text,last_item_id:None,chapter_order:0 })
        })?;
        a.mark_recent(&book_id, &id)
    }).await
}

#[tauri::command]
async fn local_books(app: AppHandle) -> Result<serde_json::Value, String> {
    blocking(&app, |st| {
        Ok(st.accounts.lock().unwrap().books(false)?["books"].clone())
    })
    .await
}

#[tauri::command]
async fn set_progress(
    app: AppHandle,
    book_id: String,
    item_id: String,
    chapter_order: i64,
    account_id: Option<String>,
) -> Result<(), String> {
    blocking(&app, move |st| {
        let mut a = st.accounts.lock().unwrap();
        let id = a.route(&book_id, account_id.as_deref())?;
        a.sessions[&id].store.set_progress(&book_id, &item_id, chapter_order)?;
        a.mark_recent(&book_id, &id)
    }).await
}

#[tauri::command]
async fn account_list(app: AppHandle) -> Result<serde_json::Value, String> {
    blocking(&app, |st| Ok(st.accounts.lock().unwrap().list())).await
}
#[tauri::command]
async fn account_switch(app: AppHandle, account_id: String) -> Result<serde_json::Value, String> {
    blocking(&app, move |st| st.accounts.lock().unwrap().switch(&account_id)).await
}
#[tauri::command]
async fn account_logout(app: AppHandle, account_id: String) -> Result<serde_json::Value, String> {
    blocking(&app, move |st| st.accounts.lock().unwrap().logout(&account_id)).await
}
#[tauri::command]
async fn sync_accounts(app: AppHandle) -> Result<serde_json::Value, String> {
    blocking(&app, |st| st.accounts.lock().unwrap().books(true)).await
}
#[tauri::command]
async fn resolve_reading_account(app: AppHandle, book_id: String) -> Result<serde_json::Value, String> {
    blocking(&app, move |st| st.accounts.lock().unwrap().resolve(&book_id)).await
}

#[tauri::command]
async fn start_login(app: AppHandle) -> Result<(), String> {
    let handle = app.clone();
    blocking(&app, move |st| {
        let mut login = st.login.lock().unwrap();
        if login.is_some() { return Err("已有登录窗口，请完成或取消本次登录".into()); }
        let path = data_dir().join("login-profiles").join(accounts::nonce());
        std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
        let smoke = std::env::var("FQ_SMOKE").as_deref() == Ok("1");
        let mut builder = tauri::WebviewWindowBuilder::new(&handle, "account-login", tauri::WebviewUrl::External(tauri::Url::parse("https://fanqienovel.com/").unwrap()))
            .title("番茄小说 · 添加账号").inner_size(1000.0, 760.0).data_directory(path.clone())
            .visible(false).skip_taskbar(smoke).incognito(true)
            .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
            .on_navigation(|url| url.scheme() == "https" && url.host_str().map(|h| h == "fanqienovel.com" || h.ends_with(".fanqienovel.com")).unwrap_or(false));
        if smoke { builder = builder.position(-20000.0, -20000.0); }
        let win = builder.build().map_err(|e| e.to_string())?;
        // Closing via the titlebar hides the isolated profile until cancel/finish cleans it.
        let close_win = win.clone();
        win.on_window_event(move |event| { if let tauri::WindowEvent::CloseRequested { api, .. } = event { api.prevent_close(); let _ = close_win.hide(); } });
        *login = Some(path);
        show_main(&win)
    }).await
}
fn clean_login(app: &AppHandle, login: &mut Option<std::path::PathBuf>) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("account-login") {
        win.clear_all_browsing_data().map_err(|_| "无法清除登录窗口会话，请重试取消登录".to_owned())?;
        win.destroy().map_err(|e| e.to_string())?;
    }
    if let Some(path) = login.take() {
        // WebView2 may retain filesystem handles briefly. Browsing data is already cleared;
        // fresh random InPrivate profiles are never reused.
        let _ = std::fs::remove_dir_all(path);
    }
    Ok(())
}
#[tauri::command]
async fn cancel_login(app: AppHandle) -> Result<(), String> {
    let handle = app.clone();
    blocking(&app, move |st| clean_login(&handle, &mut st.login.lock().unwrap())).await
}
#[tauri::command]
async fn finish_login(app: AppHandle) -> Result<serde_json::Value, String> {
    let handle = app.clone();
    blocking(&app, move |st| {
        let mut login = st.login.lock().unwrap();
        if login.is_none() { return Err("请先打开官方登录页面".into()); }
        let win = handle.get_webview_window("account-login").ok_or("登录窗口已关闭，请取消后重试")?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        let cookies = win.cookies().map_err(|_| "无法读取官方登录会话，请重试".to_owned())?.into_iter().filter_map(|c| {
            let domain = c.domain()?.to_ascii_lowercase();
            if domain.trim_start_matches('.') != "fanqienovel.com" { return None; }
            let expires = c.expires_datetime().map(|e| e.unix_timestamp());
            if expires.is_some_and(|e| e <= now) { return None; }
            Some(fanqie_core::api::Cookie { name:c.name().to_owned(),value:c.value().to_owned(),domain,path:c.path().unwrap_or("/").to_owned(),expires })
        }).collect();
        st.accounts.lock().unwrap().add(fanqie_core::api::CookieJar { cookies }, || clean_login(&handle, &mut login))
    }).await
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
fn win_fullscreen(window: tauri::WebviewWindow, enabled: bool) -> Result<bool, String> {
    if std::env::var("FQ_SMOKE").as_deref() == Ok("1") {
        return Ok(false);
    }
    window.set_fullscreen(enabled).map_err(|e| e.to_string())?;
    Ok(enabled)
}

#[tauri::command]
fn win_close(window: tauri::WebviewWindow) -> Result<(), String> {
    window.close().map_err(|e| e.to_string())
}

#[derive(Serialize, Clone)]
struct BossKeyStatus {
    enabled: bool,
    registered: bool,
    smoke: bool,
    shortcut: String,
    preferred: String,
    error: Option<String>,
    notice: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct BossKeyConfig {
    enabled: bool,
    shortcut: String,
}

impl BossKeyConfig {
    fn load() -> Self {
        let path = data_dir().join("boss-key.json");
        if let Ok(bytes) = std::fs::read(&path) {
            if let Ok(config) = serde_json::from_slice::<BossKeyConfig>(&bytes) {
                return config;
            }
            if let Ok(enabled) = serde_json::from_slice::<bool>(&bytes) {
                return BossKeyConfig { enabled, shortcut: "Alt+Z".into() };
            }
        }
        BossKeyConfig { enabled: true, shortcut: "Alt+Z".into() }
    }
    fn save(&self) -> Result<(), String> {
        std::fs::create_dir_all(data_dir()).map_err(|e| e.to_string())?;
        std::fs::write(data_dir().join("boss-key.json"), serde_json::to_vec(self).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    }
}

struct BossKey(Mutex<BossKeyStatus>);

fn parse_boss_shortcut(text: &str) -> Result<tauri_plugin_global_shortcut::Shortcut, String> {
    use std::str::FromStr;
    use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};
    let mut mods = Modifiers::empty();
    let mut code: Option<Code> = None;
    for part in text.split('+').map(str::trim).filter(|part| !part.is_empty()) {
        let lower = part.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "alt" => mods |= Modifiers::ALT,
            "shift" => mods |= Modifiers::SHIFT,
            "super" | "win" | "meta" => mods |= Modifiers::SUPER,
            _ => {
                if code.is_some() { return Err("快捷键只能包含一个主键".into()); }
                let name = if lower.len() == 1 && lower.chars().all(|c| c.is_ascii_alphanumeric()) {
                    if lower.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                        format!("Digit{}", lower)
                    } else {
                        format!("Key{}", lower.to_ascii_uppercase())
                    }
                } else if lower.starts_with('f')
                    && lower[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n))
                {
                    lower.to_ascii_uppercase()
                } else {
                    match lower.as_str() {
                        "space" => "Space".into(),
                        "up" | "arrowup" => "ArrowUp".into(),
                        "down" | "arrowdown" => "ArrowDown".into(),
                        "left" | "arrowleft" => "ArrowLeft".into(),
                        "right" | "arrowright" => "ArrowRight".into(),
                        "pagedown" => "PageDown".into(),
                        "pageup" => "PageUp".into(),
                        "home" => "Home".into(),
                        "end" => "End".into(),
                        "insert" => "Insert".into(),
                        "delete" => "Delete".into(),
                        "enter" | "return" => "Enter".into(),
                        other => return Err(format!("不支持的主键：{other}")),
                    }
                };
                code = Some(Code::from_str(&name).map_err(|_| format!("不支持的主键：{name}"))?);
            }
        }
    }
    if mods.is_empty() { return Err("快捷键至少需要一个 Ctrl、Alt 或 Shift".into()); }
    Ok(Shortcut::new(Some(mods), code.ok_or("快捷键缺少主键")?))
}

fn smoke_mode() -> bool {
    std::env::var("FQ_SMOKE").as_deref() == Ok("1")
}

fn show_main(win: &tauri::WebviewWindow) -> Result<(), String> {
    if smoke_mode() {
        #[cfg(target_os = "windows")]
        {
            #[link(name = "user32")]
            extern "system" { fn ShowWindow(hwnd: *mut std::ffi::c_void, command: i32) -> i32; }
            let hwnd = win.hwnd().map_err(|e| e.to_string())?;
            unsafe { ShowWindow(hwnd.0 as *mut std::ffi::c_void, 4); }
            return Ok(());
        }
        #[cfg(not(target_os = "windows"))]
        return Err("离屏显示测试仅支持 Windows".into());
    }
    win.show().map_err(|e| e.to_string())
}

fn toggle_main(app: &AppHandle) -> Result<bool, String> {
    let win = app.get_webview_window("main").ok_or("主窗口不存在")?;
    #[cfg(target_os = "windows")]
    let visible = {
        #[link(name = "user32")]
        extern "system" { fn IsWindowVisible(hwnd: *mut std::ffi::c_void) -> i32; }
        let hwnd = win.hwnd().map_err(|e| e.to_string())?;
        unsafe { IsWindowVisible(hwnd.0 as *mut std::ffi::c_void) != 0 }
    };
    #[cfg(not(target_os = "windows"))]
    let visible = win.is_visible().map_err(|e| e.to_string())?;
    if visible {
        if smoke_mode() {
            #[cfg(target_os = "windows")]
            {
                #[link(name = "user32")]
                extern "system" { fn ShowWindow(hwnd: *mut std::ffi::c_void, command: i32) -> i32; }
                let hwnd = win.hwnd().map_err(|e| e.to_string())?;
                unsafe { ShowWindow(hwnd.0 as *mut std::ffi::c_void, 0); }
            }
        } else {
            win.hide().map_err(|e| e.to_string())?;
        }
        Ok(false)
    } else {
        if smoke_mode() {
            win.set_position(tauri::PhysicalPosition::new(-20000i32, -20000i32)).map_err(|e| e.to_string())?;
        } else {
            win.unminimize().map_err(|e| e.to_string())?;
        }
        show_main(&win)?;
        if !smoke_mode() { win.set_focus().map_err(|e| e.to_string())?; }
        Ok(true)
    }
}

fn shortcut_candidates(text: &str) -> Vec<String> {
    let parts: Vec<String> = text.split('+').map(|part| part.trim().to_ascii_lowercase()).collect();
    let has = |name: &str| parts.iter().any(|part| part == name);
    let (ctrl, shift) = (has("ctrl") || has("control"), has("shift"));
    let mut out = vec![text.to_string()];
    let mut push = |candidate: String| {
        if !out.contains(&candidate) { out.push(candidate); }
    };
    if !shift { push(format!("Shift+{text}")); }
    if !ctrl && !shift {
        push(format!("Ctrl+{text}"));
        push(format!("Ctrl+Shift+{text}"));
    } else if !ctrl {
        push(format!("Ctrl+{text}"));
    }
    out
}

fn try_register_available(
    preferred: &str,
    mut register: impl FnMut(tauri_plugin_global_shortcut::Shortcut) -> Result<(), String>,
) -> Result<(String, Option<String>), String> {
    let mut failure = String::new();
    for candidate in shortcut_candidates(preferred) {
        let Ok(shortcut) = parse_boss_shortcut(&candidate) else { continue; };
        match register(shortcut) {
            Ok(()) => {
                let notice = (candidate != preferred).then(|| {
                    format!(
                        "{preferred} 已被占用（可能来自另一个番茄小说窗口），本次改用 {candidate}。关闭占用方后点「重试」即可切回。"
                    )
                });
                return Ok((candidate, notice));
            }
            Err(e) => {
                if failure.is_empty() { failure = e; }
            }
        }
    }
    Err(failure)
}

fn unregister_boss(app: &AppHandle, status: &mut BossKeyStatus) {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    if !status.registered { return; }
    match parse_boss_shortcut(&status.shortcut)
        .and_then(|shortcut| app.global_shortcut().unregister(shortcut).map_err(|e| e.to_string()))
    {
        Ok(()) => { status.registered = false; status.notice = None; }
        Err(e) => status.error = Some(format!("停用快捷键失败，请重试：{e}")),
    }
}

fn register_boss_key(app: &AppHandle, status: &mut BossKeyStatus) {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;
    status.error = None;
    status.notice = None;
    if status.smoke || !status.enabled || status.registered { return; }
    let preferred = status.preferred.clone();
    let register = |shortcut| app.global_shortcut().register(shortcut).map_err(|e| e.to_string());
    match try_register_available(&preferred, register) {
        Ok((candidate, notice)) => {
            status.registered = true;
            status.shortcut = candidate;
            status.notice = notice;
        }
        Err(failure) => {
            status.shortcut = preferred.clone();
            status.error = Some(format!(
                "{preferred} 注册失败，可能被其他程序或另一个番茄小说窗口占用。关闭占用方后点「重试」，或点击快捷键换一个：{failure}"
            ));
        }
    }
}

fn save_boss_config(status: &BossKeyStatus) -> Result<(), String> {
    BossKeyConfig { enabled: status.enabled, shortcut: status.preferred.clone() }.save()
}

#[tauri::command]
fn boss_key_status(st: State<'_, BossKey>) -> BossKeyStatus {
    st.0.lock().unwrap().clone()
}

#[tauri::command]
fn boss_key_set_enabled(app: AppHandle, st: State<'_, BossKey>, enabled: bool) -> Result<BossKeyStatus, String> {
    let mut status = st.0.lock().unwrap();
    status.enabled = enabled;
    if enabled {
        status.shortcut = status.preferred.clone();
        register_boss_key(&app, &mut status);
    } else {
        unregister_boss(&app, &mut status);
    }
    save_boss_config(&status)?;
    Ok(status.clone())
}

#[tauri::command]
fn boss_key_set_shortcut(app: AppHandle, st: State<'_, BossKey>, shortcut: String) -> Result<BossKeyStatus, String> {
    parse_boss_shortcut(&shortcut)?;
    let mut status = st.0.lock().unwrap();
    unregister_boss(&app, &mut status);
    if status.error.is_some() { return Ok(status.clone()); }
    status.preferred = shortcut;
    status.shortcut = status.preferred.clone();
    status.error = None;
    status.notice = None;
    if status.enabled && !status.smoke {
        register_boss_key(&app, &mut status);
    }
    save_boss_config(&status)?;
    Ok(status.clone())
}

#[tauri::command]
fn boss_key_retry(app: AppHandle, st: State<'_, BossKey>) -> BossKeyStatus {
    let mut status = st.0.lock().unwrap();
    if status.smoke || !status.enabled { return status.clone(); }
    status.error = None;
    status.notice = None;
    let active = status.shortcut.clone();
    if status.registered {
        unregister_boss(&app, &mut status);
        if status.error.is_some() { return status.clone(); }
    }
    status.shortcut = status.preferred.clone();
    register_boss_key(&app, &mut status);
    if !status.registered && active != status.preferred {
        status.shortcut = active;
        register_boss_key(&app, &mut status);
    }
    status.clone()
}

#[tauri::command]
fn boss_key_smoke_toggle(app: AppHandle) -> Result<bool, String> {
    if !smoke_mode() { return Err("仅限 FQ_SMOKE 测试".into()); }
    toggle_main(&app)
}

fn main() {
    // FQ_SMOKE=1 → main window off-screen; signer window stays invisible.
    let smoke = std::env::var("FQ_SMOKE").as_deref() == Ok("1");

    let client = Client::new(); // Always anonymous; never share a login jar with search/signing.
    let accounts = Accounts::open(data_dir()).expect("open account storage");

    tauri::Builder::default()
        .plugin(tauri_plugin_global_shortcut::Builder::new().with_handler(|app, shortcut, event| {
            use tauri_plugin_global_shortcut::ShortcutState;
            if event.state != ShortcutState::Pressed || smoke_mode() { return; }
            let matches = {
                let st = app.state::<BossKey>();
                let status = st.0.lock().unwrap();
                status.enabled && parse_boss_shortcut(&status.shortcut).is_ok_and(|s| &s == shortcut)
            };
            if !matches { return; }
            let st = app.state::<BossKey>();
            let mut status = st.0.lock().unwrap();
            if let Err(e) = toggle_main(app) { status.error = Some(e); }
        }).build())
        .manage(BossKey({
            let config = BossKeyConfig::load();
            Mutex::new(BossKeyStatus {
                enabled: config.enabled,
                registered: false,
                smoke,
                shortcut: config.shortcut.clone(),
                preferred: config.shortcut,
                error: None,
                notice: None,
            })
        }))
        .manage(AppState {
            client: Mutex::new(client),
            accounts: Mutex::new(accounts),
            login: Mutex::new(None),
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
                w = w.visible(false).focused(false).skip_taskbar(true);
            }
            let win = w.build()?;
            if smoke {
                win.set_position(tauri::PhysicalPosition::new(-20000i32, -20000i32))?;
                show_main(&win).map_err(std::io::Error::other)?;
            }

            register_boss_key(app.handle(), &mut app.state::<BossKey>().0.lock().unwrap());

            let signer_url = tauri::Url::parse("https://fanqienovel.com/")?;
            let mut sb = tauri::WebviewWindowBuilder::new(
                app,
                "signer",
                tauri::WebviewUrl::External(signer_url),
            )
            .data_directory(data_dir().join("anonymous-signer").join(accounts::nonce()))
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
                show_main(&signer).map_err(std::io::Error::other)?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            core_ping,
            signer_ready,
            bridge_deliver,
            account,
            account_list,
            account_switch,
            account_logout,
            start_login,
            finish_login,
            cancel_login,
            sync_accounts,
            resolve_reading_account,
            shelf,
            home_rails,
            community_posts,
            post_detail,
            search_books,
            book_detail,
            directory,
            chapter,
            remember_book,
            local_books,
            set_progress,
            win_min,
            win_max_toggle,
            win_fullscreen,
            win_close,
            boss_key_status,
            boss_key_set_enabled,
            boss_key_set_shortcut,
            boss_key_retry,
            boss_key_smoke_toggle
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boss_shortcut_candidates_add_missing_modifiers() {
        assert_eq!(
            shortcut_candidates("Alt+Z"),
            ["Alt+Z", "Shift+Alt+Z", "Ctrl+Alt+Z", "Ctrl+Shift+Alt+Z"]
        );
        assert_eq!(
            shortcut_candidates("Ctrl+Alt+Z"),
            ["Ctrl+Alt+Z", "Shift+Ctrl+Alt+Z"]
        );
        assert_eq!(shortcut_candidates("Ctrl+Shift+Z"), ["Ctrl+Shift+Z"]);
        for shortcut in shortcut_candidates("Alt+Z") {
            assert!(parse_boss_shortcut(&shortcut).is_ok(), "{shortcut}");
        }
    }

    #[test]
    fn fallback_uses_the_next_free_shortcut() {
        let mut attempts = 0;
        let (shortcut, notice) = try_register_available("Alt+Z", |_| {
            attempts += 1;
            if attempts == 1 { Err("HotKey already registered".into()) } else { Ok(()) }
        })
        .unwrap();
        assert_eq!(shortcut, "Shift+Alt+Z");
        assert!(notice.unwrap().contains("Shift+Alt+Z"));
    }

    #[test]
    fn fallback_reports_the_first_failure() {
        let error = try_register_available("Alt+Z", |_| Err("HotKey already registered".into())).unwrap_err();
        assert!(error.contains("already registered"));
    }
}
