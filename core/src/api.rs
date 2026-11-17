use reqwest::blocking::{Client as HttpClient, Response};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

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
    let mut end = start;
    for (off, &b) in bytes[start..].iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + off + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    serde_json::from_str(&html[start..end]).ok()
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
