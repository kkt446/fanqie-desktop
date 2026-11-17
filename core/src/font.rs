use crate::font_map::FONT_MAP;
use std::collections::HashMap;

pub struct FontDecoder {
    mapping: HashMap<char, char>,
}

fn gid_to_char(gid: u32) -> Option<char> {
    FONT_MAP.iter().find(|(g, _)| *g == gid).map(|(_, c)| *c)
}

/// Parse CFF charset: returns for each glyph index the numeric part of its
/// "gidNNNNN" name (0 when the name is a standard one). Fanqie's obfuscated
/// fonts name glyphs by the original codepoint of the character they draw.
fn cff_charset_numbers(cff: &[u8], num_glyphs: usize) -> Result<Vec<u32>, String> {
    if cff.len() < 4 {
        return Err("cff too short".into());
    }
    let mut pos = 2usize; // skip major/minor
    let hdr_size = cff[pos] as usize;
    pos += 2; // hdrSize + offSize
    let _ = hdr_size;
    // read INDEX helper
    fn read_index(data: &[u8], pos: &mut usize) -> Result<Vec<Vec<u8>>, String> {
        let count = u16::from_be_bytes(data[*pos..*pos + 2].try_into().unwrap()) as usize;
        *pos += 2;
        if count == 0 {
            return Ok(Vec::new());
        }
        let off_size = data[*pos] as usize;
        *pos += 1;
        let mut offs = Vec::with_capacity(count + 1);
        for _ in 0..count + 1 {
            let o = match off_size {
                1 => data[*pos] as usize,
                2 => u16::from_be_bytes(data[*pos..*pos + 2].try_into().unwrap()) as usize,
                3 => ((data[*pos] as usize) << 16) | u16::from_be_bytes(data[*pos + 1..*pos + 3].try_into().unwrap()) as usize,
                4 => u32::from_be_bytes(data[*pos..*pos + 4].try_into().unwrap()) as usize,
                _ => return Err("bad offSize".into()),
            };
            offs.push(o);
            *pos += off_size;
        }
        let base = *pos;
        let mut items = Vec::with_capacity(count);
        for w in offs.windows(2) {
            let (s, e) = (base + w[0] - 1, base + w[1] - 1);
            items.push(data[s..e].to_vec());
        }
        *pos = base + offs[count] - 1;
        Ok(items)
    }
    let _names = read_index(cff, &mut pos)?; // name index
    let top_dicts = read_index(cff, &mut pos)?; // top dict index
    if top_dicts.is_empty() {
        return Err("empty top dict".into());
    }
    let top = &top_dicts[0];
    // parse top dict for operator 15 (charset offset)
    let mut charset_off: Option<usize> = None;
    let mut operands: Vec<i64> = Vec::new();
    let mut ti = 0usize;
    while ti < top.len() {
        let b0 = top[ti];
        match b0 {
            0..=21 => {
                // operator
                let b1 = if b0 == 12 && ti + 1 < top.len() { top[ti + 1] } else { 0 };
                if b0 == 12 {
                    ti += 2;
                } else {
                    ti += 1;
                }
                if b0 == 15 && b1 == 0 {
                    charset_off = operands.last().map(|v| *v as usize);
                }
                operands.clear();
            }
            28 => {
                operands.push(i16::from_be_bytes(top[ti + 1..ti + 3].try_into().unwrap()) as i64);
                ti += 3;
            }
            29 => {
                operands.push(i32::from_be_bytes(top[ti + 1..ti + 5].try_into().unwrap()) as i64);
                ti += 5;
            }
            30 => {
                // real: skip until terminator nibble logic — rare in top dicts
                ti += 1;
                while ti < top.len() && (top[ti] & 0xF0 == 0 || top[ti] & 0x0F == 0xF) {
                    ti += 1;
                }
                if ti < top.len() {
                    ti += 1;
                }
            }
            255 => {
                operands.push(i32::from_be_bytes(top[ti + 1..ti + 5].try_into().unwrap()) as i64);
                ti += 5;
            }
            0x81 => {
                operands.push(i16::from_be_bytes(top[ti + 1..ti + 3].try_into().unwrap()) as i64);
                ti += 3;
            }
            0x82 => {
                operands.push(i32::from_be_bytes(top[ti + 1..ti + 5].try_into().unwrap()) as i64);
                ti += 5;
            }
            247..=250 => {
                let a = top[ti + 1] as i64;
                let b = top[ti + 2] as i64;
                operands.push((a - 247) * 256 + b + 108);
                ti += 3;
            }
            251..=254 => {
                let a = top[ti + 1] as i64;
                let b = top[ti + 2] as i64;
                operands.push(-((a - 251) * 256) - b - 108);
                ti += 3;
            }
            _ => {
                operands.push(b0 as i64 - 139);
                ti += 1;
            }
        }
    }
    let charset_off = charset_off.ok_or("no charset offset in top dict")?;
    // strings index follows top dict index in standard layout
    let strings = read_index(cff, &mut pos)?;
    let name_of_sid = |sid: usize| -> Option<&[u8]> {
        if sid >= 391 {
            strings.get(sid - 391).map(Vec::as_slice)
        } else {
            None
        }
    };
    let mut out = vec![0u32; num_glyphs];
    let mut cp = charset_off;
    let fmt = cff[cp];
    cp += 1;
    let mut gid = 1usize; // glyph 0 is .notdef
    match fmt {
        0 => {
            while gid < num_glyphs {
                let sid = u16::from_be_bytes(cff[cp..cp + 2].try_into().unwrap()) as usize;
                cp += 2;
                if let Some(n) = name_of_sid(sid) {
                    if let Some(v) = parse_gid_name(n) {
                        out[gid] = v;
                    }
                }
                gid += 1;
            }
        }
        1 => loop {
            let first = u16::from_be_bytes(cff[cp..cp + 2].try_into().unwrap()) as usize;
            let n_left = cff[cp + 2] as usize;
            cp += 3;
            for k in 0..=n_left {
                if gid >= num_glyphs {
                    break;
                }
                let sid = first + k;
                if let Some(n) = name_of_sid(sid) {
                    if let Some(v) = parse_gid_name(n) {
                        out[gid] = v;
                    }
                }
                gid += 1;
            }
            if gid >= num_glyphs {
                break;
            }
        },
        2 => loop {
            let first = u16::from_be_bytes(cff[cp..cp + 2].try_into().unwrap()) as usize;
            let n_left1 = u16::from_be_bytes(cff[cp + 2..cp + 4].try_into().unwrap()) as usize;
            let n_left2 = u16::from_be_bytes(cff[cp + 4..cp + 6].try_into().unwrap()) as usize;
            cp += 6;
            for k in 0..=n_left1 {
                if gid >= num_glyphs {
                    break;
                }
                let sid = first + k;
                if let Some(n) = name_of_sid(sid) {
                    if let Some(v) = parse_gid_name(n) {
                        out[gid] = v;
                    }
                }
                gid += 1;
            }
            for k in 1..=n_left2 {
                if gid >= num_glyphs {
                    break;
                }
                let sid = first + n_left1 + k;
                if let Some(n) = name_of_sid(sid) {
                    if let Some(v) = parse_gid_name(n) {
                        out[gid] = v;
                    }
                }
                gid += 1;
            }
            if gid >= num_glyphs {
                break;
            }
        },
        _ => return Err("bad charset format".into()),
    }
    Ok(out)
}

