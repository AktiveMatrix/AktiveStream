//! Sends input to the native host to verify CGEvent injection: moves the pointer to a few
//! normalized positions with pauses so a watcher (e.g. /tmp/mousepos over SSH) can confirm.
//! Usage: control <host>[:port]

use std::net::ToSocketAddrs;
use std::time::Duration;

use aktivestream_proto::{DEFAULT_PORT, Input, connect, read_message, write_input};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let target = std::env::args().nth(1).ok_or("usage: control <host>[:port]")?;
    let addr = if target.contains(':') { target } else { format!("{target}:{DEFAULT_PORT}") };
    let socket = addr.to_socket_addrs()?.next().ok_or("could not resolve host")?;

    let connection = connect(socket, None).await?;
    let control = connection.control("aktivestream-control", "test").await?;
    let (info, mut send, mut recv) = control.into_parts();
    println!("connected to {:?}", info.name);

    // Drain the incoming video so flow control never stalls the host.
    tokio::spawn(async move { while matches!(read_message(&mut recv).await, Ok(Some(_))) {} });

    for (x, y) in [(0.5f32, 0.5f32), (0.2, 0.2), (0.8, 0.3), (0.5, 0.9)] {
        write_input(&mut send, &Input::Pointer { buttons: 0, x, y }).await?;
        println!("pointer -> ({x}, {y})");
        tokio::time::sleep(Duration::from_millis(900)).await;
    }
    connection.close().await;
    Ok(())
}
