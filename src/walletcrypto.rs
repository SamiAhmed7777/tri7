//! Legacy wallet crypto (CryptoPP/OpenSSL compatible), needed before tri7 can
//! read or write an encrypted wallet at all — today `read_wallet` refuses any
//! file carrying `mkey`/`ckey`.
//!
//! Legacy sources (`crypter.cpp`, `wallet.cpp:EncryptWallet`):
//!   * master key = 32 random bytes
//!   * salt = 8 random bytes, derivation method 0 = EVP_BytesToKey
//!     (AES-256-CBC, SHA-512, `nDeriveIterations` rounds; legacy auto-tunes to
//!     ~2.5 s of work with a 25000 floor)
//!   * derived (key, iv) = EVP_BytesToKey(passphrase, salt)
//!   * mkey.encrypted_key = AES-256-CBC(plaintext = master key)
//!   * ckey.vchCiphertext = AES-256-CBC(plaintext = secret || checksum),
//!     key = master key, iv = ckey.nIV
//!   * checksum = first 4 bytes of SHA256d(secret)
//!
//! Everything here is byte-exact with OpenSSL so a wallet encrypted by the
//! legacy client opens here and vice versa.

use sha2::{Digest, Sha256, Sha512};

/// Legacy WALLET_CRYPTO_KEY_SIZE / SALT_SIZE.
pub const KEY_SIZE: usize = 32;
pub const SALT_SIZE: usize = 8;
/// Legacy default iteration floor (wallet.cpp:379).
pub const MIN_ITERATIONS: u32 = 25_000;

/// OpenSSL EVP_BytesToKey with SHA-512 (what legacy calls with nRounds = 1
/// digest step unless `nRounds` is given; legacy passes `nRounds` as the
/// *iteration* count, i.e. bytes = digest chain repeated).
pub fn evp_bytes_to_key_sha512(
    passphrase: &[u8],
    salt: &[u8],
    rounds: u32,
) -> ([u8; KEY_SIZE], [u8; 16]) {
    let mut key_iv = Vec::with_capacity(48);
    let mut prev: Vec<u8> = Vec::new();
    // EVP_BytesToKey: D_i = HASH^count(D_{i-1} || data || salt)
    while key_iv.len() < KEY_SIZE + 16 {
        let mut h = Sha512::new();
        h.update(&prev);
        h.update(passphrase);
        h.update(salt);
        let mut digest = h.finalize().to_vec();
        // OpenSSL's count parameter re-hashes the previous digest
        for _ in 1..rounds.max(1) {
            let mut h2 = Sha512::new();
            h2.update(&digest);
            digest = h2.finalize().to_vec();
        }
        // OpenSSL appends the WHOLE digest; 32+16 bytes come out of the
        // first SHA-512 block (64 bytes) for the usual 48-byte request.
        key_iv.extend_from_slice(&digest);
        prev = digest;
    }
    let mut k = [0u8; KEY_SIZE];
    k.copy_from_slice(&key_iv[..KEY_SIZE]);
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&key_iv[KEY_SIZE..KEY_SIZE + 16]);
    (k, iv)
}

pub fn sha256d(data: &[u8]) -> [u8; 32] {
    let a = Sha256::digest(data);
    let b = Sha256::digest(a);
    let mut out = [0u8; 32];
    out.copy_from_slice(&b);
    out
}

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};

type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

/// Legacy CCrypter::Encrypt — AES-256-CBC + PKCS#7.
pub fn encrypt(key: &[u8; KEY_SIZE], iv: &[u8; 16], plaintext: &[u8]) -> Vec<u8> {
    Aes256CbcEnc::new(key.into(), iv.into()).encrypt_padded_vec_mut::<Pkcs7>(plaintext)
}

