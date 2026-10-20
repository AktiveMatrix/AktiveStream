//! Connects to the Swift host over QUIC, runs the handshake, and saves the host's synthetic
//! test frame as a PNG. Proves the transport + framing + frame delivery end to end.
//! Usage: connect <host>[:port] [expected_fingerprint]
//! Env: AS_PASSWORD (auth token; defaults to "test").

use std::net::ToSocketAddrs;

use aktivestream_proto::{DEFAULT_PORT, connect};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let target = args.next().ok_or("usage: connect <host>[:port] [expected_fingerprint]")?;
    let expected = args.next();
    let password = std::env::var("AS_PASSWORD").unwrap_or_else(|_| "test".into());

    let addr = if target.contains(':') { target.clone() } else { format!("{target}:{DEFAULT_PORT}") };
    let socket = addr.to_socket_addrs()?.next().ok_or("could not resolve host")?;

    println!("connecting to {socket} ...");
    let started = std::time::Instant::now();
    let connection = connect(socket, expected).await?;
    println!("connected in {} ms, fingerprint {}", started.elapsed().as_millis(), connection.fingerprint);

    let control = connection.control("aktivestream-connect-example", &password).await?;
    println!(
        "handshake ok: host {:?} v{} displays={}",
        control.info.name, control.info.proto_version, control.info.display_count
    );
    drop(control);
    connection.close().await;
    Ok(())
}
