use num_bigint::BigUint;

use crate::auth::{credential_field, to_fixed, vnc_response};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Expected values computed independently with Python `cryptography` (single DES as
/// 3DES with the key repeated), challenge bytes 0..16, key bits reversed.
#[test]
fn vnc_auth_known_answers() {
    let challenge: [u8; 16] = core::array::from_fn(|i| i as u8);
    assert_eq!(hex(&vnc_response(&challenge, "secret12")), "adcd997f8e16fee575e973f93c2b62b4");
    assert_eq!(hex(&vnc_response(&challenge, "pw")), "858600d9af143c9e6541d3dd92a835d0");
    assert_eq!(hex(&vnc_response(&challenge, "longerthan8chars")), "76543820b2c358fe359083c192fa77c9");
}

#[test]
fn credential_fields_are_nul_terminated_and_cut_on_char_boundaries() {
    let f = credential_field("tester").unwrap();
    assert_eq!(&f[..7], b"tester\0");
    // 62 ASCII bytes then a 2-byte character: it would end at byte 64, so it is dropped.
    let long = format!("{}é", "a".repeat(62));
    let f = credential_field(&long).unwrap();
    assert_eq!(&f[..63], format!("{}\0", "a".repeat(62)).as_bytes());
}

#[test]
fn fixed_width_big_endian() {
    assert_eq!(to_fixed(&BigUint::from(0x0102u16), 4), vec![0, 0, 1, 2]);
    assert_eq!(to_fixed(&BigUint::from(0u8), 2), vec![0, 0]);
}