fn parse_gid_name(name: &[u8]) -> Option<u32> {
    let s = std::str::from_utf8(name).ok()?;
    s.strip_prefix("gid")?.parse().ok()
}

/// Locate a table by tag in a raw sfnt buffer.
fn find_sfnt_table<'a>(sfnt: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    if sfnt.len() < 12 {
        return None;
    }
    let n = u16::from_be_bytes(sfnt[4..6].try_into().ok()?) as usize;
    for i in 0..n {
        let off = 12 + i * 16;
        if off + 16 > sfnt.len() {
            return None;
        }
        if &sfnt[off..off + 4] == tag {
            let o = u32::from_be_bytes(sfnt[off + 8..off + 12].try_into().ok()?) as usize;
            let l = u32::from_be_bytes(sfnt[off + 12..off + 16].try_into().ok()?) as usize;
            return sfnt.get(o..o + l);
        }
    }
    None
}

fn sfnt_bytes(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() >= 4 && &data[0..4] == b"wOFF" {
        woff_to_sfnt(data)
    } else if data.len() >= 4 && &data[0..4] == b"wOF2" {
        Err("woff2 not supported; request the .woff url".into())
    } else {
        Ok(data.to_vec())
    }
}

/// Deterministic hash of a glyph outline. Fanqie ships several fonts that
/// contain the same 362 outlines under different PUA assignments; matching
/// outlines lets us carry the verified name->char table across them.
struct OutlineHasher(u64);

