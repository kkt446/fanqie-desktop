use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Security::Cryptography::{
    CryptProtectData, CryptUnprotectData, CRYPT_DATA_BLOB, CRYPTPROTECT_PROMPTSTRUCT,
};
use windows::Win32::System::Memory::LocalFree;

const ENTROPY: &[u8] = b"fanqie-desktop-v1";

fn dpapi_protect(data: &[u8]) -> Result<Vec<u8>, String> {
    unsafe {
        let in_blob = CRYPT_DATA_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let entropy = CRYPT_DATA_BLOB {
            cbData: ENTROPY.len() as u32,
            pbData: ENTROPY.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_DATA_BLOB::default();
        let hr = CryptProtectData(
            &in_blob,
            PCWSTR::null(),
            Some(&entropy),
            None,
            None,
            Default::default(),
            &mut out,
        );
        if hr != ERROR_SUCCESS {
            return Err(format!("CryptProtectData failed: {hr:?}"));
        }
        let slice = std::slice::from_raw_parts(out.pbData, out.cbData as usize);
        let result = slice.to_vec();
        LocalFree(windows::Win32::Foundation::HLOCAL(out.pbData as *mut _));
        Ok(result)
    }
}

fn dpapi_unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    unsafe {
        let in_blob = CRYPT_DATA_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let entropy = CRYPT_DATA_BLOB {
            cbData: ENTROPY.len() as u32,
            pbData: ENTROPY.as_ptr() as *mut u8,
        };
        let mut out = CRYPT_DATA_BLOB::default();
        let hr = CryptUnprotectData(
            &in_blob,
            Some(&mut PWSTR::null()),
            Some(&entropy),
            None,
            None as Option<*mut CRYPTPROTECT_PROMPTSTRUCT>,
            Default::default(),
            &mut out,
        );
        if hr != ERROR_SUCCESS {
            return Err(format!("CryptUnprotectData failed: {hr:?}"));
        }
        let slice = std::slice::from_raw_parts(out.pbData, out.cbData as usize);
        let result = slice.to_vec();
        LocalFree(windows::Win32::Foundation::HLOCAL(out.pbData as *mut _));
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
        std::fs::write(&self.path, blob).map_err(|e| e.to_string())
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
        assert!(!raw.iter().any(|b| *b == b'a'));
        std::fs::remove_file(&tmp).ok();
    }
}
