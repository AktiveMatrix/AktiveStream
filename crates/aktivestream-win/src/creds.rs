//! Mac logins in Windows Credential Manager (generic credentials named `AktiveStream/<host>`)
//! and the standard Windows credential prompt.

use windows::Win32::Foundation::{ERROR_CANCELLED, HWND, NO_ERROR};
use windows::Win32::Graphics::Gdi::HBITMAP;
use windows::Win32::Security::Credentials::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CREDUI_FLAGS_ALWAYS_SHOW_UI,
    CREDUI_FLAGS_DO_NOT_PERSIST, CREDUI_FLAGS_EXCLUDE_CERTIFICATES, CREDUI_FLAGS_GENERIC_CREDENTIALS,
    CREDUI_FLAGS_SHOW_SAVE_CHECK_BOX, CREDUI_INFOW, CredDeleteW, CredFree, CredReadW, CredUIPromptForCredentialsW,
    CredWriteW,
};
use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR};

pub struct Login {
    pub user: String,
    pub password: String,
}

pub fn read(target: &str) -> Option<Login> {
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    unsafe {
        CredReadW(&HSTRING::from(target), CRED_TYPE_GENERIC, None, &mut credential).ok()?;
        let c = &*credential;
        let user = if c.UserName.is_null() { String::new() } else { c.UserName.to_string().unwrap_or_default() };
        let blob = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
        let wide: Vec<u16> = blob.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        let password = String::from_utf16_lossy(&wide);
        CredFree(credential as *const _);
        Some(Login { user, password })
    }
}

pub fn write(target: &str, user: &str, password: &str) {
    let mut target_w: Vec<u16> = target.encode_utf16().chain([0]).collect();
    let mut user_w: Vec<u16> = user.encode_utf16().chain([0]).collect();
    let mut blob: Vec<u8> = password.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target_w.as_mut_ptr()),
        UserName: PWSTR(user_w.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        ..Default::default()
    };
    unsafe {
        let _ = CredWriteW(&credential, 0);
    }
    blob.fill(0);
}

pub fn delete(target: &str) {
    unsafe {
        let _ = CredDeleteW(&HSTRING::from(target), CRED_TYPE_GENERIC, None);
    }
}

/// Shows the Windows credential prompt. Returns `None` if the user cancels, otherwise the
/// login and whether "Remember my credentials" was ticked.
pub fn prompt(target: &str, host: &str, user: Option<&str>, auth_error: u32) -> Result<Option<(Login, bool)>, String> {
    let mut user_buf = [0u16; 514];
    let mut password_buf = [0u16; 257];
    for (slot, unit) in user_buf.iter_mut().zip(user.unwrap_or_default().encode_utf16()) {
        *slot = unit;
    }
    let caption = HSTRING::from("AktiveStream");
    let message = HSTRING::from(format!("Mac account for {host}"));
    let info = CREDUI_INFOW {
        cbSize: size_of::<CREDUI_INFOW>() as u32,
        hwndParent: HWND::default(),
        pszMessageText: PCWSTR(message.as_ptr()),
        pszCaptionText: PCWSTR(caption.as_ptr()),
        hbmBanner: HBITMAP::default(),
    };
    let mut save = BOOL(1);
    let flags = CREDUI_FLAGS_GENERIC_CREDENTIALS
        | CREDUI_FLAGS_ALWAYS_SHOW_UI
        | CREDUI_FLAGS_DO_NOT_PERSIST
        | CREDUI_FLAGS_SHOW_SAVE_CHECK_BOX
        | CREDUI_FLAGS_EXCLUDE_CERTIFICATES;
    let result = unsafe {
        CredUIPromptForCredentialsW(
            Some(&info),
            &HSTRING::from(target),
            None,
            auth_error,
            &mut user_buf,
            &mut password_buf,
            Some(&mut save),
            flags,
        )
    };
    if result == ERROR_CANCELLED {
        return Ok(None);
    }
    if result != NO_ERROR {
        return Err(format!("Credential prompt failed (error {})", result.0));
    }
    let take = |buf: &[u16]| String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())]);
    let login = Login { user: take(&user_buf), password: take(&password_buf) };
    password_buf.fill(0);
    Ok(Some((login, save.as_bool())))
}
