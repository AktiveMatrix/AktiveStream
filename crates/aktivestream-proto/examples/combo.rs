//! Sends one key chord (keyDown+keyUp with modifier flags) to the native host, to verify the
//! keyboard + modifier injection path with a system hotkey.
//! Usage: combo <host> <keycode> <flags_hex>  e.g. combo mac 20 120000  (Cmd+Shift+3)

use std::net::ToSocketAddrs;
use std::time::Duration;

use aktivestream_proto::{DEFAULT_PORT, Input, connect, read_message, write_input};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let host = args.next().ok_or("usage: combo <host> <keycode> <flags_hex>")?;
    let keycode: u16 = args.next().ok_or("missing keycode")?.parse()?;
    let flags = u32::from_str_radix(&args.next().ok_or("missing flags")?, 16)?;
    let hold_ms: u64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(60);
    let addr = if host.contains(':') { host } else { format!("{host}:{DEFAULT_PORT}") };
    let socket = addr.to_socket_addrs()?.next().ok_or("could not resolve host")?;

    let connection = connect(socket, None).await?;
    let control = connection.control("aktivestream-combo", "test").await?;
    let (_info, mut send, mut recv) = control.into_parts();
    tokio::spawn(async move { while matches!(read_message(&mut recv).await, Ok(Some(_))) {} });

    tokio::time::sleep(Duration::from_millis(400)).await;
    write_input(&mut send, &Input::Key { keycode, down: true, flags }).await?;
    tokio::time::sleep(Duration::from_millis(hold_ms)).await;
    write_input(&mut send, &Input::Key { keycode, down: false, flags }).await?;
    tokio::time::sleep(Duration::from_millis(400)).await;
    connection.close().await;
    println!("sent chord keycode={keycode} flags=0x{flags:x}");
    Ok(())
}