/// Legacy CCrypter::Decrypt.
pub fn decrypt(key: &[u8; KEY_SIZE], iv: &[u8; 16], ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    Aes256CbcDec::new(key.into(), iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
        .map_err(|e| format!("decrypt: {e}"))
}

/// Encrypt a 32-byte secret the way legacy `EncryptSecret` does: payload is
/// the secret followed by its 4-byte checksum, AES-256-CBC under the master
/// key with the ckey's own IV.
pub fn encrypt_secret(master: &[u8; KEY_SIZE], iv: &[u8; 16], secret: &[u8; 32]) -> Vec<u8> {
    let mut plain = Vec::with_capacity(36);
    plain.extend_from_slice(secret);
    plain.extend_from_slice(&sha256d(secret)[..4]);
    encrypt(master, iv, &plain)
}

/// Decrypt a ckey payload and verify the trailing checksum. A wrong master key
/// (wrong passphrase) fails here, not by returning garbage silently.
pub fn decrypt_secret(
    master: &[u8; KEY_SIZE],
    iv: &[u8; 16],
    ciphertext: &[u8],
) -> Result<[u8; 32], String> {
    let plain = decrypt(master, iv, ciphertext)?;
    if plain.len() != 36 {
        return Err(format!(
            "decrypt_secret: plaintext is {} bytes, expected 36 (secret+checksum)",
            plain.len()
        ));
    }
    let mut secret = [0u8; 32];
    secret.copy_from_slice(&plain[..32]);
    if &plain[32..36] != &sha256d(&secret)[..4] {
        return Err("decrypt_secret: checksum mismatch (wrong passphrase?)".into());
    }
    Ok(secret)
}

/// Derive the (key, iv) pair for a passphrase/salt, as `SetKeyFromPassphrase`
/// does for derivation method 0.
pub fn derive_key_iv(passphrase: &str, salt: &[u8], iterations: u32) -> ([u8; KEY_SIZE], [u8; 16]) {
    evp_bytes_to_key_sha512(passphrase.as_bytes(), salt, iterations)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vector generated independently with Python `cryptography`/hashlib for
    /// EVP_BytesToKey-equivalent derivation (SHA-512, 1 round).
    #[test]
    fn kdf_matches_reference_vector() {
        let (k, iv) = derive_key_iv("correct horse battery staple", b"12345678", 1);
        assert_eq!(
            k.iter().map(|b| format!("{:02x}", b)).collect::<String>(),
            "081a7c803ed83b65d3f15a938dce369ee69804fe331b38e7d2003223906b09fb"
        );
        assert_eq!(
            iv.iter().map(|b| format!("{:02x}", b)).collect::<String>(),
            "0b49f6ae973bee6fa3258c75b43c288d"
        );
    }

    /// Ciphertext produced independently by Python `cryptography` (OpenSSL)
    /// with key = 00..1f, iv = 00..0f over the same plaintext. If this matches,
    /// a wallet encrypted by OpenSSL and one encrypted here are interchangeable.
    #[test]
    fn aes_matches_openssl_vector() {
        let key: [u8; 32] = core::array::from_fn(|i| i as u8);
        let iv: [u8; 16] = core::array::from_fn(|i| i as u8);
        let pt = b"triangles-wallet-crypto-test-0123";
        let ct = encrypt(&key, &iv, pt);
        assert_eq!(
            ct.iter().map(|b| format!("{:02x}", b)).collect::<String>(),
            "d1fcfaded90538b79a15eb37481d17433e56ac516b6c30dfeba4687bb15d97dfc34dfeb17ef8905d1d93eb8c4dc31787"
        );
        assert_eq!(decrypt(&key, &iv, &ct).unwrap(), pt.to_vec());
    }

    #[test]
    fn aes_roundtrip_and_checksum() {
        let master = [7u8; 32];
        let iv = [9u8; 16];
        let secret = [3u8; 32];
        let ct = encrypt_secret(&master, &iv, &secret);
        assert_eq!(ct.len(), 48, "36 bytes + PKCS#7 padding to 48");
        assert_eq!(decrypt_secret(&master, &iv, &ct).unwrap(), secret);
        // wrong master key must fail, not return garbage
        let bad = [8u8; 32];
        assert!(decrypt_secret(&bad, &iv, &ct).is_err());
    }
}
