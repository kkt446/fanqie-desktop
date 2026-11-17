use windows::core::PCWSTR;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB};

const ENTROPY: &[u8] = b"fanqie-desktop-v1";

fn dpapi_protect(data: &[u8]) -> Result<Vec<u8>, String> {
    unsafe {
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let entropy = CRYPT_INTEGER_BLOB {
            cbData: ENTROPY.len() as u32,
            pbData: ENTROPY.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(&in_blob, PCWSTR::null(), Some(&entropy), None, None, 0, &mut out)
            .map_err(|e| format!("CryptProtectData: {e}"))?;
        let slice = std::slice::from_raw_parts(out.pbData, out.cbData as usize);
        let result = slice.to_vec();
        let _ = LocalFree(HLOCAL(out.pbData as *mut _));
        Ok(result)
    }
}

fn dpapi_unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    unsafe {
        let in_blob = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let entropy = CRYPT_INTEGER_BLOB {
            cbData: ENTROPY.len() as u32,
            pbData: ENTROPY.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(&in_blob, None, Some(&entropy), None, None, 0, &mut out)
            .map_err(|e| format!("CryptUnprotectData: {e}"))?;
        let slice = std::slice::from_raw_parts(out.pbData, out.cbData as usize);
        let result = slice.to_vec();
        let _ = LocalFree(HLOCAL(out.pbData as *mut _));
        Ok(result)
    }
}

/// DPAPI-encrypted cookie vault persisted at `path`.
/// Payload is a JSON array of {name, value, domain, path} entries.
pub struct Vault {
    path: std::path::PathBuf,
}

impl Vault {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn save(&self, cookies_json: &str) -> Result<(), String> {
        let blob = dpapi_protect(cookies_json.as_bytes())?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        use std::{io::Write, os::windows::ffi::OsStrExt};
        use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};
        let tmp = self.path.with_extension("pending");
        let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        file.write_all(&blob).and_then(|_| file.sync_all()).map_err(|e| e.to_string())?;
        drop(file);
        let from: Vec<u16> = tmp.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = self.path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe { MoveFileExW(PCWSTR(from.as_ptr()), PCWSTR(to.as_ptr()), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) }
            .map_err(|e| e.to_string())
    }

    pub fn load(&self) -> Result<Option<String>, String> {
        if !self.path.exists() {
            return Ok(None);
        }
        let blob = std::fs::read(&self.path).map_err(|e| e.to_string())?;
        let plain = dpapi_unprotect(&blob)?;
        String::from_utf8(plain).map(Some).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpapi_roundtrip() {
        let tmp = std::env::temp_dir().join("fq-vault-test.bin");
        let v = Vault::new(&tmp);
        v.save(r#"[{"name":"sessionid","value":"abc"}]"#).unwrap();
        let got = v.load().unwrap().unwrap();
        assert!(got.contains("sessionid"));
        let raw = std::fs::read(&tmp).unwrap();
        assert!(!raw.windows(9).any(|w| w == b"sessionid"));
        std::fs::remove_file(&tmp).ok();
    }
}
