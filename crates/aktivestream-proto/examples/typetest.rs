//! Types an ASCII string into the focused Mac app via native key injection, to verify the
//! host's CGEvent keyboard path end to end. Usage: typetest <host> <text>

use std::net::ToSocketAddrs;
use std::time::Duration;

use aktivestream_proto::{DEFAULT_PORT, Input, connect, read_message, write_input};

const SHIFT: u32 = 0x2_0000; // CGEventFlags.maskShift

/// US-QWERTY virtual keycode for an ASCII char, and whether Shift is needed.
fn keycode(c: char) -> Option<(u16, bool)> {
    const LETTERS: [u16; 26] =
        [0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46, 45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7, 16, 6];
    const DIGITS: [u16; 10] = [29, 18, 19, 20, 21, 23, 22, 26, 28, 25];
    match c {
        'a'..='z' => Some((LETTERS[c as usize - 'a' as usize], false)),
        'A'..='Z' => Some((LETTERS[c as usize - 'A' as usize], true)),
        '0'..='9' => Some((DIGITS[c as usize - '0' as usize], false)),
        ' ' => Some((49, false)),
        '.' => Some((47, false)),
        _ => None,
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let host = std::env::args().nth(1).ok_or("usage: typetest <host> <text>")?;
    let text = std::env::args().nth(2).ok_or("usage: typetest <host> <text>")?;
    let addr = if host.contains(':') { host } else { format!("{host}:{DEFAULT_PORT}") };
    let socket = addr.to_socket_addrs()?.next().ok_or("could not resolve host")?;

    let connection = connect(socket, None).await?;
    let control = connection.control("aktivestream-type", "test").await?;
    let (_info, mut send, mut recv) = control.into_parts();
    tokio::spawn(async move { while matches!(read_message(&mut recv).await, Ok(Some(_))) {} });

    // Give the host a moment to start its input reader.
    tokio::time::sleep(Duration::from_millis(400)).await;
    // Click into the window under the cursor to make its text view first responder.
    for buttons in [0u8, 1, 0] {
        write_input(&mut send, &Input::Pointer { buttons, x: 0.5, y: 0.45 }).await?;
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    for c in text.chars() {
        let Some((code, shift)) = keycode(c) else { continue };
        let flags = if shift { SHIFT } else { 0 };
        write_input(&mut send, &Input::Key { keycode: code, down: true, flags }).await?;
        write_input(&mut send, &Input::Key { keycode: code, down: false, flags }).await?;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    connection.close().await;
    println!("sent {} chars", text.chars().count());
    Ok(())
}
