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
        let cd = cookie_domain.trim_start_matches('.').to_ascii_lowercase();
        let host = host.to_ascii_lowercase();
        !cd.is_empty() && (host == cd || host.ends_with(&format!(".{cd}")))
    }

    pub fn header_for(&self, host: &str) -> Option<String> {
        self.header_for_path(host, "/")
    }

    pub fn header_for_path(&self, host: &str, path: &str) -> Option<String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let parts: Vec<String> = self
            .cookies
            .iter()
            .filter(|c| c.domain.trim_start_matches('.').eq_ignore_ascii_case("fanqienovel.com") && Self::domain_matches(&c.domain, host))
            .filter(|c| path == c.path || (path.starts_with(&c.path) && (c.path.ends_with('/') || path.as_bytes().get(c.path.len()) == Some(&b'/'))))
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
        for v in resp.headers().get_all("set-cookie") {
            if let Ok(s) = v.to_str() { self.ingest_header(url, s); }
        }
    }

    pub fn ingest_header(&mut self, url: &str, header: &str) {
        let Ok(url) = reqwest::Url::parse(url) else { return };
        let Some(host) = url.host_str() else { return };
        if url.scheme() != "https" { return; }
        let Ok(parsed) = cookie::Cookie::parse(header) else { return };
        let domain = parsed.domain().unwrap_or(host).trim_start_matches('.').to_ascii_lowercase();
        // This client only authenticates against the official root domain.
        if domain != "fanqienovel.com" || !Self::domain_matches(&domain, host) { return; }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        let default_path = url.path().rsplit_once('/').map(|(p,_)| if p.is_empty() { "/" } else { p }).unwrap_or("/");
        let path = parsed.path().filter(|p| p.starts_with('/')).unwrap_or(default_path).to_owned();
        let expires = parsed.max_age().map(|a| now.saturating_add(a.whole_seconds())).or_else(|| parsed.expires_datetime().map(|e| e.unix_timestamp()));
        let c = Cookie { name:parsed.name().to_owned(),value:parsed.value().to_owned(),domain,path,expires };
        self.cookies.retain(|x| !(x.name == c.name && x.domain.trim_start_matches('.').eq_ignore_ascii_case(&c.domain) && x.path == c.path));
        if !expires.is_some_and(|e| e <= now) { self.cookies.push(c); }
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
    pub word_count: i64,
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

#[derive(Debug, Clone, serde::Serialize)]
pub struct HomeRail {
    pub title: String,
    pub books: Vec<SearchBook>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PostBook {
    pub book_id: String,
    pub name: String,
    pub author: String,
    pub cover: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PostCard {
    pub book_id: String,
    pub id: String,
    pub user: String,
    pub avatar: String,
    pub time: String,
    pub text: String,
    pub books: Vec<PostBook>,
}

#[cfg(test)]
mod account_tests {
    use super::*;
    #[test]
    fn cookie_domain_path_expiry_and_rotation() {
        let mut jar = CookieJar::default();
        let url = "https://fanqienovel.com/api/user/info/v2";
        jar.ingest_header(url, "test=one; domain=.fanqienovel.com; path=/api; Max-Age=3600");
        assert_eq!(jar.header_for_path("fanqienovel.com", "/api/user/info"), Some("test=one".into()));
        assert!(jar.header_for_path("evilfanqienovel.com", "/api").is_none());
        assert!(jar.header_for_path("fanqienovel.com.evil.test", "/api").is_none());
        assert!(jar.header_for_path("fanqienovel.com", "/apievil").is_none());
        jar.ingest_header(url, "test=two; DOMAIN=fanqienovel.com; PATH=/api; Max-Age=3600");
        assert_eq!(jar.cookies.len(), 1);
        assert_eq!(jar.cookies[0].value, "two");
        jar.ingest_header(url, "test=deleted; Domain=fanqienovel.com; Path=/api; Max-Age=0");
        assert!(jar.cookies.is_empty());
        jar.ingest_header(url, "bad=value; Domain=com; Path=/");
        jar.ingest_header(url, "expired=value; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Path=/");
        assert!(jar.cookies.is_empty());
    }
    #[test]
    fn strict_cloud_progress_shape() {
        let p = parse_reading_progress(&serde_json::json!([{"book_id":"123","item_id":"456","index":3,"read_timestamp":"1700000000"}])).unwrap();
        assert_eq!(p[0].chapter_order,3);
        assert!(parse_reading_progress(&serde_json::json!({"books":[]})).is_err());
        assert!(parse_reading_progress(&serde_json::json!([{"book_id":"123","item_id":"456","read_progress":3}])).is_err());
        assert!(parse_reading_progress(&serde_json::json!([])).unwrap().is_empty());
    }
}

#[derive(Debug)]
pub struct ReadingProgress {
    pub book_id: String,
    pub item_id: String,
    pub chapter_order: i64,
    pub timestamp: i64,
}

/// Official muye_faff5f61.js fetchReadProgress consumes data[] keyed by book_id;
/// syncBookProgress writes item_id, index and read_timestamp (epoch seconds).
/// Reject unknown shapes, rather than interpreting them as an empty full history.
pub fn parse_reading_progress(value: &Value) -> Result<Vec<ReadingProgress>, String> {
    fn identifier(v: &Value) -> Option<String> {
        v.as_str().filter(|s| !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_digit())).map(str::to_owned)
    }
    fn integer(v: &Value) -> Option<i64> { v.as_i64().or_else(|| v.as_str()?.parse().ok()) }
    value.as_array().ok_or("云进度结构不支持")?.iter().map(|v| {
        Ok(ReadingProgress {
            book_id: identifier(&v["book_id"]).ok_or("云进度图书标识无效")?,
            item_id: identifier(&v["item_id"]).ok_or("云进度章节标识无效")?,
            chapter_order: integer(&v["index"]).filter(|n| *n >= 0).ok_or("云进度章节序号无效")?,
            timestamp: integer(&v["read_timestamp"]).filter(|n| *n > 0 && *n <= std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64 + 300).ok_or("云进度时间无效")?,
        })
    }).collect()
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
        if let Ok(parsed) = reqwest::Url::parse(url) {
            if parsed.scheme() == "https" {
                if let Some(c) = parsed.host_str().and_then(|h| self.jar.header_for_path(h, parsed.path())) {
                    req = req.header("Cookie", c);
                }
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
            return Err(format!("HTTP {status}"));
        }
        let env: Envelope = serde_json::from_str(&body).map_err(|_| "服务器响应格式无效".to_owned())?;
        if env.code != 0 {
            return Err(format!("服务器返回错误码 {}", env.code));
        }
        check_base_resp(&env.data)?;
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
        self.request_json(&format!("{BASE}/api/user/info/v2"))
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

    pub fn reading_progress(&mut self) -> Result<Vec<ReadingProgress>, String> {
        parse_reading_progress(&self.progress()?)
    }

    pub fn fetch_reader_html(&mut self, item_id: &str) -> Result<String, String> {
        self.request_html(&format!("{BASE}/reader/{item_id}"))
    }

    pub fn fetch_bytes_pub(&self, url: &str) -> Result<Vec<u8>, String> {
        self.request_bytes(url)
    }

    /// Curated rails from the SSR homepage state (editor picks, weekly hot,
    /// male/female picks). All plain text — no font obfuscation.
    pub fn home_rails(&mut self) -> Result<Vec<HomeRail>, String> {
        let html = self.request_html(&format!("{BASE}/"))?;
        let state = extract_initial_state(&html).ok_or("INITIAL_STATE not found")?;
        let home = state.get("home").cloned().unwrap_or(Value::Null);
        let mut out = Vec::new();
        for (title, key) in [
            ("编辑推荐", "editorList"),
            ("本周热门", "weekList"),
            ("男频精选", "boyList"),
            ("女频精选", "girlList"),
        ] {
            let list = home
                .get(key)
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let books: Vec<SearchBook> = list
                .iter()
                .map(map_home_book)
                .filter(|b| !b.book_id.is_empty() && !b.name.is_empty())
                .collect();
            if !books.is_empty() {
                out.push(HomeRail { title: title.to_string(), books });
            }
        }
        Ok(out)
    }

    /// Reader "推书帖" from the SEO keyword pages (book-recommendation posts
    /// with author-picked book lists). Each page is plain SSR HTML.
    pub fn community_posts(&mut self) -> Result<Vec<PostCard>, String> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for seed in POST_SEEDS {
            if out.len() >= 18 {
                break;
            }
            let Ok(html) = self.request_html(&format!("{BASE}/keyword/{seed}")) else {
                continue;
            };
            for p in parse_posts(&html) {
                if seen.insert(p.id.clone()) {
                    out.push(p);
                }
            }
        }
        Ok(out)
    }

    pub fn post_detail(&mut self, book_id: &str, post_id: &str) -> Result<PostDetail, String> {
        if !valid_id(book_id) || !valid_id(post_id) { return Err("帖子标识无效".into()); }
        let data = self.request_json(&format!("{BASE}/api/comment/get_book_comment?bookId={book_id}&commentId={post_id}"))?;
        parse_post_detail(&data, post_id)
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
            word_count: chapter_word_count(&cd),
            next_item_id: str_opt("nextItemId"),
            pre_item_id: str_opt("preItemId"),
        })
    }
}

