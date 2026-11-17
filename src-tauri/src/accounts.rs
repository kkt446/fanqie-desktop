use fanqie_core::{api::{Client, CookieJar}, store::{BookRow, Store}, vault::Vault};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::{BTreeMap, HashMap}, path::PathBuf};

#[derive(Clone, Serialize, Deserialize)]
pub struct AccountInfo {
    pub id: String,
    pub name: String,
    pub avatar: String,
    pub logged_in: bool,
    #[serde(skip_serializing, default)]
    pub identity: String,
}
#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    accounts: Vec<AccountInfo>,
    active_id: Option<String>,
    #[serde(default)]
    recent: HashMap<String, String>,
}
pub struct Session {
    pub client: Option<Client>,
    pub store: Store,
    verified: bool,
}
pub struct Accounts {
    root: PathBuf,
    manifest: Manifest,
    pub sessions: HashMap<String, Session>,
    // A resolved book never silently changes account, even after logout/switch.
    routes: HashMap<String, String>,
}
fn io(e: impl std::fmt::Display) -> String { e.to_string() }
pub fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 80 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
pub fn nonce() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!("a{:x}-{:x}-{:x}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos(), N.fetch_add(1, Ordering::Relaxed))
}
// Verified against official muye_faff5f61.js: /api/user/info/v2 populates
// loginInfo {id, name, avatar}. Display names never identify users.
// Keep identity encrypted and do not expose it through IPC or path names.
fn profile(v: &Value) -> Result<(String, String, String), String> {
    let id = v.get("id").and_then(|v| v.as_str().map(str::to_owned).or_else(|| v.as_u64().map(|n| n.to_string())))
        .filter(|s| s.len() <= 128 && s.bytes().all(|b| b.is_ascii_digit()) && s.bytes().any(|b| b != b'0'))
        .ok_or("尚未登录，或官方账号资料格式暂不支持，请在登录窗口完成登录后重试")?;
    let name = v.get("name").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).ok_or("账号资料缺少昵称，请重新登录")?.to_owned();
    let avatar = v.get("avatar").and_then(Value::as_str).filter(|s| s.starts_with("https://")).unwrap_or("").to_owned();
    Ok((id, name, avatar))
}
impl Accounts {
    pub fn open(root: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(root.join("accounts")).map_err(io)?;
        let mut manifest: Manifest = match Vault::new(root.join("accounts.bin")).load()? {
            Some(s) => serde_json::from_str(&s).map_err(|_| "账号索引损坏，原文件已保留".to_owned())?,
            None => Manifest::default(),
        };
        let mut sessions = HashMap::new();
        for info in &mut manifest.accounts {
            if !safe_id(&info.id) || sessions.contains_key(&info.id) { return Err("账号索引标识无效".into()); }
            let dir = root.join("accounts").join(&info.id);
            std::fs::create_dir_all(&dir).map_err(io)?;
            let cookie = Vault::new(dir.join("cookies.bin")).load()?;
            let client = cookie.map(|s| { let mut c = Client::new(); c.restore_cookies(&s); c });
            info.logged_in = client.is_some();
            sessions.insert(info.id.clone(), Session { client, store: Store::open(dir.join("library.sqlite"))?, verified: false });
        }
        let mut this = Self { root, manifest, sessions, routes: HashMap::new() };
        this.migrate()?;
        if this.manifest.active_id.as_ref().is_some_and(|id| this.require(id).is_err()) {
            this.manifest.active_id = None;
            this.save()?;
        }
        Ok(this)
    }
    fn dir(&self, id: &str) -> PathBuf { self.root.join("accounts").join(id) }
    fn save(&self) -> Result<(), String> {
        // AccountInfo intentionally hides identity in IPC; private manifest stores it explicitly.
        let mut v = serde_json::to_value(&self.manifest).map_err(io)?;
        for (v, a) in v["accounts"].as_array_mut().unwrap().iter_mut().zip(&self.manifest.accounts) { v["identity"] = json!(a.identity); }
        Vault::new(self.root.join("accounts.bin")).save(&v.to_string())
    }
    fn migrate(&mut self) -> Result<(), String> {
        let legacy = self.root.join("cookies.bin");
        if self.root.join("migration-v1.done").exists() {
            // Complete retirement if a crash occurred between marker and deletion.
            if legacy.exists() { std::fs::remove_file(&legacy).map_err(io)?; }
            return Ok(());
        }
        let library = self.root.join("library.sqlite");
        if !legacy.exists() && !library.exists() { return Ok(()); }
        let backup = self.root.join("backup-before-accounts");
        std::fs::create_dir_all(&backup).map_err(io)?;
        if legacy.exists() && !backup.join("cookies.bin").exists() { std::fs::copy(&legacy, backup.join("cookies.bin")).map_err(io)?; }
        if library.exists() && !backup.join("library.sqlite").exists() { Store::snapshot_file(&library, &backup.join("library.sqlite"))?; }
        let id = "legacy-v1";
        if !self.sessions.contains_key(id) {
            let dir = self.dir(id);
            std::fs::create_dir_all(&dir).map_err(io)?;
            if library.exists() && !dir.join("library.sqlite").exists() { Store::snapshot_file(&backup.join("library.sqlite"), &dir.join("library.sqlite"))?; }
            if legacy.exists() && !dir.join("cookies.bin").exists() { std::fs::copy(&legacy, dir.join("cookies.bin")).map_err(io)?; }
            let client = Vault::new(dir.join("cookies.bin")).load()?.map(|s| { let mut c = Client::new(); c.restore_cookies(&s); c });
            self.manifest.accounts.push(AccountInfo { id: id.into(), name: "原账号（待验证）".into(), avatar: String::new(), logged_in: client.is_some(), identity: String::new() });
            if client.is_some() { self.manifest.active_id = Some(id.into()); }
            self.sessions.insert(id.into(), Session { client, store: Store::open(dir.join("library.sqlite"))?, verified: false });
            self.save()?;
        }
        // Marker precedes retiring the old credential so a restart cannot re-import a logged-out account.
        std::fs::write(self.root.join("migration-v1.done"), b"1").map_err(io)?;
        if legacy.exists() { std::fs::remove_file(legacy).map_err(io)?; }
        Ok(())
    }
    pub fn list(&self) -> Value { json!({"accounts": self.manifest.accounts, "active_id": self.manifest.active_id}) }
    pub fn current(&mut self) -> Result<Value, String> {
        let id = self.manifest.active_id.clone().ok_or("请先登录账号")?;
        self.verify(&id)?;
        let a = self.info(&id)?;
        Ok(json!({"id":a.id,"name":a.name,"avatar":a.avatar}))
    }
    fn info(&self, id: &str) -> Result<&AccountInfo, String> { self.manifest.accounts.iter().find(|a| a.id == id).ok_or("账号不存在".into()) }
    fn require(&self, id: &str) -> Result<(), String> {
        if !safe_id(id) { return Err("无效账号标识".into()); }
        match self.sessions.get(id) { Some(s) if s.client.is_some() => Ok(()), _ => Err("该账号已退出，请重新登录或明确选择其他账号".into()) }
    }
    pub fn switch(&mut self, id: &str) -> Result<Value, String> { self.require(id)?; self.manifest.active_id = Some(id.into()); self.save()?; Ok(self.list()) }
    pub fn logout(&mut self, id: &str) -> Result<Value, String> {
        self.info(id)?;
        let path = self.dir(id).join("cookies.bin");
        // Explicit logout also retires this migrated account's encrypted cookie
        // backup; its library backup and all reading history remain untouched.
        if id == "legacy-v1" {
            for old in [self.root.join("cookies.bin"), self.root.join("cookies.pending"), self.root.join("backup-before-accounts/cookies.bin")] {
                if old.exists() { std::fs::remove_file(old).map_err(io)?; }
            }
        }
        // Persist deletion first; do not report a successful logout on filesystem failure.
        let pending = path.with_extension("pending");
        if pending.exists() { std::fs::remove_file(pending).map_err(io)?; }
        if path.exists() { std::fs::remove_file(path).map_err(io)?; }
        self.sessions.get_mut(id).unwrap().client = None;
        self.manifest.accounts.iter_mut().find(|a| a.id == id).unwrap().logged_in = false;
        if self.manifest.active_id.as_deref() == Some(id) { self.manifest.active_id = None; }
        self.save()?;
        Ok(self.list())
    }
    pub fn add(&mut self, jar: CookieJar, clear_login: impl FnOnce() -> Result<(), String>) -> Result<Value, String> {
        let mut client = Client::new(); client.jar = jar;
        let v = client.user_info().map_err(|_| "尚未登录或登录已过期，请在官方页面完成登录后重试".to_owned())?;
        let (identity, name, avatar) = profile(&v)?;
        // Bind an imported legacy session before deduplicating a re-login.
        if self.sessions.get("legacy-v1").is_some_and(|s| s.client.is_some()) && self.info("legacy-v1")?.identity.is_empty() {
            let _ = self.verify("legacy-v1");
        }
        let id = self.manifest.accounts.iter().find(|a| a.identity == identity).map(|a| a.id.clone()).unwrap_or_else(nonce);
        // Registration and logout share the account lock. No browser session
        // may survive a successful account registration (or race a logout).
        clear_login()?;
        let dir = self.dir(&id);
        std::fs::create_dir_all(&dir).map_err(io)?;
        Vault::new(dir.join("cookies.bin")).save(&client.jar.to_json())?;
        let store = Store::open(dir.join("library.sqlite"))?;
        let info = AccountInfo { id: id.clone(), name, avatar, logged_in: true, identity };
        if let Some(a) = self.manifest.accounts.iter_mut().find(|a| a.id == id) { *a = info; } else { self.manifest.accounts.push(info); }
        self.sessions.insert(id.clone(), Session { client: Some(client), store, verified: true });
        self.manifest.active_id = Some(id);
        self.save()?;
        Ok(self.list())
    }
    pub fn route(&mut self, book: &str, explicit: Option<&str>) -> Result<String, String> {
        let id = explicit.map(str::to_owned).or_else(|| self.routes.get(book).cloned()).or_else(|| self.manifest.active_id.clone()).ok_or("请先登录账号")?;
        self.require(&id)?;
        self.routes.insert(book.into(), id.clone());
        Ok(id)
    }
    pub fn resolve(&mut self, book: &str) -> Result<Value, String> {
        // No automatic alternative after logout: the user must explicitly switch/login first.
        let active = self.manifest.active_id.clone().ok_or("请先登录，或明确选择其他账号")?;
        self.require(&active)?;
        let recent = self.manifest.recent.get(book).filter(|id| self.require(id).is_ok()).cloned();
        let read = self.manifest.accounts.iter().filter(|a| self.require(&a.id).is_ok()).find(|a| self.sessions[&a.id].store.list_books().unwrap_or_default().iter().any(|b| b.book_id == book && b.last_item_id.is_some())).map(|a| a.id.clone());
        let id = recent.or(read).unwrap_or(active);
        self.routes.insert(book.into(), id.clone());
        let b = self.sessions[&id].store.list_books()?.into_iter().find(|b| b.book_id == book);
        Ok(json!({"account_id":id,"account_name":self.info(&id)?.name,"last_item_id":b.as_ref().and_then(|b| b.last_item_id.clone()),"chapter_order":b.map(|b| b.chapter_order).unwrap_or(0)}))
    }
    fn verify(&mut self, id: &str) -> Result<(), String> {
        self.require(id)?;
        if self.sessions[id].verified { return Ok(()); }
        let path = self.dir(id).join("cookies.bin");
        let c = self.sessions.get_mut(id).unwrap().client.as_mut().unwrap();
        let result = c.user_info().map_err(|_| "账号验证失败，请检查网络或重新登录".to_owned());
        Vault::new(path).save(&c.jar.to_json())?;
        let (identity,name,avatar) = profile(&result?)?;
        let a = self.manifest.accounts.iter_mut().find(|a| a.id == id).unwrap();
        if !a.identity.is_empty() && a.identity != identity { return Err("账号身份不匹配，请退出后重新添加".into()); }
        a.identity = identity; a.name = name; a.avatar = avatar;
        self.save()?;
        self.sessions.get_mut(id).unwrap().verified = true;
        Ok(())
    }
    pub fn with_session<T>(&mut self, id: &str, f: impl FnOnce(&mut Client, &Store) -> Result<T, String>) -> Result<T, String> {
        self.verify(id)?;
        let path = self.dir(id).join("cookies.bin");
        let s = self.sessions.get_mut(id).unwrap();
        let c = s.client.as_mut().unwrap();
        let result = f(c, &s.store);
        // Flush even failed requests: Set-Cookie may have rotated or deleted a session.
        Vault::new(path).save(&c.jar.to_json())?;
        result
    }
    pub fn mark_recent(&mut self, book: &str, id: &str) -> Result<(), String> { self.manifest.recent.insert(book.into(), id.into()); self.save() }
    pub fn books(&mut self, remote: bool) -> Result<Value, String> {
        let mut warnings = Vec::new();
        if remote {
            let ids: Vec<_> = self.manifest.accounts.iter().filter(|a| a.logged_in).map(|a| a.id.clone()).collect();
            for id in ids {
                let expected = self.info(&id)?.identity.clone();
                let result = self.with_session(&id, |c, store| {
                    let p = profile(&c.user_info().map_err(|_| "账号验证失败".to_owned())?)?;
                    if !expected.is_empty() && p.0 != expected { return Err("账号身份不匹配".into()); }
                    let mut issues = Vec::new();
                    match c.reading_progress() {
                        Ok(progress) => for p in progress { store.merge_progress(&p)?; },
                        Err(_) => issues.push("云进度获取失败或格式不支持，未覆盖本地进度"),
                    }
                    match c.shelf_books() {
                        Ok(books) => for b in books { store.remember_book(&BookRow { book_id:b.book_id,name:b.name,author:b.author,thumb:b.thumb,abstract_text:String::new(),last_item_id:None,chapter_order:0 })?; },
                        Err(_) => issues.push("官方书架同步失败，保留本地书架"),
                    }
                    Ok((p, issues))
                });
                match result {
                    Ok(((identity,name,avatar),issues)) => { for issue in issues { warnings.push(format!("{name}：{issue}")); } let a = self.manifest.accounts.iter_mut().find(|a| a.id == id).unwrap(); a.identity=identity; a.name=name; a.avatar=avatar; }
                    Err(_) => warnings.push(format!("{}：同步失败，保留本地记录，请检查网络或重新登录", self.info(&id)?.name)),
                }
            }
            self.save()?;
        }
        let mut out: BTreeMap<String, Value> = BTreeMap::new();
        for a in &self.manifest.accounts {
            for b in self.sessions[&a.id].store.list_books()? {
                let source = json!({"id":a.id,"name":a.name});
                let mut v = serde_json::to_value(&b).map_err(io)?;
                v["account_id"] = json!(a.id); v["account_name"] = json!(a.name);
                if let Some(old) = out.get_mut(&b.book_id) {
                    let mut sources = old["source_accounts"].as_array().unwrap().clone(); sources.push(source);
                    if self.manifest.recent.get(&b.book_id) == Some(&a.id) || (old["last_item_id"].is_null() && b.last_item_id.is_some()) { *old = v; }
                    old["source_accounts"] = json!(sources);
                } else { v["source_accounts"] = json!([source]); out.insert(b.book_id.clone(), v); }
            }
        }
        Ok(json!({"books":out.into_values().collect::<Vec<_>>(),"warnings":warnings}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    fn temp() -> PathBuf { std::env::temp_dir().join(nonce()) }
    fn book() -> BookRow { BookRow { book_id:"book".into(),name:"test".into(),author:String::new(),thumb:String::new(),abstract_text:String::new(),last_item_id:None,chapter_order:0 } }
    fn fixture(root: &Path, id: &str) {
        let dir=root.join("accounts").join(id); std::fs::create_dir_all(&dir).unwrap();
        Vault::new(dir.join("cookies.bin")).save(&CookieJar::default().to_json()).unwrap();
    }
    #[test]
    fn isolation_routes_logout_and_reopen() {
        let root=temp(); let mut a=Accounts::open(root.clone()).unwrap();
        for id in ["one","two"] { fixture(&root,id); a.manifest.accounts.push(AccountInfo { id:id.into(),name:id.into(),avatar:String::new(),logged_in:true,identity:id.into() }); }
        a.manifest.active_id=Some("one".into()); a.save().unwrap(); drop(a);
        let mut a=Accounts::open(root.clone()).unwrap();
        a.sessions["one"].store.remember_book(&book()).unwrap();
        a.sessions["one"].store.set_progress("book","chapter",9).unwrap();
        a.sessions["one"].store.cache_chapter("chapter","book","title","text").unwrap();
        assert!(a.sessions["two"].store.list_books().unwrap().is_empty());
        assert!(a.sessions["two"].store.get_chapter("chapter").unwrap().is_none());
        assert_eq!(a.route("book",None).unwrap(),"one"); a.switch("two").unwrap();
        assert_eq!(a.route("book",None).unwrap(),"one");
        assert!(a.route("book",Some("../two")).is_err());
        a.mark_recent("book","one").unwrap();
        assert_eq!(a.resolve("book").unwrap()["account_id"],"one");
        a.sessions.get_mut("two").unwrap().verified = true;
        let refresh: Result<(), String> = a.with_session("two", |c,_| { c.jar.ingest_header("https://fanqienovel.com/", "synthetic=rotated; Path=/; Max-Age=3600"); Err("synthetic request failure".into()) });
        assert!(refresh.is_err());
        let persisted = Vault::new(a.dir("two").join("cookies.bin")).load().unwrap().unwrap();
        assert_eq!(CookieJar::from_json(&persisted).cookies.len(),1);
        let merged = a.books(false).unwrap(); assert_eq!(merged["books"].as_array().unwrap().len(),1);
        a.logout("one").unwrap(); assert!(a.route("book",None).is_err());
        assert!(a.route("book",Some("one")).is_err());
        assert_eq!(a.route("book",Some("two")).unwrap(),"two");
        assert_eq!(a.sessions["one"].store.list_books().unwrap().len(),1);
        drop(a); let a=Accounts::open(root.clone()).unwrap(); assert!(a.require("one").is_err()); assert!(a.require("two").is_ok());
        drop(a); std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn migration_is_backed_up_and_idempotent() {
        let root=temp(); std::fs::create_dir_all(&root).unwrap();
        let s=Store::open(root.join("library.sqlite")).unwrap(); s.remember_book(&book()).unwrap(); drop(s);
        Vault::new(root.join("cookies.bin")).save(&CookieJar::default().to_json()).unwrap();
        let mut a=Accounts::open(root.clone()).unwrap();
        assert!(root.join("backup-before-accounts/cookies.bin").exists());
        assert!(root.join("backup-before-accounts/library.sqlite").exists());
        assert_eq!(a.sessions["legacy-v1"].store.list_books().unwrap().len(),1);
        a.logout("legacy-v1").unwrap();
        assert!(!root.join("backup-before-accounts/cookies.bin").exists());
        assert!(root.join("backup-before-accounts/library.sqlite").exists());
        assert!(a.resolve("book").is_err());
        drop(a);
        let a=Accounts::open(root.clone()).unwrap(); assert_eq!(a.manifest.accounts.len(),1); assert!(a.require("legacy-v1").is_err());
        drop(a); std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn account_ipc_is_local_only() {
        let local: Value = serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        let signer: Value = serde_json::from_str(include_str!("../capabilities/signer.json")).unwrap();
        assert_eq!(local["windows"], json!(["main"]));
        assert!(local.get("remote").is_none());
        for cmd in ["allow-account-list","allow-account-switch","allow-account-logout","allow-start-login","allow-finish-login","allow-cancel-login","allow-sync-accounts","allow-resolve-reading-account"] {
            assert!(local["permissions"].as_array().unwrap().contains(&json!(cmd)));
            assert!(!signer["permissions"].as_array().unwrap().contains(&json!(cmd)));
        }
        assert!(!signer.to_string().contains("account-login"));
    }
    #[test]
    fn identity_and_public_shape() {
        assert!(profile(&json!({"name":"someone","user_id":"1"})).is_err());
        assert!(profile(&json!({"name":"someone","id":"0"})).is_err());
        assert!(profile(&json!({"name":"someone","id":"../1"})).is_err());
        assert!(profile(&json!({"name":"someone","id":"123"})).is_ok());
        let info=AccountInfo{id:"local".into(),name:"name".into(),avatar:String::new(),logged_in:true,identity:"private".into()};
        assert!(serde_json::to_value(info).unwrap().get("identity").is_none());
    }
}
