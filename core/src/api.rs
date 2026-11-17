use reqwest::blocking::{Client as HttpClient, Response};
use serde::Deserialize;
use serde_json::Value;

use crate::font::FontDecoder;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const BASE: &str = "https://fanqienovel.com";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub expires: Option<i64>,
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
pub struct CookieJar {
    pub cookies: Vec<Cookie>,
}

impl CookieJar {
    fn domain_matches(cookie_domain: &str, host: &str) -> bool {
        let cd = cookie_domain.trim_start_matches('.');
        host == cd || host.ends_with(&format!(".{cd}"))
    }

    pub fn header_for(&self, host: &str) -> Option<String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let parts: Vec<String> = self
            .cookies
            .iter()
            .filter(|c| Self::domain_matches(&c.domain, host))
            .filter(|c| c.expires.map(|e| e > now).unwrap_or(true))
            .map(|c| format!("{}={}", c.name, c.value))
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("; "))
        }
    }

    pub fn ingest(&mut self, url: &str, resp: &Response) {
        let host = url.split('/').nth(2).unwrap_or("");
        for (k, v) in resp.headers().iter() {
            if k.as_str() != "set-cookie" {
                continue;
            }
            let Ok(sv) = v.to_str() else { continue };
            let mut it = sv.split(';');
            let Some(pair) = it.next() else { continue };
            let Some((name, value)) = pair.split_once('=') else { continue };
            let mut c = Cookie {
                name: name.trim().to_string(),
                value: value.trim().to_string(),
                domain: host.to_string(),
                path: "/".to_string(),
                expires: None,
            };
            for attr in it {
                let attr = attr.trim();
                if let Some(d) = attr.strip_prefix("Domain=") {
                    c.domain = d.trim().to_string();
                } else if let Some(p) = attr.strip_prefix("Path=") {
                    c.path = p.trim().to_string();
                } else if let Some(ma) = attr.strip_prefix("Max-Age=") {
                    let secs: i64 = ma.trim().parse().unwrap_or(0);
                    c.expires = Some(std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0)
                        + secs);
                }
            }
            self.cookies.retain(|x| !(x.name == c.name && x.domain == c.domain));
            self.cookies.push(c);
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }
}

