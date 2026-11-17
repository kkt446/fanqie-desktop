use rusqlite::{params, Connection};

#[derive(Debug, serde::Serialize)]
pub struct BookRow {
    pub book_id: String,
    pub name: String,
    pub author: String,
    pub thumb: String,
    pub abstract_text: String,
    pub last_item_id: Option<String>,
    pub chapter_order: i64,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS books(
                book_id TEXT PRIMARY KEY, name TEXT NOT NULL, author TEXT NOT NULL DEFAULT '',
                thumb TEXT NOT NULL DEFAULT '', abstract_text TEXT NOT NULL DEFAULT '',
                last_item_id TEXT, chapter_order INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS progress_clock(book_id TEXT PRIMARY KEY, timestamp INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS chapters(
                item_id TEXT PRIMARY KEY, book_id TEXT NOT NULL, title TEXT NOT NULL,
                text TEXT NOT NULL, fetched_at INTEGER NOT NULL
            );
            "#,
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { conn })
    }

    /// Read-only source: back up before applying any schema changes. SQLite
    /// includes committed WAL content instead of copying potentially stale pages.
    pub fn snapshot_file(source: &std::path::Path, destination: &std::path::Path) -> Result<(), String> {
        let conn = Connection::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
        let pending = destination.with_extension("snapshot-pending");
        if pending.exists() { std::fs::remove_file(&pending).map_err(|e| e.to_string())?; }
        conn.execute("VACUUM INTO ?1", params![pending.to_string_lossy().as_ref()]).map_err(|e| e.to_string())?;
        std::fs::rename(pending, destination).map_err(|e| e.to_string())
    }