fn chapter_word_count(cd: &Value) -> i64 {
    match cd.get("chapterWordNumber") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn extract_initial_state(html: &str) -> Option<Value> {
    extract_state(html, "window.__INITIAL_STATE__")
}

fn extract_state(html: &str, marker: &str) -> Option<Value> {
    let i = html.find(marker)? + marker.len();
    let assignment = html[i..].trim_start().strip_prefix('=')?.trim_start();
    let start = html.len() - assignment.len();
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

/// SEO keyword pages that carry user "推书帖" (book-recommendation posts).
const POST_SEEDS: &[&str] = &[
    "7322677137683531811",
    "7345198789168318499",
    "7357073288092370996",
    "7322677137683531812",
];

fn map_home_book(v: &Value) -> SearchBook {
    SearchBook {
        book_id: str_field(v, &["bookId", "book_id"]),
        name: str_field(v, &["bookName", "book_name"]),
        author: str_field(v, &["author", "authorName"]),
        abstract_text: str_field(v, &["abstract", "book_abstract"]),
        category: str_field(v, &["category"]),
        thumb: str_field(v, &["thumbUri", "thumb_url", "thumbUrl"]),
        word_count: num_field(v, &["wordNumber", "word_count"]),
        read_count: num_field(v, &["readCount", "read_count"]),
        last_chapter_title: str_field(v, &["lastChapterTitle"]),
        creation_status: num_field(v, &["creationStatus", "creation_status"]),
    }
}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_digit())
}

