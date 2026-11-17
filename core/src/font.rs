use crate::font_map::FONT_MAP;
use std::collections::HashMap;

pub struct FontDecoder {
    mapping: HashMap<char, char>,
}

fn gid_to_char(gid: u16) -> Option<char> {
    FONT_MAP.iter().find(|(g, _)| *g == gid as u32).map(|(_, c)| *c)
}

impl FontDecoder {
    /// Build decoder from raw woff/woff2 bytes referenced by the reader page.
    pub fn from_font_bytes(data: &[u8]) -> Result<Self, String> {
        let face = ttf_parser::Face::parse(data, 0).map_err(|e| format!("font parse: {e:?}"))?;
        let mut mapping = HashMap::new();
        for (cp, gid) in face.cmap_codepoint_glyph_id_pairs() {
            if !(0xE000..=0xF8FF).contains(&cp) {
                continue;
            }
            if let Some(ch) = gid_to_char(gid) {
                mapping.insert(char::from_u32(cp).unwrap(), ch);
            }
        }
        if mapping.is_empty() {
            return Err("no PUA mappings found in font".into());
        }
        Ok(Self { mapping })
    }

    pub fn decode(&self, text: &str) -> String {
        text.chars().map(|c| self.mapping.get(&c).copied().unwrap_or(c)).collect()
    }

    /// Decode an HTML fragment (chapter content is <p>-tagged), then strip tags.
    pub fn decode_html_to_plain(&self, html: &str) -> String {
        let decoded = self.decode(html);
        let mut out = String::with_capacity(decoded.len());
        let mut in_tag = false;
        for ch in decoded.chars() {
            match ch {
                '<' => in_tag = true,
                '>' => {
                    in_tag = false;
                    out.push('\n');
                }
                _ if !in_tag => out.push(ch),
                _ => {}
            }
        }
        out.replace("&nbsp;", " ")
            .lines()
            .map(str::trim)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    }

    pub fn mapped_count(&self) -> usize {
        self.mapping.len()
    }
}

/// Extract the custom font URL from reader page HTML/CSS.
pub fn extract_font_url(page_html: &str) -> Option<String> {
    let re_start = page_html.find("@font-face")?;
    let window = &page_html[re_start..re_start.min(page_html.len().saturating_add(2000))];
    let at = window.find("url(")?;
    let rest = &window[at + 4..];
    let end = rest.find(')')?;
    Some(rest[..end].trim_matches(['"', '\'']).to_string())
}