    pub fn upsert_book(&self, b: &BookRow) -> Result<(), String> {
        self.conn
            .execute(
                r#"INSERT INTO books(book_id,name,author,thumb,abstract_text,last_item_id,chapter_order,updated_at)
                   VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
                   ON CONFLICT(book_id) DO UPDATE SET name=excluded.name,author=excluded.author,
                     thumb=excluded.thumb,abstract_text=excluded.abstract_text,
                     last_item_id=excluded.last_item_id,chapter_order=excluded.chapter_order,
                     updated_at=excluded.updated_at"#,
                params![b.book_id, b.name, b.author, b.thumb, b.abstract_text, b.last_item_id, b.chapter_order, now()],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn remember_book(&self, b: &BookRow) -> Result<(), String> {
        self.conn
            .execute(
                r#"INSERT INTO books(book_id,name,author,thumb,abstract_text,last_item_id,chapter_order,updated_at)
                   VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
                   ON CONFLICT(book_id) DO UPDATE SET name=excluded.name,author=excluded.author,
                     thumb=excluded.thumb,abstract_text=excluded.abstract_text,updated_at=excluded.updated_at"#,
                params![b.book_id, b.name, b.author, b.thumb, b.abstract_text, b.last_item_id, b.chapter_order, now()],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn set_progress(&self, book_id: &str, item_id: &str, chapter_order: i64) -> Result<(), String> {
        if chapter_order < 0 { return Err("章节序号无效".into()); }
        let tx = self.conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let t = now();
        let changed = tx.execute("UPDATE books SET last_item_id=?2, chapter_order=?3, updated_at=?4 WHERE book_id=?1", params![book_id,item_id,chapter_order,t]).map_err(|e| e.to_string())?;
        if changed == 0 { return Err("请先保存图书信息".into()); }
        tx.execute("INSERT INTO progress_clock VALUES(?1,?2) ON CONFLICT(book_id) DO UPDATE SET timestamp=excluded.timestamp", params![book_id,t]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// Import only newer verified cloud progress. Never overwrite a newer local position.
    pub fn merge_progress(&self, p: &crate::api::ReadingProgress) -> Result<(), String> {
        use rusqlite::OptionalExtension;
        let tx = self.conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let previous: Option<i64> = tx.query_row("SELECT COALESCE((SELECT timestamp FROM progress_clock WHERE book_id=books.book_id), CASE WHEN last_item_id IS NOT NULL THEN updated_at ELSE 0 END) FROM books WHERE book_id=?1", params![p.book_id], |r| r.get(0)).optional().map_err(|e| e.to_string())?;
        if previous.unwrap_or(0) >= p.timestamp { return Ok(()); }
        tx.execute("INSERT INTO books(book_id,name,updated_at,last_item_id,chapter_order) VALUES(?1,'云端阅读记录（书名待加载）',?2,?3,?4) ON CONFLICT(book_id) DO UPDATE SET last_item_id=excluded.last_item_id,chapter_order=excluded.chapter_order,updated_at=excluded.updated_at", params![p.book_id,p.timestamp,p.item_id,p.chapter_order]).map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO progress_clock VALUES(?1,?2) ON CONFLICT(book_id) DO UPDATE SET timestamp=excluded.timestamp", params![p.book_id,p.timestamp]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn cache_chapter(&self, item_id: &str, book_id: &str, title: &str, text: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO chapters(item_id,book_id,title,text,fetched_at) VALUES(?1,?2,?3,?4,?5)
                 ON CONFLICT(item_id) DO UPDATE SET text=excluded.text,title=excluded.title,fetched_at=excluded.fetched_at",
                params![item_id, book_id, title, text, now()],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_chapter(&self, item_id: &str) -> Result<Option<(String, String)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT title, text FROM chapters WHERE item_id=?1")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query_map(params![item_id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|e| e.to_string())?;
        match rows.next() {
            Some(Ok(v)) => Ok(Some(v)),
            Some(Err(e)) => Err(e.to_string()),
            None => Ok(None),
        }
    }

    pub fn get_chapter_for_book(&self, book_id: &str, item_id: &str) -> Result<Option<(String, String)>, String> {
        use rusqlite::OptionalExtension;
        self.conn.query_row("SELECT title,text FROM chapters WHERE book_id=?1 AND item_id=?2", params![book_id,item_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional().map_err(|e| e.to_string())
    }

    pub fn list_books(&self) -> Result<Vec<BookRow>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT book_id,name,author,thumb,abstract_text,last_item_id,chapter_order FROM books ORDER BY updated_at DESC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok(BookRow {
                    book_id: r.get(0)?,
                    name: r.get(1)?,
                    author: r.get(2)?,
                    thumb: r.get(3)?,
                    abstract_text: r.get(4)?,
                    last_item_id: r.get(5)?,
                    chapter_order: r.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_merge_preserves_newer_local_progress() {
        let s = Store::open(":memory:").unwrap();
        let mut p = crate::api::ReadingProgress { book_id:"1".into(),item_id:"10".into(),chapter_order:1,timestamp:100 };
        s.merge_progress(&p).unwrap();
        p.timestamp=90; p.item_id="9".into(); s.merge_progress(&p).unwrap();
        assert_eq!(s.list_books().unwrap()[0].last_item_id.as_deref(),Some("10"));
        p.timestamp=110; p.item_id="11".into(); s.merge_progress(&p).unwrap();
        assert_eq!(s.list_books().unwrap()[0].last_item_id.as_deref(),Some("11"));
        s.set_progress("1","20",20).unwrap(); p.timestamp=120; s.merge_progress(&p).unwrap();
        assert_eq!(s.list_books().unwrap()[0].last_item_id.as_deref(),Some("20"));
        assert!(s.set_progress("missing","1",1).is_err());
        s.cache_chapter("20","1","title","text").unwrap();
        assert!(s.get_chapter_for_book("other","20").unwrap().is_none());
    }

    #[test]
    fn store_roundtrip() {
        let tmp = std::env::temp_dir().join("fq-store-test.sqlite");
        std::fs::remove_file(&tmp).ok();
        let s = Store::open(&tmp).unwrap();
        s.upsert_book(&BookRow {
            book_id: "1".into(), name: "测试书".into(), author: "作者".into(),
            thumb: "".into(), abstract_text: "".into(), last_item_id: Some("10".into()), chapter_order: 3,
        })
        .unwrap();
        s.cache_chapter("10", "1", "第3章", "正文内容").unwrap();
        let (t, c) = s.get_chapter("10").unwrap().unwrap();
        assert_eq!(t, "第3章");
        assert_eq!(c, "正文内容");
        assert_eq!(s.list_books().unwrap().len(), 1);
        std::fs::remove_file(&tmp).ok();
    }
}