impl OutlineHasher {
    fn mix(&mut self, tag: u8, pts: &[f32]) {
        self.0 ^= tag as u64;
        self.0 = self.0.wrapping_mul(0x100000001b3);
        for v in pts {
            self.0 ^= v.to_bits() as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
}

impl ttf_parser::OutlineBuilder for OutlineHasher {
    fn move_to(&mut self, x: f32, y: f32) {
        self.mix(1, &[x, y]);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.mix(2, &[x, y]);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.mix(3, &[x1, y1, x, y]);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.mix(4, &[x1, y1, x2, y2, x, y]);
    }
    fn close(&mut self) {
        self.mix(5, &[]);
    }
}

/// (PUA char, outline hash) for every mapped glyph of a font.
fn pua_outline_hashes(data: &[u8]) -> Result<Vec<(char, u64)>, String> {
    let bytes = sfnt_bytes(data)?;
    let face = ttf_parser::Face::parse(&bytes, 0).map_err(|e| format!("font parse: {e:?}"))?;
    let mut out = Vec::new();
    for cp in 0xE000u32..=0xF8FF {
        let Some(c) = char::from_u32(cp) else { continue };
        let Some(gid) = face.glyph_index(c) else { continue };
        let mut h = OutlineHasher(0xcbf29ce484222325);
        if face.outline_glyph(gid, &mut h).is_some() {
            out.push((c, h.0));
        }
    }
    if out.is_empty() {
        return Err("no PUA outlines".into());
    }
    Ok(out)
}

impl FontDecoder {
    /// Build decoder from raw font bytes (WOFF1 converted, or plain sfnt).
    pub fn from_font_bytes(data: &[u8]) -> Result<Self, String> {
        let bytes = sfnt_bytes(data)?;
        let face = ttf_parser::Face::parse(&bytes, 0).map_err(|e| format!("font parse: {e:?}"))?;
        let num_glyphs = face.tables().maxp.number_of_glyphs.get() as usize;
        let cff_bytes = find_sfnt_table(&bytes, b"CFF ").ok_or("no CFF table")?;
        let charset = cff_charset_numbers(cff_bytes, num_glyphs)?;
        let mut mapping = HashMap::new();
        let mut hits = 0usize;
        let mut sample: Vec<(u32, u32)> = Vec::new();
        for cp in 0xE000u32..=0xF8FF {
            let Some(c) = char::from_u32(cp) else { continue };
            let Some(gid) = face.glyph_index(c) else { continue };
            hits += 1;
            let Some(name_num) = charset.get(gid.0 as usize).copied().filter(|v| *v != 0) else { continue };
            if sample.len() < 6 {
                sample.push((cp, name_num));
            }
            let Some(ch) = gid_to_char(name_num) else { continue };
            mapping.insert(c, ch);
        }
        if mapping.is_empty() {
            return Err(format!("no PUA mappings (hits: {hits}, sfnt len: {}, sample: {sample:?})", bytes.len()));
        }
        Ok(Self { mapping })
    }

    /// Decode `active` by matching its glyph outlines against `reference`,
    /// whose names are known to be covered by the static table.
    pub fn from_font_pair(active: &[u8], reference: &[u8]) -> Result<Self, String> {
        let ref_dec = Self::from_font_bytes(reference)?;
        let mut hash_to_char: HashMap<u64, char> = HashMap::new();
        for (c, h) in pua_outline_hashes(reference)? {
            if let Some(ch) = ref_dec.mapping.get(&c) {
                hash_to_char.insert(h, *ch);
            }
        }
        if hash_to_char.is_empty() {
            return Err("reference font has no decodable outlines".into());
        }
        let mut mapping = HashMap::new();
        for (c, h) in pua_outline_hashes(active)? {
            if let Some(ch) = hash_to_char.get(&h) {
                mapping.insert(c, *ch);
            }
        }
        if mapping.is_empty() {
            return Err("outline match produced no mappings".into());
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

/// Extract the custom font URL from reader page HTML/CSS. Prefer .woff
/// (simple zlib container we can decode locally); woff2 needs brotli +
/// glyph transform and is not supported here.
pub fn extract_font_url(page_html: &str) -> Option<String> {
    let mut woff = None;
    let mut woff2 = None;
    let mut rest = page_html;
    while let Some(at) = rest.find("url(") {
        let after = &rest[at + 4..];
        let Some(end) = after.find(')') else { break };
        let url = after[..end].trim_matches(['"', '\'']).to_string();
        if url.ends_with(".woff") {
            woff.get_or_insert(url.clone());
        } else if url.ends_with(".woff2") {
            woff2.get_or_insert(url.clone());
        }
        rest = &rest[at + 4..];
    }
    woff.or(woff2)
}

/// Convert a WOFF1 container to a plain sfnt/TTF byte buffer.
fn woff_to_sfnt(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.len() < 44 || &data[0..4] != b"wOFF" {
        return Err("not a WOFF1 file".into());
    }
    let rd_u16 = |o: usize| u16::from_be_bytes(data[o..o + 2].try_into().unwrap()) as usize;
    let rd_u32 = |o: usize| u32::from_be_bytes(data[o..o + 4].try_into().unwrap());
    let num_tables = rd_u16(12);
    let total = rd_u32(8) as usize; // total WOFF file length
    if data.len() < total {
        return Err("truncated WOFF".into());
    }
    struct Entry {
        tag: [u8; 4],
        orig_len: u32,
        orig_checksum: u32,
        data: Vec<u8>,
    }
    let mut entries = Vec::with_capacity(num_tables);
    let mut dir_off = 44usize;
    for _ in 0..num_tables {
        let mut tag = [0u8; 4];
        tag.copy_from_slice(&data[dir_off..dir_off + 4]);
        let woff_offset = rd_u32(dir_off + 4) as usize;
        let comp_len = rd_u32(dir_off + 8) as usize;
        let orig_len = rd_u32(dir_off + 12);
        let orig_checksum = rd_u32(dir_off + 16);
        let raw = &data[woff_offset..woff_offset + comp_len];
        let table = if comp_len < orig_len as usize {
            use std::io::Read;
            let mut d = flate2::read::ZlibDecoder::new(raw);
            let mut buf = Vec::with_capacity(orig_len as usize);
            d.read_to_end(&mut buf).map_err(|e| format!("inflate {tag:?}: {e}"))?;
            buf
        } else {
            raw.to_vec()
        };
        entries.push(Entry { tag, orig_len, orig_checksum, data: table });
        dir_off += 20;
    }
    // build sfnt
    let n = entries.len();
    let mut out = Vec::new();
    let header = 12 + 16 * n;
    let padded: Vec<usize> = entries.iter().map(|e| (e.orig_len as usize + 3) & !3).collect();
    let mut offset = header;
    let entry_sel = (n as f64).log2().floor() as u32;
    let search_range: u32 = (1 << entry_sel) * 16;
    let range_shift: u32 = (n as u32) * 16 - search_range;
    out.extend_from_slice(&0x00010000u32.to_be_bytes());
    out.extend_from_slice(&(n as u16).to_be_bytes());
    out.extend_from_slice(&(search_range as u16).to_be_bytes());
    out.extend_from_slice(&(entry_sel as u16).to_be_bytes());
    out.extend_from_slice(&(range_shift as u16).to_be_bytes());
    let mut body: Vec<u8> = Vec::new();
    for (e, pad) in entries.iter().zip(&padded) {
        out.extend_from_slice(&e.tag);
        out.extend_from_slice(&e.orig_checksum.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&e.orig_len.to_be_bytes());
        body.extend_from_slice(&e.data);
        body.resize(body.len() + pad - e.data.len(), 0);
        offset += pad;
    }
    out.extend_from_slice(&body);
    Ok(out)
}
