//! Password storage for the desktop client, backed by the OS keyring (Secret Service on Linux,
//! Keychain on macOS, Credential Manager on Windows). The username lives in the connections file;
//! only the secret is stored here, keyed by host. Every operation is best-effort: a missing or
//! locked keyring is never fatal (a password can still be supplied through `RFB_PASSWORD`).

use keyring::Entry;

const SERVICE: &str = "AktiveStream";

fn entry(host: &str) -> Option<Entry> {
    Entry::new(SERVICE, host).ok()
}

pub fn store(host: &str, _username: &str, password: &str) {
    if let Some(entry) = entry(host) {
        let _ = entry.set_password(password);
    }
}

pub fn forget(host: &str) {
    if let Some(entry) = entry(host) {
        let _ = entry.delete_credential();
    }
}

/// The stored password for `host`, if any.
pub fn load(host: &str) -> Option<String> {
    entry(host)?.get_password().ok()
}
