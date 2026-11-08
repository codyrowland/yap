//! API keys in the macOS Keychain.

use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};
use yap_app::credential_store::CredentialStore;

const SERVICE: &str = "Yap";
const ITEM_NOT_FOUND: i32 = -25300;

pub struct Keychain;

pub fn store() -> &'static dyn CredentialStore {
    &Keychain
}

fn check_target(target: &str) -> Result<(), String> {
    if target.is_empty() || target.contains('\0') {
        return Err("Invalid credential target".to_owned());
    }

    Ok(())
}

impl CredentialStore for Keychain {
    fn write(&self, target: &str, secret: &str) -> Result<(), String> {
        check_target(target)?;
        if secret.is_empty() {
            return Err("Invalid credential length".to_owned());
        }

        set_generic_password(SERVICE, target, secret.as_bytes())
            .map_err(|_| "Could not save credential in the macOS Keychain".to_owned())
    }

    fn read(&self, target: &str) -> Result<Option<String>, String> {
        check_target(target)?;
        match get_generic_password(SERVICE, target) {
            Ok(bytes) => String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| "Invalid credential in the macOS Keychain".to_owned()),
            Err(error) if error.code() == ITEM_NOT_FOUND => Ok(None),
            Err(_) => Err("Could not read credential from the macOS Keychain".to_owned()),
        }
    }

    fn delete(&self, target: &str) -> Result<(), String> {
        check_target(target)?;
        match delete_generic_password(SERVICE, target) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == ITEM_NOT_FOUND => Ok(()),
            Err(_) => Err("Could not remove credential from the macOS Keychain".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_credentials() {
        assert!(Keychain.write("bad\0target", "secret").is_err());
        assert!(Keychain.write("Yap/test", "").is_err());
    }

    #[test]
    #[ignore = "writes to the macOS Keychain"]
    fn stores_reads_and_removes_credential() {
        let target = format!("Yap/test/{}", std::process::id());
        Keychain.write(&target, "test-secret").unwrap();
        assert_eq!(
            Keychain.read(&target).unwrap().as_deref(),
            Some("test-secret")
        );
        Keychain.delete(&target).unwrap();
        assert_eq!(Keychain.read(&target).unwrap(), None);
    }
}