fn map_post_book(v: &Value) -> PostBook {
    let url = str_field(v, &["book_detail_page_url"]);
    let book_id = reqwest::Url::parse(&url).ok().filter(|u| u.host_str() == Some("fanqienovel.com"))
        .and_then(|u| u.path().strip_prefix("/page/").filter(|s| valid_id(s)).map(str::to_owned))
        .unwrap_or_else(|| str_field(v, &["book_id"]));
    PostBook { book_id, name: str_field(v, &["title"]), author: str_field(v, &["author_name"]), cover: str_field(v, &["cover_url"]).replace("http://", "https://") }
}

fn parse_posts(html: &str) -> Vec<PostCard> {
    let Some(state) = extract_state(html, "window._SSR_DATA") else { return vec![] };
    let Some(loaders) = state.pointer("/data/loadersData").and_then(Value::as_object) else { return vec![] };
    let mut out = Vec::new();
    for loader in loaders.values() {
        let Some(posts) = loader.pointer("/data/fanqie_novel_comment_list").and_then(Value::as_array) else { continue };
        for p in posts {
            let id = str_field(p, &["id"]);
            if !valid_id(&id) { continue; }
            let books: Vec<_> = p["recommend_novel_list"].as_array().map(|a| a.iter().map(map_post_book).collect()).unwrap_or_default();
            let book_id = books.iter().find(|b| valid_id(&b.book_id)).map(|b| b.book_id.clone()).unwrap_or_default();
            out.push(PostCard { id, book_id, user: str_field(p, &["nickname"]), avatar: str_field(p, &["avatar_url"]), time: String::new(), text: str_field(p, &["comment"]), books });
        }
    }
    out
}

#[derive(Debug, serde::Serialize)]
pub struct PostReply { pub user: String, pub text: String }

#[derive(Debug, serde::Serialize)]
pub struct PostDetail {
    pub id: String,
    pub text: String,
    pub replies: Vec<PostReply>,
    pub total: u64,
    pub complete: bool,
    pub notice: String,
}

fn check_base_resp(data: &Value) -> Result<(), String> {
    if let Some(base) = data.get("base_resp") {
        if base["code"].as_i64() != Some(0) { return Err("评论接口返回错误".into()); }
    }
    Ok(())
}

// The service serializes 64-bit IDs as rounded JS numbers. SSR string IDs
// remain authoritative; numeric ownership is only comparable at f64 precision.
fn same_comment_id(value: &Value, id: &str) -> bool {
    if let Some(s) = value.as_str() { return s == id; }
    value.as_f64().zip(id.parse::<f64>().ok()).is_some_and(|(a,b)| a == b)
}