#[derive(Debug, Deserialize)]
struct Envelope {
    code: i64,
    #[serde(default)]
    message: String,
    #[serde(default)]
    data: Value,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Chapter {
    pub item_id: String,
    pub book_id: String,
    pub title: String,
    pub text: String,
    pub need_pay: bool,
    pub is_lock: bool,
    pub order: i64,
    pub next_item_id: Option<String>,
    pub pre_item_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchBook {
    pub book_id: String,
    pub name: String,
    pub author: String,
    pub abstract_text: String,
    pub category: String,
    pub thumb: String,
    pub word_count: i64,
    pub read_count: i64,
    pub last_chapter_title: String,
    pub creation_status: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct BookDetail {
    pub book_id: String,
    pub name: String,
    pub author: String,
    pub abstract_text: String,
    pub category: String,
    pub thumb: String,
    pub word_count: i64,
    pub read_count: i64,
    pub last_chapter_title: String,
    pub creation_status: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChapterMeta {
    pub item_id: String,
    pub title: String,
    pub volume: String,
    pub order: i64,
    pub locked: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ShelfBook {
    pub book_id: String,
    pub name: String,
    pub author: String,
    pub thumb: String,
    pub last_item_id: Option<String>,
}

pub struct Client {
    http: HttpClient,
    pub jar: CookieJar,
}

impl Client {
    pub fn new() -> Self {
        Self {
            http: HttpClient::builder()
                .user_agent(UA)
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .expect("client"),
            jar: CookieJar::default(),
        }
    }

    pub fn restore_cookies(&mut self, json: &str) {
        self.jar = CookieJar::from_json(json);
    }

    fn get(&self, url: &str) -> Result<Response, String> {
        let mut req = self.http.get(url).header("Referer", format!("{BASE}/"));
        if let Some(host) = url.split('/').nth(2) {
            if let Some(c) = self.jar.header_for(host) {
                req = req.header("Cookie", c);
            }
        }
        req.send().map_err(|e| format!("GET {url}: {e}"))
    }

    fn request_json(&mut self, url: &str) -> Result<Value, String> {
        let resp = self.get(url)?;
        let final_url = resp.url().to_string();
        let status = resp.status();
        self.jar.ingest(&final_url, &resp);
        let body = resp.text().map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("HTTP {status}: {}", &body[..body.len().min(120)]));
        }
        let env: Envelope = serde_json::from_str(&body).map_err(|e| format!("json {e}: {}", &body[..body.len().min(120)]))?;
        if env.code != 0 {
            return Err(format!("code {} msg {}", env.code, env.message));
        }
        Ok(env.data)
    }

    fn request_html(&mut self, url: &str) -> Result<String, String> {
        let resp = self.get(url)?;
        let final_url = resp.url().to_string();
        let status = resp.status();
        self.jar.ingest(&final_url, &resp);
        let body = resp.text().map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("HTTP {status}"));
        }
        Ok(body)
    }

    fn request_bytes(&self, url: &str) -> Result<Vec<u8>, String> {
        let resp = self.get(url).map_err(|e| e.to_string())?;
        resp.bytes().map(|b| b.to_vec()).map_err(|e| e.to_string())
    }

    pub fn user_info(&mut self) -> Result<Value, String> {
        self.request_json(&format!("{BASE}/api/user/info"))
    }

    pub fn book_info(&mut self, book_id: &str) -> Result<Value, String> {
        self.request_json(&format!("{BASE}/api/book/info?bookId={book_id}"))
    }

    pub fn directory(&mut self, book_id: &str) -> Result<Vec<String>, String> {
        let d = self.request_json(&format!("{BASE}/api/reader/directory/detail?bookId={book_id}"))?;
        d.get("allItemIds")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .ok_or_else(|| "no allItemIds".to_string())
    }

    pub fn search(&mut self, query: &str, page_index: u32, page_count: u32) -> Result<Vec<Value>, String> {
        let q = urlencode(query);
        let d = self.request_json(&format!(
            "{BASE}/api/author/search/search_book/v1?filter=127%2C0%2C0%2C0&page_count={page_count}&page_index={page_index}&query_type=0&query_word={q}"
        ))?;
        Ok(d.get("search_book_data_list")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default())
    }

    pub fn search_url(query: &str, page_index: u32, page_count: u32) -> String {
        format!(
            "/api/author/search/search_book/v1?filter=127%2C0%2C0%2C0&page_count={page_count}&page_index={page_index}&query_type=0&query_word={}",
            urlencode(query)
        )
    }

    fn font_by_hash(&self, hash: &str) -> Result<Vec<u8>, String> {
        let url = format!("https://lf6-awef.bytetos.com/obj/awesome-font/c/{hash}.woff");
        self.request_bytes(&url)
            .or_else(|_| self.request_bytes(&format!("https://lf3-awef.bytetos.com/obj/awesome-font/c/{hash}.woff")))
    }

    /// Build a decoder for the response-specific obfuscation font. `font_hash`
    /// comes from the `x-tt-zhal` response header (f=...). The static table
    /// follows glyph names; fanqie's per-response fonts keep identical outlines
    /// but reshuffle which outline a name/PUA points at, so fall back to
    /// matching outlines against a reader-page font whose names we trust.
    fn response_decoder(&mut self, font_hash: &str, reference_item: Option<&str>) -> Option<FontDecoder> {
        let active = self.font_by_hash(font_hash).ok()?;
        if let Ok(d) = FontDecoder::from_font_bytes(&active) {
            if d.mapped_count() >= 360 {
                return Some(d);
            }
        }
        let item = reference_item?;
        let html = self.fetch_reader_html(item).ok()?;
        let url = crate::font::extract_font_url(&html)?;
        let reference = self.request_bytes(&url).ok()?;
        FontDecoder::from_font_pair(&active, &reference).ok()
    }

    pub fn search_from_body(&mut self, body: &str, font_hash: Option<&str>) -> Result<Vec<SearchBook>, String> {
        let mut val: Value = serde_json::from_str(body).map_err(|e| format!("json {e}"))?;
        let code = val.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
        if code != 0 {
            return Err(format!("code {code}"));
        }
        let first_item = val
            .pointer("/data/search_book_data_list/0/first_chapter_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if let Some(hash) = font_hash {
            if let Some(dec) = self.response_decoder(hash, first_item.as_deref()) {
                decode_value(&mut val, &dec);
            }
        }
        let list = val
            .pointer("/data/search_book_data_list")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(list.iter().map(map_search_book).collect())
    }

    pub fn book_detail(&mut self, book_id: &str) -> Result<BookDetail, String> {
        let d = self.request_json(&format!("{BASE}/api/book/info?bookId={book_id}"))?;
        let s = |k: &str| d.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let n = |k: &str| {
            d.get(k)
                .and_then(|v| {
                    v.as_i64()
                        .or_else(|| v.as_str().and_then(|x| x.trim().parse().ok()))
                })
                .unwrap_or(0)
        };
        let category = d
            .get("categoryV2")
            .map(category_names)
            .filter(|v| !v.is_empty())
            .or_else(|| d.get("category").map(category_names).filter(|v| !v.is_empty()))
            .unwrap_or_default();
        Ok(BookDetail {
            book_id: s("bookId"),
            name: s("bookName"),
            author: s("author"),
            abstract_text: s("abstract"),
            category,
            thumb: s("thumbUrl"),
            word_count: n("wordNumber"),
            read_count: n("readCount"),
            last_chapter_title: s("lastChapterTitle"),
            creation_status: d
                .get("creationStatus")
                .and_then(|v| v.as_str().and_then(|x| x.parse().ok()).or_else(|| v.as_i64()))
                .unwrap_or(0),
        })
    }

    pub fn directory_meta(&mut self, book_id: &str) -> Result<Vec<ChapterMeta>, String> {
        let d = self.request_json(&format!("{BASE}/api/reader/directory/detail?bookId={book_id}"))?;
        let volumes = d
            .get("chapterListWithVolume")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let volume_names: Vec<String> = d
            .get("volumeNameList")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let mut out = Vec::new();
        for (vi, vol) in volumes.iter().enumerate() {
            let vname = volume_names.get(vi).cloned().unwrap_or_default();
            let Some(chapters) = vol.as_array() else { continue };
            for ch in chapters {
                let item_id = ch.get("itemId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if item_id.is_empty() {
                    continue;
                }
                out.push(ChapterMeta {
                    item_id,
                    title: ch.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    volume: vname.clone(),
                    order: ch
                        .get("realChapterOrder")
                        .and_then(|v| v.as_str().and_then(|x| x.parse().ok()).or_else(|| v.as_i64()))
                        .unwrap_or(0),
                    locked: ch.get("isChapterLock").and_then(|v| v.as_bool()).unwrap_or(false)
                        || ch.get("needPay").and_then(|v| v.as_i64()).unwrap_or(0) != 0,
                });
            }
        }
        Ok(out)
    }

    pub fn shelf_books(&mut self) -> Result<Vec<ShelfBook>, String> {
        let d = self.request_json(&format!(
            "{BASE}/reading/bookapi/bookshelf/info/v1/?device_platform=android&aid=2329"
        ))?;
        let list = d.get("book_list").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        Ok(list.iter().filter_map(map_shelf_book).collect())
    }

    pub fn bookshelf(&mut self) -> Result<Vec<Value>, String> {
        let d = self.request_json(&format!("{BASE}/reading/bookapi/bookshelf/info/v1/?device_platform=android&aid=2329"))?;
        Ok(d.get("book_list").and_then(|v| v.as_array()).cloned().unwrap_or_default())
    }

    pub fn progress(&mut self) -> Result<Value, String> {
        self.request_json(&format!("{BASE}/api/reader/book/progress"))
    }

    pub fn fetch_reader_html(&mut self, item_id: &str) -> Result<String, String> {
        self.request_html(&format!("{BASE}/reader/{item_id}"))
    }

    pub fn fetch_bytes_pub(&self, url: &str) -> Result<Vec<u8>, String> {
        self.request_bytes(url)
    }

    /// Fetch + font-decode one chapter via the reader page route.
    pub fn chapter(&mut self, item_id: &str) -> Result<Chapter, String> {
        let html = self.request_html(&format!("{BASE}/reader/{item_id}"))?;
        let state = extract_initial_state(&html).ok_or("INITIAL_STATE not found")?;
        let cd = state.pointer("/reader/chapterData").cloned().unwrap_or(Value::Null);
        if cd.is_null() {
            return Err("no chapterData".into());
        }
        let raw_content = cd.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let font_url = crate::font::extract_font_url(&html).ok_or("no @font-face url")?;
        let font_bytes = self.request_bytes(&font_url)?;
        let decoder = FontDecoder::from_font_bytes(&font_bytes)?;
        let text = decoder.decode_html_to_plain(&raw_content);
        let num = |k: &str| cd.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        let str_opt = |k: &str| cd.get(k).and_then(|v| v.as_str()).map(str::to_string);
        Ok(Chapter {
            item_id: item_id.to_string(),
            book_id: cd.get("bookId").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            title: cd.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            text,
            need_pay: num("needPay") != 0,
            is_lock: num("isChapterLock") != 0,
            order: num("realChapterOrder"),
            next_item_id: str_opt("nextItemId"),
            pre_item_id: str_opt("preItemId"),
        })
    }
}

fn extract_initial_state(html: &str) -> Option<Value> {
    let i = html.find("window.__INITIAL_STATE__=")?;
    let start = i + "window.__INITIAL_STATE__=".len();
    let bytes = html.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut end = None;
    for (off, &b) in bytes[start..].iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + off + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    serde_json::from_str(&js_literals_to_json(&html[start..end?])).ok()
}

/// The SSR payload sometimes embeds bare JS `undefined` literals, which are
/// not valid JSON. Replace them with null outside of strings.
fn js_literals_to_json(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut in_str = false;
    let mut esc = false;
    let mut rest = src;
    while let Some(ch) = rest.chars().next() {
        if in_str {
            out.push(ch);
            if esc {
                esc = false;
            } else if ch == '\\' {
                esc = true;
            } else if ch == '"' {
                in_str = false;
            }
            rest = &rest[ch.len_utf8()..];
            continue;
        }
        if ch == '"' {
            in_str = true;
            out.push(ch);
            rest = &rest[1..];
            continue;
        }
        if rest.starts_with("undefined") {
            let after = rest[9..].chars().next();
            let boundary = after.map(|c| !c.is_alphanumeric() && c != '_').unwrap_or(true);
            if boundary {
                out.push_str("null");
                rest = &rest[9..];
                continue;
            }
        }
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

pub fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn decode_value(v: &mut Value, dec: &FontDecoder) {
    match v {
        Value::String(s) => *s = dec.decode(s),
        Value::Array(a) => a.iter_mut().for_each(|x| decode_value(x, dec)),
        Value::Object(o) => o.values_mut().for_each(|x| decode_value(x, dec)),
        _ => {}
    }
}

fn num_field(v: &Value, keys: &[&str]) -> i64 {
    for k in keys {
        if let Some(x) = v.get(*k) {
            if let Some(n) = x.as_i64() {
                return n;
            }
            if let Some(n) = x.as_str().and_then(|s| s.parse().ok()) {
                return n;
            }
        }
    }
    0
}

fn str_field(v: &Value, keys: &[&str]) -> String {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
            return s.to_string();
        }
    }
    String::new()
}

/// `categoryV2` can be a JSON-encoded array of `{name}` objects (sometimes a
/// string of that JSON); normalize whatever shape to comma-joined names.
fn category_names(v: &Value) -> String {
    match v {
        Value::String(s) => {
            let t = s.trim();
            if t.starts_with('[') || t.starts_with('{') {
                serde_json::from_str::<Value>(t)
                    .map(|j| category_names(&j))
                    .unwrap_or_default()
            } else if t.is_empty() {
                String::new()
            } else {
                t.to_string()
            }
        }
        Value::Array(a) => a
            .iter()
            .map(category_names)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(o) => ["name", "Name"]
            .iter()
            .find_map(|k| o.get(*k))
            .map(category_names)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn map_search_book(v: &Value) -> SearchBook {
    SearchBook {
        book_id: str_field(v, &["book_id", "bookId"]),
        name: str_field(v, &["book_name", "bookName"]),
        author: str_field(v, &["author", "author_name", "authorName"]),
        abstract_text: str_field(v, &["book_abstract", "abstract"]),
        category: str_field(v, &["category", "categoryV2"]),
        thumb: str_field(v, &["thumb_url", "thumbUrl"]),
        word_count: num_field(v, &["word_count", "wordNumber"]),
        read_count: num_field(v, &["read_count", "readCount"]),
        last_chapter_title: str_field(v, &["last_chapter_title", "lastChapterTitle"]),
        creation_status: num_field(v, &["creation_status", "creationStatus"]),
    }
}

fn map_shelf_book(v: &Value) -> Option<ShelfBook> {
    let inner = v
        .get("bookInfo")
        .or_else(|| v.get("book_info"))
        .or_else(|| v.get("book_data").and_then(|x| x.get(0)))
        .unwrap_or(v);
    let pick = |keys: &[&str]| str_field(inner, keys);
    let book_id = {
        let id = pick(&["bookId", "book_id"]);
        if id.is_empty() {
            return None;
        }
        id
    };
    Some(ShelfBook {
        book_id,
        name: pick(&["bookName", "book_name"]),
        author: pick(&["author", "authorName"]),
        thumb: pick(&["thumbUrl", "thumb_url"]),
        last_item_id: inner
            .get("lastItemId")
            .or_else(|| inner.get("last_item_id"))
            .and_then(|x| x.as_str())
            .map(str::to_string),
    })
}
