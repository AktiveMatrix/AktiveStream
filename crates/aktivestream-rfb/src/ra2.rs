//! RealVNC's RSA-AES security type (RA2, type 5). Both sides exchange ephemeral RSA public keys,
//! swap a 16-byte random each (RSA-PKCS#1 v1.5 encrypted to the other's key), derive AES-128 keys
//! from the randoms, verify a transcript hash, and exchange credentials. From the credentials on,
//! every message is AES-128-EAX framed as `u16 big-endian length ‖ ciphertext ‖ 16-byte tag`, the
//! length doubling as the EAX associated data and a 16-byte little-endian counter as the nonce.
//! See the "RSA-AES Security Type" section of the community RFB protocol document.

use std::io::{self, Read, Write};

use aes::Aes128;
use eax::Eax;
use eax::aead::generic_array::GenericArray;
use eax::aead::{Aead, KeyInit, Payload};
use rand::rngs::OsRng;
use rsa::traits::PublicKeyParts;
use rsa::{BigUint, Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use sha1::{Digest, Sha1};

use crate::auth::random_bytes;
use crate::{Error, Result};

type Eax128 = Eax<Aes128>;

fn rsa_err(e: impl std::fmt::Display) -> Error {
    Error::Malformed(format!("RA2 RSA: {e}"))
}

/// Big-endian `bytes` left-padded (or truncated) to exactly `len` bytes.
fn left_pad(bytes: Vec<u8>, len: usize) -> Vec<u8> {
    if bytes.len() >= len {
        bytes[bytes.len() - len..].to_vec()
    } else {
        let mut out = vec![0u8; len - bytes.len()];
        out.extend_from_slice(&bytes);
        out
    }
}

fn sha1_of(parts: &[&[u8]]) -> [u8; 20] {
    let mut h = Sha1::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// One direction's AES-128-EAX stream cipher with a 16-byte little-endian message counter.
pub(crate) struct Ra2Cipher {
    cipher: Eax128,
    counter: u128,
}

impl Ra2Cipher {
    fn new(key: [u8; 16]) -> Ra2Cipher {
        Ra2Cipher { cipher: Eax128::new(GenericArray::from_slice(&key)), counter: 0 }
    }

    /// Frames `plaintext` as `u16 len ‖ ciphertext ‖ tag`, the length as EAX associated data.
    fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let len = u16::try_from(plaintext.len()).map_err(|_| Error::Malformed("RA2 frame too long".into()))?;
        let hdr = len.to_be_bytes();
        let nonce = self.counter.to_le_bytes();
        let ct = self
            .cipher
            .encrypt(GenericArray::from_slice(&nonce), Payload { msg: plaintext, aad: &hdr })
            .map_err(|_| Error::Malformed("RA2 encrypt".into()))?;
        self.counter += 1;
        let mut frame = Vec::with_capacity(2 + ct.len());
        frame.extend_from_slice(&hdr);
        frame.extend_from_slice(&ct);
        Ok(frame)
    }

    /// Reads and decrypts one frame from `r`.
    fn open(&mut self, r: &mut impl Read) -> Result<Vec<u8>> {
        let mut hdr = [0u8; 2];
        r.read_exact(&mut hdr)?;
        let len = u16::from_be_bytes(hdr) as usize;
        let mut body = vec![0u8; len + 16]; // ciphertext + 16-byte tag
        r.read_exact(&mut body)?;
        let nonce = self.counter.to_le_bytes();
        let pt = self
            .cipher
            .decrypt(GenericArray::from_slice(&nonce), Payload { msg: &body, aad: &hdr })
            .map_err(|_| Error::AuthFailed("RA2 message authentication failed".into()))?;
        self.counter += 1;
        Ok(pt)
    }
}

/// Runs the RA2 handshake over the raw stream and returns the `(client->server, server->client)`
/// ciphers, positioned to continue framing the rest of the session.
pub(crate) fn handshake<S: Read + Write>(
    stream: &mut S,
    username: &str,
    password: &str,
) -> Result<(Ra2Cipher, Ra2Cipher)> {
    // 1. Server public key: u32 bit-length (BE), then N and E, each ceil(bits/8) bytes.
    let mut bits_be = [0u8; 4];
    stream.read_exact(&mut bits_be)?;
    let bits = u32::from_be_bytes(bits_be) as usize;
    if !(1024..=8192).contains(&bits) {
        return Err(Error::Malformed(format!("RA2 server key length {bits} bits")));
    }
    let klen = bits.div_ceil(8);
    let mut n_bytes = vec![0u8; klen];
    stream.read_exact(&mut n_bytes)?;
    let mut e_bytes = vec![0u8; klen];
    stream.read_exact(&mut e_bytes)?;
    let mut server_blob = Vec::with_capacity(4 + 2 * klen);
    server_blob.extend_from_slice(&bits_be);
    server_blob.extend_from_slice(&n_bytes);
    server_blob.extend_from_slice(&e_bytes);
    let server_pub =
        RsaPublicKey::new(BigUint::from_bytes_be(&n_bytes), BigUint::from_bytes_be(&e_bytes)).map_err(rsa_err)?;

    // 2. Our ephemeral 2048-bit key; send u32 length (2048) + N (256) + E (256, left-padded).
    let client_priv = RsaPrivateKey::new(&mut OsRng, 2048).map_err(rsa_err)?;
    let client_pub = RsaPublicKey::from(&client_priv);
    let mut client_blob = Vec::with_capacity(4 + 512);
    client_blob.extend_from_slice(&2048u32.to_be_bytes());
    client_blob.extend_from_slice(&left_pad(client_pub.n().to_bytes_be(), 256));
    client_blob.extend_from_slice(&left_pad(client_pub.e().to_bytes_be(), 256));
    stream.write_all(&client_blob)?;

    // 3. Our 16-byte random, RSA-PKCS#1 v1.5 to the server key: u16 length + ciphertext.
    let client_random = random_bytes(16)?;
    let enc = server_pub.encrypt(&mut OsRng, Pkcs1v15Encrypt, &client_random).map_err(rsa_err)?;
    stream.write_all(&(enc.len() as u16).to_be_bytes())?;
    stream.write_all(&enc)?;

    // 4. Server's 16-byte random: u16 length + ciphertext to our key; decrypt it.
    let mut slen_be = [0u8; 2];
    stream.read_exact(&mut slen_be)?;
    let slen = u16::from_be_bytes(slen_be) as usize;
    let mut server_ct = vec![0u8; slen];
    stream.read_exact(&mut server_ct)?;
    let server_random = client_priv.decrypt(Pkcs1v15Encrypt, &server_ct).map_err(rsa_err)?;
    if server_random.len() != 16 {
        return Err(Error::Malformed("RA2 server random length".into()));
    }

    // 5. Session keys: first 16 bytes of SHA-1 over the two randoms, ordered by direction.
    let mut c2s_key = [0u8; 16];
    c2s_key.copy_from_slice(&sha1_of(&[&server_random, &client_random])[..16]);
    let mut s2c_key = [0u8; 16];
    s2c_key.copy_from_slice(&sha1_of(&[&client_random, &server_random])[..16]);
    let mut c2s = Ra2Cipher::new(c2s_key);
    let mut s2c = Ra2Cipher::new(s2c_key);

    // 6. Transcript hashes (now framed): send SHA1(clientBlob‖serverBlob), verify the server's reverse.
    let client_hash = sha1_of(&[&client_blob, &server_blob]);
    let frame = c2s.seal(&client_hash)?;
    stream.write_all(&frame)?;
    let server_hash = s2c.open(stream)?;
    if server_hash != sha1_of(&[&server_blob, &client_blob]) {
        return Err(Error::AuthFailed("RA2 server key verification failed".into()));
    }

    // 7. Subtype: 1 = username + password, 2 = password only.
    let subtype = s2c.open(stream)?;
    let subtype = *subtype.first().ok_or_else(|| Error::Malformed("RA2 subtype".into()))?;

    // 8. Credentials: [u8 len][username][u8 len][password]; username empty for subtype 2.
    let user = if subtype == 2 { "" } else { username };
    let ub = &user.as_bytes()[..user.len().min(255)];
    let pb = &password.as_bytes()[..password.len().min(255)];
    let mut creds = Vec::with_capacity(2 + ub.len() + pb.len());
    creds.push(ub.len() as u8);
    creds.extend_from_slice(ub);
    creds.push(pb.len() as u8);
    creds.extend_from_slice(pb);
    let frame = c2s.seal(&creds)?;
    stream.write_all(&frame)?;

    Ok((c2s, s2c))
}

/// Read adapter: decrypts RA2 frames from the inner reader into a plaintext byte stream.
pub(crate) struct Ra2Reader<R: Read> {
    inner: R,
    cipher: Ra2Cipher,
    plain: Vec<u8>,
    pos: usize,
}

impl<R: Read> Ra2Reader<R> {
    pub(crate) fn new(inner: R, cipher: Ra2Cipher) -> Ra2Reader<R> {
        Ra2Reader { inner, cipher, plain: Vec::new(), pos: 0 }
    }
}

impl<R: Read> Read for Ra2Reader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.plain.len() {
            self.plain = self.cipher.open(&mut self.inner).map_err(io::Error::other)?;
            self.pos = 0;
        }
        let n = (self.plain.len() - self.pos).min(out.len());
        out[..n].copy_from_slice(&self.plain[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Write adapter: frames and encrypts writes as RA2 frames onto the inner writer.
pub(crate) struct Ra2Writer<W: Write> {
    inner: W,
    cipher: Ra2Cipher,
}

impl<W: Write> Ra2Writer<W> {
    pub(crate) fn new(inner: W, cipher: Ra2Cipher) -> Ra2Writer<W> {
        Ra2Writer { inner, cipher }
    }

    pub(crate) fn get_ref(&self) -> &W {
        &self.inner
    }
}

impl<W: Write> Write for Ra2Writer<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        // One frame per call, chunked well under the u16 length limit.
        let chunk = data.len().min(8192);
        let frame = self.cipher.seal(&data[..chunk]).map_err(io::Error::other)?;
        self.inner.write_all(&frame)?;
        Ok(chunk)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