fn parse_post_detail(data: &Value, id: &str) -> Result<PostDetail, String> {
    check_base_resp(data)?;
    let comments = data["comments"].as_array().ok_or("帖子响应格式无效")?;
    let post = comments.iter().find(|p| same_comment_id(&p["info"]["comment_id"], id)).ok_or("帖子不存在或标识不匹配")?;
    let text = post["info"]["text"].as_str().ok_or("帖子正文格式无效")?.to_owned();
    let total = post["info"]["reply_count"].as_u64().ok_or("评论总数格式无效")?;
    let raw = data["replies"].as_array().ok_or("评论列表格式无效")?;
    let mut replies = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for r in raw {
        if !same_comment_id(&r["reply_to_comment_id"], id) { continue; }
        let Some(text) = r["info"]["text"].as_str() else { continue };
        let key = r["info"]["comment_id"].to_string();
        if key == "null" || !seen.insert(key) { continue; }
        replies.push(PostReply { user: str_field(&r["user"], &["nick_name"]), text: text.to_owned() });
    }
    let complete = replies.len() as u64 == total && replies.len() == raw.len();
    let notice = if complete { String::new() } else { "只展示接口可取得评论，未返回或无法核实归属的评论未展示".into() };
    Ok(PostDetail { id: id.into(), text, replies, total, complete, notice })
}

#[cfg(test)]
mod post_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn ssr_community_only_exact_ids() {
        let state = json!({"data":{"loadersData":{"route":{"data":{
            "fanqie_novel_comment_list":[{"id":"7481136815622210329","nickname":"原作者","comment":"<script>正文</script>","recommend_novel_list":[{"title":"书","book_id":7476430484457672000u64,"book_detail_page_url":"https://fanqienovel.com/page/7476430484457671704?source=seo"}]}],
            "novel_book_comment_list":[{"id":"6818309885236248589","comment":"不是社区"}]
        }}}}});
        let p = parse_posts(&format!("<script>window._SSR_DATA = {state};</script>"));
        assert_eq!(p.len(),1);
        assert_eq!(p[0].id,"7481136815622210329");
        assert_eq!(p[0].book_id,"7476430484457671704");
        assert_eq!(p[0].text,"<script>正文</script>");
    }
    #[test]
    fn detail_empty_partial_ownership_and_errors() {
        let id = "7481136815622210329";
        let mut d = json!({"base_resp":{"code":0},"comments":[{"info":{"comment_id":7481136815622211000u64,"text":"完整正文","reply_count":0}}],"replies":[]});
        assert!(parse_post_detail(&d,id).unwrap().complete);
        d["comments"][0]["info"]["reply_count"] = json!(2);
        d["replies"] = json!([{"info":{"comment_id":"1","text":"测试回复"},"user":{"nick_name":"测试用户"},"reply_to_comment_id":7481136815622211000u64},{"info":{"comment_id":"2","text":"别的帖子"},"reply_to_comment_id":"6818309885236248589"}]);
        let p = parse_post_detail(&d,id).unwrap();
        assert_eq!(p.replies.len(),1);
        assert!(!p.complete);
        assert!(!p.notice.is_empty());
        assert!(parse_post_detail(&d,"1").is_err());
        d["base_resp"]["code"] = json!(5);
        assert!(parse_post_detail(&d,id).is_err());
        assert!(!same_comment_id(&json!("7481136815622211000"),id));
    }
}

fn between(s: &str, open: &str, close: &str) -> Option<String> {
    let i = s.find(open)? + open.len();
    let j = s[i..].find(close)? + i;
    Some(s[i..j].to_string())
}

/// Inner text of the first tag starting with `open`, skipping its attributes.
fn tagged_text(s: &str, open: &str, close: &str) -> Option<String> {
    let i = s.find(open)?;
    let tag_end = s[i..].find('>')? + i + 1;
    let j = s[tag_end..].find(close)? + tag_end;
    Some(s[tag_end..j].to_string())
}

/// src of the first <img> whose tag contains `needle`.
fn img_src(s: &str, needle: &str) -> String {
    let mut idx = 0;
    while let Some(pos) = s[idx..].find("<img ") {
        let start = idx + pos;
        let end = s[start..].find('>').map(|p| start + p + 1).unwrap_or(s.len());
        let tag = &s[start..end];
        if tag.contains(needle) {
            if let Some(v) = between(tag, "src=\"", "\"") {
                return html_unescape(&v).replace("http://", "https://");
            }
        }
        idx = end;
    }
    String::new()
}

/// Strip HTML comments/tags from a small fragment and unescape entities.
fn html_text(s: Option<String>) -> String {
    let raw = s.unwrap_or_default();
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw.as_str();
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        match rest[i..].find('>') {
            Some(j) => rest = &rest[i + j + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    html_unescape(out.trim())
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
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
