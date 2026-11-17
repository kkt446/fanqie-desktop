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
            CREATE TABLE IF NOT EXISTS chapters(
                item_id TEXT PRIMARY KEY, book_id TEXT NOT NULL, title TEXT NOT NULL,
                text TEXT NOT NULL, fetched_at INTEGER NOT NULL
            );
            "#,
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { conn })
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
