//! Phase 2 native client path: connect to the Swift host over QUIC, decode its HEVC video
//! stream, and present it in the same window/renderer the RFB path uses, while sending
//! keyboard/mouse input back on the same stream.

use std::net::ToSocketAddrs;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

use aktivestream_proto::{DEFAULT_PORT, Message, connect, read_message, write_input, write_set_resolution};
use aktivestream_video::{Codec, Decoder, init as video_init};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::Args;
use crate::app::{self, Backend, HostMsg};
use crate::net::Shared;

/// Connects, learns the remote size, then runs the window with a background decode/input thread.
pub fn run(args: &Args) -> Result<(), String> {
    // The native host listens on 35390; a user who typed no port gets the rfb default (5900),
    // so treat that as "use the native default".
    let port = if args.port == 5900 { DEFAULT_PORT } else { args.port };
    let host = args.host.clone();
    let password = std::env::var("AS_PASSWORD").unwrap_or_else(|_| "native".into());

    let (info_tx, info_rx) = mpsc::channel::<Result<(u32, u32, String), String>>();
    let (shared_tx, shared_rx) = mpsc::channel::<Arc<Shared>>();
    let (input_tx, input_rx) = unbounded_channel::<HostMsg>();

    thread::Builder::new()
        .name("native-session".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = info_tx.send(Err(format!("tokio runtime: {e}")));
                    return;
                }
            };
            let info_for_err = info_tx.clone();
            if let Err(e) = runtime.block_on(session(host, port, password, info_tx, shared_rx, input_rx)) {
                let _ = info_for_err.send(Err(e));
            }
        })
        .map_err(|e| format!("native thread: {e}"))?;

    let (w, h, name) = info_rx.recv().map_err(|_| "native connection failed".to_string())??;
    let shared = Arc::new(Shared::new(w.min(u16::MAX.into()) as u16, h.min(u16::MAX.into()) as u16));
    app::run_window(args, shared, name, Backend::Native(input_tx), Vec::new(), move |shared, _rx| {
        let _ = shared_tx.send(shared);
    })
}

async fn session(
    host: String,
    port: u16,
    password: String,
    info_tx: mpsc::Sender<Result<(u32, u32, String), String>>,
    shared_rx: mpsc::Receiver<Arc<Shared>>,
    mut input_rx: UnboundedReceiver<HostMsg>,
) -> Result<(), String> {
    let socket = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or("could not resolve host")?;
    let connection = connect(socket, None).await.map_err(|e| e.to_string())?;
    let control = connection.control("aktivestream-win", &password).await.map_err(|e| e.to_string())?;
    let (info, mut send, mut recv) = control.into_parts();
    let name = info.name;

    // Serialize input writes onto `send` from one task, so the read loop can await read_message
    // directly instead of inside a select!. read_message is not cancellation-safe: a select! that
    // drops it mid-read (the instant an input event arrives) discards bytes already taken off the
    // stream and desyncs the next video message's length prefix.
    let forwarder = tokio::spawn(async move {
        while let Some(msg) = input_rx.recv().await {
            let r = match msg {
                HostMsg::Input(input) => write_input(&mut send, &input).await,
                HostMsg::Resize(w, h) => write_set_resolution(&mut send, w, h).await,
            };
            if r.is_err() {
                break;
            }
        }
    });

    let mut active: Option<(Decoder, Arc<Shared>)> = None;
    let result = loop {
        let message = match read_message(&mut recv).await {
            Ok(m) => m,
            Err(e) => {
                fail(&active, &e.to_string());
                break Err(e.to_string());
            }
        };
        match message {
            Some(Message::VideoStart(start)) => {
                if let Err(e) = video_init() {
                    break Err(e.to_string());
                }
                let codec = if start.codec == 1 { Codec::H264 } else { Codec::Hevc };
                let decoder = match Decoder::new(codec, start.width, start.height) {
                    Ok(d) => d,
                    Err(e) => break Err(e.to_string()),
                };
                match active.take() {
                    None => {
                        // First stream: hand the size to the window and take its shared framebuffer.
                        if info_tx.send(Ok((start.width, start.height, name.clone()))).is_err() {
                            break Err("window gone".to_string());
                        }
                        let shared = match shared_rx.recv() {
                            Ok(s) => s,
                            Err(_) => break Err("window gone".to_string()),
                        };
                        active = Some((decoder, shared));
                    }
                    Some((_, shared)) => {
                        // Resolution switch mid-session: keep the window's framebuffer and swap the
                        // decoder. The first frame at the new size resizes the framebuffer and the
                        // window re-fits (Shared::present_decoded posts WM_APP_RESIZED on a change).
                        active = Some((decoder, shared));
                    }
                }
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
                            break Err(e.to_string());
                        }
                    }
                }
            }
            Some(_) => {}
            None => {
                if let Some((_, shared)) = &active {
                    shared.disconnect();
                }
                break Ok(());
            }
        }
    };
    forwarder.abort();
    result
}

fn fail(active: &Option<(Decoder, Arc<Shared>)>, error: &str) {
    if let Some((_, shared)) = active {
        shared.report_error(format!("Connection lost: {error}"));
    }
}
