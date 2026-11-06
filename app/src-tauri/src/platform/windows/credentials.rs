//! API keys in Windows Credential Manager.

use std::ptr;

use windows_sys::Win32::Foundation::ERROR_NOT_FOUND;
use windows_sys::Win32::Security::Credentials::{
    CRED_MAX_CREDENTIAL_BLOB_SIZE, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW,
    CredDeleteW, CredFree, CredReadW, CredWriteW,
};
use yap_app::credential_store::CredentialStore;

pub struct CredentialManager;

fn target_name(target: &str) -> Result<Vec<u16>, String> {
    if target.is_empty() || target.contains('\0') {
        return Err("Invalid credential target".to_owned());
    }

    Ok(target.encode_utf16().chain(std::iter::once(0)).collect())
}

impl CredentialStore for CredentialManager {
    fn write(&self, target: &str, secret: &str) -> Result<(), String> {
        let mut target = target_name(target)?;
        let mut blob = secret.as_bytes().to_vec();
        if blob.is_empty() || blob.len() > CRED_MAX_CREDENTIAL_BLOB_SIZE as usize {
            return Err("Invalid credential length".to_owned());
        }

        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: target.as_mut_ptr(),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_mut_ptr(),
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        if unsafe { CredWriteW(&credential, 0) } == 0 {
            return Err("Could not save credential in Windows Credential Manager".to_owned());
        }

        Ok(())
    }

    fn read(&self, target: &str) -> Result<Option<String>, String> {
        let target = target_name(target)?;
        let mut credential = ptr::null_mut();
        if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) } == 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
                return Ok(None);
            }

            return Err("Could not read credential from Windows Credential Manager".to_owned());
        }

        let result = unsafe {
            let stored = &*credential;
            let bytes = if stored.CredentialBlobSize == 0 {
                &[][..]
            } else {
                std::slice::from_raw_parts(
                    stored.CredentialBlob,
                    stored.CredentialBlobSize as usize,
                )
            };
            String::from_utf8(bytes.to_vec())
                .map(Some)
                .map_err(|_| "Invalid credential in Windows Credential Manager".to_owned())
        };
        unsafe { CredFree(credential.cast()) };
        result
    }

    fn delete(&self, target: &str) -> Result<(), String> {
        let target = target_name(target)?;
        if unsafe { CredDeleteW(target.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0
            && std::io::Error::last_os_error().raw_os_error() != Some(ERROR_NOT_FOUND as i32)
        {
            return Err("Could not remove credential from Windows Credential Manager".to_owned());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_credentials() {
        assert!(target_name("bad\0target").is_err());
        assert!(CredentialManager.write("Yap/test", "").is_err());
        assert!(
            CredentialManager
                .write(
                    "Yap/test",
                    &"a".repeat(CRED_MAX_CREDENTIAL_BLOB_SIZE as usize + 1)
                )
                .is_err()
        );
    }

    #[test]
    #[ignore = "writes to Windows Credential Manager"]
    fn stores_reads_and_removes_credential() {
        let target = format!("Yap/test/{}", std::process::id());
        CredentialManager.write(&target, "test-secret").unwrap();
        assert_eq!(
            CredentialManager.read(&target).unwrap().as_deref(),
            Some("test-secret")
        );
        CredentialManager.delete(&target).unwrap();
        assert_eq!(CredentialManager.read(&target).unwrap(), None);
    }
}
