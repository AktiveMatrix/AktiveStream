//! Phase 2 native client path: connect to the Swift host over QUIC, decode its HEVC video
//! stream, and present it in the same window/renderer the RFB path uses. View-only for now;
//! input injection follows once the host accepts native keycodes.

use std::net::ToSocketAddrs;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use aktivestream_proto::{DEFAULT_PORT, Message, connect};
use aktivestream_video::{Codec, Decoder, init as video_init};

use crate::Args;
use crate::app;
use crate::net::Shared;

/// Connects, learns the remote size, then runs the window with a background decode thread.
pub fn run(args: &Args) -> Result<(), String> {
    // The native host listens on 35390; a user who typed no port gets the rfb default (5900),
    // so treat that as "use the native default".
    let port = if args.port == 5900 { DEFAULT_PORT } else { args.port };
    let host = args.host.clone();
    let password = std::env::var("AS_PASSWORD").unwrap_or_else(|_| "native".into());

    let (info_tx, info_rx) = mpsc::channel::<Result<(u32, u32, String), String>>();
    let (shared_tx, shared_rx) = mpsc::channel::<Arc<Shared>>();

    thread::Builder::new()
        .name("native-session".into())
        .spawn(move || {
            // Multi-thread so quinn's endpoint driver gets a worker thread; the session future
            // (which holds the non-Send Media Foundation decoder) still runs on this thread via
            // block_on, so the decoder never crosses threads.
            let runtime = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = info_tx.send(Err(format!("tokio runtime: {e}")));
                    return;
                }
            };
            let info_for_err = info_tx.clone();
            if let Err(e) = runtime.block_on(session(host, port, password, info_tx, shared_rx)) {
                // Reaches the main thread only if nothing was sent yet (connection never reached
                // VideoStart); otherwise the receiver is gone and this is dropped.
                let _ = info_for_err.send(Err(e));
            }
        })
        .map_err(|e| format!("native thread: {e}"))?;

    let (w, h, name) = info_rx.recv().map_err(|_| "native connection failed".to_string())??;
    let shared = Arc::new(Shared::new(w.min(u16::MAX.into()) as u16, h.min(u16::MAX.into()) as u16));
    app::run_window(args, shared, name, move |shared, _rx| {
        let _ = shared_tx.send(shared);
    })
}

async fn session(
    host: String,
    port: u16,
    password: String,
    info_tx: mpsc::Sender<Result<(u32, u32, String), String>>,
    shared_rx: mpsc::Receiver<Arc<Shared>>,
) -> Result<(), String> {
    let socket = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("could not resolve host")?;
    let connection = connect(socket, None).await.map_err(|e| e.to_string())?;
    let mut control = connection.control("aktivestream-win", &password).await.map_err(|e| e.to_string())?;
    let name = control.info.name.clone();

    let mut active: Option<(Decoder, Arc<Shared>)> = None;
    loop {
        // Once a window exists, surface errors through it rather than only returning them.
        let message = match control.recv().await {
            Ok(m) => m,
            Err(e) => {
                if let Some((_, shared)) = &active {
                    shared.report_error(format!("Connection lost: {e}"));
                }
                return Err(e.to_string());
            }
        };
        match message {
            Some(Message::VideoStart(start)) => {
                video_init().map_err(|e| e.to_string())?;
                let codec = if start.codec == 1 { Codec::H264 } else { Codec::Hevc };
                let decoder = Decoder::new(codec, start.width, start.height).map_err(|e| e.to_string())?;
                // Hand the dimensions to the main thread so it can size and show the window,
                // then wait for the shared framebuffer it creates.
                info_tx.send(Ok((start.width, start.height, name.clone()))).map_err(|_| "window gone")?;
                let shared = shared_rx.recv().map_err(|_| "window gone".to_string())?;
                active = Some((decoder, shared));
            }
            Some(Message::VideoFrame(frame)) => {
                if let Some((decoder, shared)) = active.as_mut() {
                    match decoder.decode(&frame.data) {
                        Ok(frames) => {
                            for decoded in frames {
                                shared.present_decoded(decoded.width, decoded.height, &decoded.rgba);
                            }
                        }
                        Err(e) => {
                            shared.report_error(format!("Connection lost: {e}"));
                            return Err(e.to_string());
                        }
                    }
                }
            }
            Some(_) => {}
            None => {
                if let Some((_, shared)) = &active {
                    shared.disconnect();
                }
                break;
            }
        }
    }
    Ok(())
}
