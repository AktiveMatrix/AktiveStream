//! An in-process server that plays macOS Screen Sharing's side of the protocol.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, KeyInit};
use num_bigint::BigUint;

use super::encode::{Deflater, Tile, image_for, raw_bytes, zrle_tiles};
use crate::auth::{apple_dh_key, random_bytes, to_fixed, vnc_response};
use crate::decode;
use crate::session::lock;
use crate::{ConnectOptions, Error, Framebuffer, Rect, SecurityType, ServerEvent, connect};

/// RFC 2409 Oakley group 2 (1024-bit MODP), generator 2.
const OAKLEY2: &str = "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B139B22514A08798E3404DD\
EF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED\
EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE65381FFFFFFFFFFFFFFFF";

fn rd<const N: usize>(s: &mut TcpStream) -> [u8; N] {
    let mut b = [0u8; N];
    s.read_exact(&mut b).unwrap();
    b
}

fn rd_vec(s: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    s.read_exact(&mut b).unwrap();
    b
}

fn fbur(incremental: bool, w: u16, h: u16) -> [u8; 10] {
    crate::wire::update_request(incremental, Rect { x: 0, y: 0, w, h })
}

fn rect_header(m: &mut Vec<u8>, x: u16, y: u16, w: u16, h: u16, encoding: i32) {
    for v in [x, y, w, h] {
        m.extend_from_slice(&v.to_be_bytes());
    }
    m.extend_from_slice(&encoding.to_be_bytes());
}

fn with_len(m: &mut Vec<u8>, payload: &[u8]) {
    m.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    m.extend_from_slice(payload);
}

fn spawn<T: Send + 'static>(serve: impl FnOnce(TcpStream) -> T + Send + 'static) -> (u16, JoinHandle<T>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    (port, thread::spawn(move || serve(listener.accept().unwrap().0)))
}

/// Server side of version and security negotiation. Returns whether the login was accepted.
fn handshake(s: &mut TcpStream, version: &[u8; 12], offered: &[u8], user: &str, password: &str) -> bool {
    s.write_all(version).unwrap();
    let legacy = version == b"RFB 003.003\n";
    let reply = rd::<12>(s);
    let chosen = if legacy {
        assert_eq!(&reply, b"RFB 003.003\n");
        s.write_all(&2u32.to_be_bytes()).unwrap();
        2
    } else {
        assert_eq!(&reply, b"RFB 003.008\n");
        let mut m = vec![offered.len() as u8];
        m.extend_from_slice(offered);
        s.write_all(&m).unwrap();
        rd::<1>(s)[0]
    };
    let accepted = match chosen {
        30 => {
            let prime = BigUint::parse_bytes(OAKLEY2.as_bytes(), 16).unwrap();
            let key_len = 128;
            let private = BigUint::from_bytes_be(&random_bytes(64).unwrap());
            let public = BigUint::from(2u8).modpow(&private, &prime);
            let mut m = Vec::new();
            m.extend_from_slice(&2u16.to_be_bytes());
            m.extend_from_slice(&(key_len as u16).to_be_bytes());
            m.extend_from_slice(&to_fixed(&prime, key_len));
            m.extend_from_slice(&to_fixed(&public, key_len));
            s.write_all(&m).unwrap();
            let mut creds = rd_vec(s, 128);
            let client_public = BigUint::from_bytes_be(&rd_vec(s, key_len));
            let key = apple_dh_key(&client_public.modpow(&private, &prime), key_len);
            let cipher = Aes128::new(GenericArray::from_slice(&key));
            for block in creds.as_chunks_mut::<16>().0 {
                cipher.decrypt_block(GenericArray::from_mut_slice(block));
            }
            let field = |f: &[u8]| String::from_utf8(f[..f.iter().position(|&b| b == 0).unwrap()].to_vec()).unwrap();
            field(&creds[..64]) == user && field(&creds[64..]) == password
        }
        2 => {
            let challenge: [u8; 16] = random_bytes(16).unwrap().try_into().unwrap();
            s.write_all(&challenge).unwrap();
            rd::<16>(s) == vnc_response(&challenge, password)
        }
        other => panic!("client chose security type {other}"),
    };
    let mut m = (!accepted as u32).to_be_bytes().to_vec();
    if !accepted && !legacy {
        with_len(&mut m, b"bad login");
    }
    s.write_all(&m).unwrap();
    accepted
}

