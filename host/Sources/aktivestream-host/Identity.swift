// The host's TLS identity: a self-signed P-256 certificate persisted in Application Support,
// generated once with openssl and loaded as a SecIdentity. The client pins its SHA-256
// fingerprint on first connect (trust on first use), so no CA is involved.
import CryptoKit
import Foundation
import Security

struct HostIdentity {
    let secIdentity: SecIdentity
    let certificateDER: Data
    /// Lowercase hex SHA-256 of the DER certificate, shown to and pinned by the client.
    var fingerprint: String {
        SHA256.hash(data: certificateDER).map { String(format: "%02x", $0) }.joined()
    }
}

enum IdentityError: Error, CustomStringConvertible {
    case openssl(String)
    case importFailed(OSStatus)
    case noIdentity
    var description: String {
        switch self {
        case .openssl(let s): return "openssl: \(s)"
        case .importFailed(let s): return "PKCS12 import failed: \(s)"
        case .noIdentity: return "no identity in PKCS12"
        }
    }
}

func supportDirectory() throws -> URL {
    let base = try FileManager.default.url(
        for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
    let dir = base.appendingPathComponent("AktiveStream", isDirectory: true)
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir
}

/// Loads the persisted identity, generating one with openssl on first run. The private key
/// lives in a dedicated file-based keychain in the agent's support dir, so a SecIdentity can
/// form without the login keychain (which is unavailable over SSH and would couple the agent
/// to the user's keychain).
func loadOrCreateIdentity() throws -> HostIdentity {
    let dir = try supportDirectory()
    let p12URL = dir.appendingPathComponent("identity.p12")
    let passphrase = "aktivestream"
    if !FileManager.default.fileExists(atPath: p12URL.path) {
        try generateIdentity(into: dir, passphrase: passphrase)
    }
    // Re-import into a fresh keychain each run. Keychain private-key ACLs are tied to the
    // importing binary; a rebuilt (ad-hoc, new cdhash) agent would otherwise be denied the
    // key and TLS signing would fail. The cert/fingerprint stays stable via the persisted p12.
    let keychainURL = dir.appendingPathComponent("aktivestream.keychain-db")
    try? FileManager.default.removeItem(at: keychainURL)
    let keychain = try openKeychain(at: keychainURL, password: passphrase)
    let data = try Data(contentsOf: p12URL)
    var items: CFArray?
    var options: [String: Any] = [
        kSecImportExportPassphrase as String: passphrase,
        kSecImportExportKeychain as String: keychain,
    ]
    // Import the key so any application may use it with no confirmation prompt. The agent runs in a
    // headless GUI login session, where a keychain-access dialog is invisible and blocks the import
    // forever (the TLS handshake then never completes and the agent never binds its port). The key
    // still lives only in the agent's private, passphrase-locked keychain file in its support dir.
    if let access = allowAllApplicationsAccess() {
        options[kSecImportExportAccess as String] = access
    }
    let status = SecPKCS12Import(data as CFData, options as CFDictionary, &items)
    guard status == errSecSuccess, let array = items as? [[String: Any]],
        let identityAny = array.first?[kSecImportItemIdentity as String]
    else {
        throw IdentityError.importFailed(status)
    }
    // Only after the import: stop the keychain auto-locking, and keep re-unlocking it so that a
    // sleep/wake which locks it anyway recovers within seconds rather than breaking every TLS
    // handshake until the agent is restarted.
    disableAutoLock(keychain, password: passphrase)
    keepUnlocked(keychain, password: passphrase)
    return hostIdentity(from: identityAny as! SecIdentity)
}

/// A keychain access that lets any application use the imported key with no confirmation prompt, so
/// a headless GUI-session agent never blocks on an invisible keychain-access dialog.
private func allowAllApplicationsAccess() -> SecAccess? {
    var access: SecAccess?
    guard SecAccessCreate("AktiveStream Host" as CFString, nil, &access) == errSecSuccess, let access else {
        return nil
    }
    var aclList: CFArray?
    guard SecAccessCopyACLList(access, &aclList) == errSecSuccess, let acls = aclList as? [SecACL] else {
        return access
    }
    for acl in acls {
        // A nil trusted-application list trusts every application; an empty prompt selector means no
        // confirmation dialog is ever shown for use of the key.
        SecACLSetContents(acl, nil, "AktiveStream Host" as CFString, [])
    }
    return access
}

private func hostIdentity(from identity: SecIdentity) -> HostIdentity {
    var certRef: SecCertificate?
    SecIdentityCopyCertificate(identity, &certRef)
    let der = certRef.map { SecCertificateCopyData($0) as Data } ?? Data()
    return HostIdentity(secIdentity: identity, certificateDER: der)
}

private func openKeychain(at url: URL, password: String) throws -> SecKeychain {
    var keychain: SecKeychain?
    let path = url.path
    let pw = Array(password.utf8)
    if FileManager.default.fileExists(atPath: path) {
        let status = SecKeychainOpen(path, &keychain)
        if status == errSecSuccess, let keychain {
            SecKeychainUnlock(keychain, UInt32(pw.count), pw, true)
            return keychain
        }
    }
    let status = SecKeychainCreate(path, UInt32(pw.count), pw, false, nil, &keychain)
    guard status == errSecSuccess, let keychain else { throw IdentityError.importFailed(status) }
    return keychain
}

/// Keep the TLS private key usable for the life of the agent: a keychain that locks on sleep or
/// after an idle interval would start failing the TLS handshake on a long-running agent (the
/// server can no longer sign), which looks like a client-side `crypto TLS error`.
private func disableAutoLock(_ keychain: SecKeychain, password: String) {
    var settings = SecKeychainSettings(
        version: 1, lockOnSleep: false, useLockInterval: false, lockInterval: UInt32.max)
    SecKeychainSetSettings(keychain, &settings)
    // SecKeychainSetSettings can leave the keychain locked; re-unlock so the TLS key stays usable.
    let pw = Array(password.utf8)
    SecKeychainUnlock(keychain, UInt32(pw.count), pw, true)
}

/// Re-unlocks the keychain every few seconds with the known passphrase (no prompt), so a lock
/// from sleep/wake self-heals and the agent keeps serving TLS without a manual restart.
private func keepUnlocked(_ keychain: SecKeychain, password: String) {
    let box = SendableBox(value: keychain)
    Thread.detachNewThread {
        let pw = Array(password.utf8)
        while true {
            Thread.sleep(forTimeInterval: 10)
            SecKeychainUnlock(box.value, UInt32(pw.count), pw, true)
        }
    }
}


private func generateIdentity(into dir: URL, passphrase: String) throws {
    let key = dir.appendingPathComponent("key.pem")
    let cert = dir.appendingPathComponent("cert.pem")
    let p12 = dir.appendingPathComponent("identity.p12")
    // RSA, not EC: macOS's SecPKCS12Import reads RSA archives from LibreSSL's openssl reliably,
    // whereas the EC-key archive fails to import. TLS 1.3 is fine with an RSA certificate.
    // -addext forces an X.509 v3 certificate (v1 is rejected by rustls/webpki with
    // UnsupportedCertVersion); the SAN also gives the client a stable name to connect to.
    try run("/usr/bin/openssl", [
        "req", "-x509", "-newkey", "rsa:2048",
        "-keyout", key.path, "-out", cert.path, "-days", "3650", "-nodes",
        "-subj", "/CN=AktiveStream Host",
        "-addext", "subjectAltName=DNS:aktivestream-host",
        "-addext", "basicConstraints=critical,CA:FALSE",
        "-addext", "keyUsage=digitalSignature,keyEncipherment",
    ])
    try run("/usr/bin/openssl", [
        "pkcs12", "-export", "-inkey", key.path, "-in", cert.path,
        "-out", p12.path, "-passout", "pass:\(passphrase)", "-name", "aktivestream",
    ])
    try? FileManager.default.removeItem(at: key)
}

private func run(_ tool: String, _ args: [String]) throws {
    let process = Process()
    process.executableURL = URL(fileURLWithPath: tool)
    process.arguments = args
    let err = Pipe()
    process.standardError = err
    try process.run()
    process.waitUntilExit()
    if process.terminationStatus != 0 {
        let message = String(data: err.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        throw IdentityError.openssl(message)
    }
}
