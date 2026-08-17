use compact_str::{format_compact, CompactString};
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::db::now_unix;
use crate::error::{AppError, Result};

type HmacSha256 = Hmac<Sha256>;

pub fn sign_card(user_id: u64, issued_at: i64, secret: &str) -> CompactString {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(format_compact!("{user_id}:{issued_at}").as_bytes());
    CompactString::from(hex::encode(mac.finalize().into_bytes()))
}

pub fn card_path(public_base_url: &str, user_id: u64, secret: &str) -> CompactString {
    let issued_at = now_unix();
    let sig = sign_card(user_id, issued_at, secret);
    format_compact!("{public_base_url}/card/{user_id}.jpg?t={issued_at}&sig={sig}")
}

pub fn verify_card(
    user_id: u64,
    issued_at: i64,
    sig: &str,
    secret: &str,
    ttl_secs: i64,
) -> Result<()> {
    if issued_at <= 0 || now_unix() - issued_at > ttl_secs {
        return Err(AppError::InvalidCardUrl);
    }
    let expected = sign_card(user_id, issued_at, secret);
    if expected.len() != sig.len() {
        return Err(AppError::InvalidCardUrl);
    }
    let mut mismatch = 0u8;
    for (a, b) in expected.bytes().zip(sig.bytes()) {
        mismatch |= a ^ b;
    }
    if mismatch == 0 {
        Ok(())
    } else {
        Err(AppError::InvalidCardUrl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_fresh_signature_and_rejects_bad_ones() {
        let secret = "s3cret";
        let issued_at = now_unix();
        let sig = sign_card(99, issued_at, secret);
        verify_card(99, issued_at, &sig, secret, 300).unwrap();
        assert!(verify_card(98, issued_at, &sig, secret, 300).is_err());
        assert!(verify_card(99, issued_at, "deadbeef", secret, 300).is_err());
        assert!(verify_card(99, issued_at - 10_000, &sig, secret, 300).is_err());
    }
}