/// ClientInit, ServerInit, and the client's setup messages.
fn server_init(s: &mut TcpStream, w: u16, h: u16) {
    assert_eq!(rd::<1>(s), [1], "shared flag");
    let mut m = Vec::new();
    m.extend_from_slice(&w.to_be_bytes());
    m.extend_from_slice(&h.to_be_bytes());
    m.extend_from_slice(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
    with_len(&mut m, b"Fake Mac");
    s.write_all(&m).unwrap();

    let pf = rd::<20>(s);
    assert_eq!((pf[0], &pf[4..8], &pf[14..17]), (0, &[32u8, 24, 0, 1][..], &[16u8, 8, 0][..]));
    let hdr = rd::<4>(s);
    assert_eq!(hdr[0], 2);
    let n = u16::from_be_bytes([hdr[2], hdr[3]]) as usize;
    let encodings: Vec<i32> = rd_vec(s, n * 4).chunks(4).map(|c| i32::from_be_bytes(c.try_into().unwrap())).collect();
    assert_eq!(encodings[0], 16, "ZRLE preferred");
    assert!(encodings.contains(&-239) && encodings.contains(&-224));
    // Auto-pasteboard enable (type 0x15) precedes the first update request by default.
    assert_eq!(rd::<8>(s), [0x15, 0, 0, 1, 0, 0, 0, 0], "auto pasteboard enable");
    assert_eq!(rd::<10>(s), fbur(false, w, h), "first request is a full update");
}

#[test]
fn apple_dh_session_end_to_end() {
    let (port, server) = spawn(|mut s| {
        assert!(handshake(&mut s, b"RFB 003.889\n", &[30, 33, 36, 2, 35], "tester", "pässword"));
        server_init(&mut s, 150, 70);
        let (mut zrle, mut zlib) = (Deflater::new(), Deflater::new());
        let mut model = Framebuffer::new(150, 70);

        // Update 1: ZRLE full screen, Raw, Zlib, CopyRect, Cursor.
        let modes = [Tile::Raw, Tile::Solid, Tile::Packed, Tile::PaletteRle, Tile::PlainRle, Tile::Packed];
        let full = image_for(150, 70, |i| modes[i], |_| 4);
        let mut m = vec![0u8, 0];
        m.extend_from_slice(&5u16.to_be_bytes());
        rect_header(&mut m, 0, 0, 150, 70, 16);
        with_len(&mut m, &zrle.chunk(&zrle_tiles(&full, 150, 70, |i| modes[i])));
        model.pixels.copy_from_slice(&full);
        let raw: Vec<u32> = (0..100).map(|i| 0x00AB_0000 | i).collect();
        rect_header(&mut m, 5, 5, 10, 10, 0);
        m.extend_from_slice(&raw_bytes(&raw));
        decode::blit(&mut model, Rect { x: 5, y: 5, w: 10, h: 10 }, &raw);
        let zl: Vec<u32> = (0..160).map(|i| 0x0000_CD00 | i).collect();
        rect_header(&mut m, 100, 50, 20, 8, 6);
        with_len(&mut m, &zlib.chunk(&raw_bytes(&zl)));
        decode::blit(&mut model, Rect { x: 100, y: 50, w: 20, h: 8 }, &zl);
        rect_header(&mut m, 120, 40, 16, 16, 1);
        m.extend_from_slice(&[0, 0, 0, 0]);
        decode::copy_rect(&mut model, Rect { x: 120, y: 40, w: 16, h: 16 }, 0, 0);
        let cursor: Vec<u32> = (0..12).map(|i| 0x0011_0000 * i).collect();
        rect_header(&mut m, 2, 1, 4, 3, -239);
        m.extend_from_slice(&raw_bytes(&cursor));
        m.extend_from_slice(&[0xF0, 0x90, 0xF0]);
        s.write_all(&m).unwrap();
        assert_eq!(rd::<10>(&mut s), fbur(true, 150, 70), "incremental request after update 1");

        // Update 2, LastRect-terminated: resize to 200x100 and fill it.
        let mut m = vec![0u8, 0, 0xFF, 0xFF];
        rect_header(&mut m, 0, 0, 200, 100, -223);
        rect_header(&mut m, 0, 0, 200, 100, 16);
        with_len(&mut m, &zrle.chunk(&zrle_tiles(&vec![0x0042_4242; 200 * 100], 200, 100, |_| Tile::Solid)));
        rect_header(&mut m, 0, 0, 0, 0, -224);
        // SetColourMapEntries (ignored), Bell, ServerCutText in Latin-1.
        m.extend_from_slice(&[1, 0, 0, 0, 0, 1, 0xFF, 0xFF, 0, 0, 0, 0]);
        m.push(2);
        m.extend_from_slice(&[3, 0, 0, 0]);
        with_len(&mut m, &[b'h', 0xE9, b'l', b'l', b'o']);
        s.write_all(&m).unwrap();
        assert_eq!(rd::<10>(&mut s), fbur(true, 200, 100), "incremental request uses the new size");

        // Client input sent after it saw the resize.
        assert_eq!(rd::<8>(&mut s), [4, 1, 0, 0, 0, 0, 0xFF, 0xEB]);
        assert_eq!(rd::<6>(&mut s), [5, 1, 0, 10, 0, 20]);
        assert_eq!(rd::<8>(&mut s), [6, 0, 0, 0, 0, 0, 0, 4]);
        assert_eq!(&rd::<4>(&mut s), b"hi ?");
        model
    });

    let options = ConnectOptions::new("127.0.0.1", port, Some("tester".into()), "pässword");
    let (info, mut reader, mut writer) = connect(&options).unwrap();
    assert_eq!(
        (info.protocol.as_str(), info.security, info.width, info.height, info.name.as_str()),
        ("RFB 003.889", SecurityType::AppleDh, 150, 70, "Fake Mac")
    );
    // Deliberately 0x0: the reader must size it before writing pixels.
    let fb = Mutex::new(Framebuffer::default());
    let mut events = Vec::new();
    let mut after_first_update = None;
    loop {
        match reader.next_event(&fb) {
            Ok(event) => {
                if matches!(event, ServerEvent::Update { .. }) && after_first_update.is_none() {
                    after_first_update = Some(lock(&fb).clone());
                }
                if matches!(event, ServerEvent::Resized { .. }) {
                    writer.key_event(0xFFEB, true).unwrap();
                    writer.pointer_event(1, 10, 20).unwrap();
                    writer.cut_text("hi ✓").unwrap();
                }
                events.push(event);
            }
            Err(Error::Io(e))
                if matches!(
                    e.kind(),
                    ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
                ) =>
            {
                break;
            }
            Err(e) => panic!("{e}"),
        }
    }
    let model = server.join().unwrap();

    let rect = |x, y, w, h| Rect { x, y, w, h };
    match &events[0] {
        ServerEvent::Cursor(c) => {
            assert_eq!((c.hotspot_x, c.hotspot_y, c.width, c.height), (2, 1, 4, 3));
            let alpha: Vec<u32> = c.pixels.iter().map(|p| p >> 24).collect();
            assert_eq!(alpha, [255, 255, 255, 255, 255, 0, 0, 255, 255, 255, 255, 255]);
        }
        other => panic!("expected cursor, got {other:?}"),
    }
    assert_eq!(
        events[1..],
        [
            ServerEvent::Update {
                dirty: vec![rect(0, 0, 150, 70), rect(5, 5, 10, 10), rect(100, 50, 20, 8), rect(120, 40, 16, 16)]
            },
            ServerEvent::Resized { width: 200, height: 100 },
            ServerEvent::Update { dirty: vec![rect(0, 0, 200, 100)] },
            ServerEvent::Bell,
            ServerEvent::CutText("héllo".into()),
        ]
    );
    let first = after_first_update.unwrap();
    assert_eq!((first.width, first.height), (150, 70));
    assert!(first.pixels == model.pixels, "framebuffer after update 1 differs from the model");
    let fb = lock(&fb);
    assert_eq!((fb.width, fb.height, reader.size()), (200, 100, (200, 100)));
    assert!(fb.pixels.iter().all(|&p| p == 0x0042_4242));
    let stats = reader.stats();
    assert_eq!((stats.updates, stats.rects[&16], stats.rects[&-224]), (2, 2, 1));
}

#[test]
fn continuous_updates_stop_per_frame_requests() {
    use std::time::Duration;
    let (port, server) = spawn(|mut s| {
        assert!(handshake(&mut s, b"RFB 003.889\n", &[30, 2], "tester", "pw"));
        server_init(&mut s, 64, 48); // consumes the setup messages and the first full request

        // Advertise continuous-update support; the client must enable it for the whole screen.
        s.write_all(&[150]).unwrap();
        assert_eq!(rd::<10>(&mut s), [150, 1, 0, 0, 0, 0, 0, 64, 0, 48], "EnableContinuousUpdates full screen");

        // Push an update the client never requested.
        let px: Vec<u32> = (0..4).map(|i| 0x00AB_CD00 | i).collect();
        let mut m = vec![0u8, 0];
        m.extend_from_slice(&1u16.to_be_bytes());
        rect_header(&mut m, 0, 0, 2, 2, 0);
        m.extend_from_slice(&raw_bytes(&px));
        s.write_all(&m).unwrap();

        // In continuous mode the client must not send a FramebufferUpdateRequest back.
        s.set_read_timeout(Some(Duration::from_millis(400))).unwrap();
        let mut b = [0u8; 1];
        let r = s.read(&mut b);
        assert!(
            matches!(&r, Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut),
            "expected no client request in continuous mode, got {r:?}"
        );
    });

    let options = ConnectOptions::new("127.0.0.1", port, Some("tester".into()), "pw");
    let (_info, mut reader, _writer) = connect(&options).unwrap();
    let fb = Mutex::new(Framebuffer::default());
    loop {
        match reader.next_event(&fb) {
            Ok(ServerEvent::Update { dirty }) => {
                assert_eq!(dirty, vec![Rect { x: 0, y: 0, w: 2, h: 2 }]);
                break;
            }
            Ok(_) => continue,
            Err(e) => panic!("{e}"),
        }
    }
    server.join().unwrap();
}

#[test]
fn apple_dh_wrong_password_reports_reason() {
    let (port, server) = spawn(|mut s| handshake(&mut s, b"RFB 003.889\n", &[30, 2], "tester", "right"));
    let options = ConnectOptions::new("127.0.0.1", port, Some("tester".into()), "wrong");
    match connect(&options) {
        Err(Error::AuthFailed(reason)) => assert_eq!(reason, "bad login"),
        Err(e) => panic!("unexpected error {e}"),
        Ok(_) => panic!("login should fail"),
    }
    assert!(!server.join().unwrap());
}

#[test]
fn vnc_auth_when_preferred() {
    let (port, server) = spawn(|mut s| {
        assert!(handshake(&mut s, b"RFB 003.889\n", &[30, 2], "", "secret12"));
        server_init(&mut s, 8, 8);
    });
    let mut options = ConnectOptions::new("127.0.0.1", port, Some("tester".into()), "secret12");
    options.security = vec![SecurityType::VncAuth];
    let (info, _reader, _writer) = connect(&options).unwrap();
    assert_eq!(info.security, SecurityType::VncAuth);
    server.join().unwrap();
}

#[test]
fn apple_dh_without_username_falls_back_or_errors() {
    // Offered 30 and 2, no user name: VNC auth is used.
    let (port, server) = spawn(|mut s| {
        assert!(handshake(&mut s, b"RFB 003.008\n", &[30, 2], "", "pw"));
        server_init(&mut s, 4, 4);
    });
    let (info, _r, _w) = connect(&ConnectOptions::new("127.0.0.1", port, None, "pw")).unwrap();
    assert_eq!(info.security, SecurityType::VncAuth);
    server.join().unwrap();

    // Only 30 offered: the user name is required.
    let (port, server) = spawn(|mut s| {
        s.write_all(b"RFB 003.889\n").unwrap();
        let _ = rd::<12>(&mut s);
        s.write_all(&[1, 30]).unwrap();
        let mut rest = Vec::new();
        let _ = s.read_to_end(&mut rest);
        rest
    });
    assert!(matches!(connect(&ConnectOptions::new("127.0.0.1", port, None, "pw")), Err(Error::UsernameRequired(30))));
    assert!(server.join().unwrap().is_empty(), "client must not pick a type it cannot complete");
}

#[test]
fn rfb_3_3_vnc_auth() {
    let (port, server) = spawn(|mut s| {
        assert!(handshake(&mut s, b"RFB 003.003\n", &[], "", "pw"));
        server_init(&mut s, 16, 9);
    });
    let (info, _r, _w) = connect(&ConnectOptions::new("127.0.0.1", port, Some("tester".into()), "pw")).unwrap();
    assert_eq!((info.protocol.as_str(), info.security, info.width), ("RFB 003.003", SecurityType::VncAuth, 16));
    server.join().unwrap();
}
