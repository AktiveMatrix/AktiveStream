//! Security type handshakes: Apple Diffie-Hellman (30) and classic VNC auth (2).

use std::io::{Read, Write};

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockEncrypt, KeyInit};
use des::Des;
use md5::{Digest, Md5};
use num_bigint::BigUint;

use crate::wire::{read_into, read_u16};
use crate::{Error, Result};

pub(crate) fn random_bytes(len: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    getrandom::fill(&mut buf).map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
    Ok(buf)
}

/// Big-endian bytes of `n`, left-padded to `len`.
pub(crate) fn to_fixed(n: &BigUint, len: usize) -> Vec<u8> {
    let bytes = n.to_bytes_be();
    let mut out = vec![0u8; len.saturating_sub(bytes.len())];
    out.extend_from_slice(&bytes[bytes.len().saturating_sub(len)..]);
    out
}

/// A 64-byte credential field: UTF-8, NUL-terminated, random padding. Longer values are
/// cut to 63 bytes on a character boundary.
pub(crate) fn credential_field(value: &str) -> Result<[u8; 64]> {
    let mut end = value.len().min(63);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut field = [0u8; 64];
    field[..end].copy_from_slice(&value.as_bytes()[..end]);
    let pad = random_bytes(64 - end - 1)?;
    field[end + 1..].copy_from_slice(&pad);
    Ok(field)
}

/// The derived AES key for Apple DH: MD5 of the shared secret at the server's key length.
pub(crate) fn apple_dh_key(shared: &BigUint, key_len: usize) -> [u8; 16] {
    Md5::digest(to_fixed(shared, key_len)).into()
}

/// Client side of security type 30. The SecurityResult is read by the caller.
pub(crate) fn apple_dh<S: Read + Write>(stream: &mut S, username: &str, password: &str) -> Result<()> {
    let generator = read_u16(stream)?;
    let key_len = read_u16(stream)? as usize;
    if key_len == 0 || key_len > 1024 {
        return Err(Error::Malformed(format!("Apple DH key length {key_len}")));
    }
    let mut buf = Vec::new();
    read_into(stream, &mut buf, key_len)?;
    let prime = BigUint::from_bytes_be(&buf);
    read_into(stream, &mut buf, key_len)?;
    let server_public = BigUint::from_bytes_be(&buf);

    let three = BigUint::from(3u8);
    if prime < three {
        return Err(Error::Malformed("Apple DH prime too small".into()));
    }
    let span = &prime - BigUint::from(2u8);
    let private = BigUint::from_bytes_be(&random_bytes(key_len)?) % &span + BigUint::from(1u8);
    let public = BigUint::from(generator).modpow(&private, &prime);
    let shared = server_public.modpow(&private, &prime);
    let key = apple_dh_key(&shared, key_len);

    let mut creds = [0u8; 128];
    creds[..64].copy_from_slice(&credential_field(username)?);
    creds[64..].copy_from_slice(&credential_field(password)?);
    // The protocol fixes AES-128 in ECB mode over the two credential fields.
    let cipher = Aes128::new(GenericArray::from_slice(&key));
    for block in creds.as_chunks_mut::<16>().0 {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }

    let mut reply = Vec::with_capacity(128 + key_len);
    reply.extend_from_slice(&creds);
    reply.extend_from_slice(&to_fixed(&public, key_len));
    stream.write_all(&reply)?;
    Ok(())
}

/// DES response to a VNC auth challenge. The key is the password's first 8 bytes,
/// NUL-padded, with each byte's bits reversed.
pub(crate) fn vnc_response(challenge: &[u8; 16], password: &str) -> [u8; 16] {
    let mut key = [0u8; 8];
    for (k, &b) in key.iter_mut().zip(password.as_bytes()) {
        *k = b.reverse_bits();
    }
    let cipher = Des::new(GenericArray::from_slice(&key));
    let mut out = *challenge;
    for block in out.as_chunks_mut::<8>().0 {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    out
}

/// Client side of security type 2. The SecurityResult is read by the caller.
pub(crate) fn vnc_auth<S: Read + Write>(stream: &mut S, password: &str) -> Result<()> {
    let mut challenge = [0u8; 16];
    stream.read_exact(&mut challenge)?;
    stream.write_all(&vnc_response(&challenge, password))?;
    Ok(())
}
