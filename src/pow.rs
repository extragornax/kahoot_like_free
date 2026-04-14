use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

const DIFFICULTY: usize = 4; // leading hex zeros required (4 = ~65k attempts)
const CHALLENGE_TTL_SECS: u64 = 120;
const SECRET: &[u8] = b"pow-secret-change-me-in-prod";

/// Generate a challenge string: base64(timestamp_be_bytes ++ hmac)
pub fn generate_challenge() -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let ts_bytes = ts.to_be_bytes();

    let mut mac = HmacSha256::new_from_slice(SECRET).unwrap();
    mac.update(&ts_bytes);
    let sig = mac.finalize().into_bytes();

    let mut payload = Vec::with_capacity(8 + 32);
    payload.extend_from_slice(&ts_bytes);
    payload.extend_from_slice(&sig);
    STANDARD.encode(&payload)
}

/// Verify a challenge + nonce pair
pub fn verify(challenge: &str, nonce: &str) -> bool {
    // Decode challenge
    let payload = match STANDARD.decode(challenge) {
        Ok(p) if p.len() == 40 => p, // 8 bytes timestamp + 32 bytes hmac
        _ => return false,
    };

    let ts_bytes: [u8; 8] = payload[..8].try_into().unwrap();
    let sig = &payload[8..];
    let ts = u64::from_be_bytes(ts_bytes);

    // Check timestamp freshness
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    if now.saturating_sub(ts) > CHALLENGE_TTL_SECS {
        return false;
    }

    // Verify HMAC
    let mut mac = HmacSha256::new_from_slice(SECRET).unwrap();
    mac.update(&ts_bytes);
    if mac.verify_slice(sig).is_err() {
        return false;
    }

    // Verify proof-of-work: sha256(challenge + ":" + nonce) must have DIFFICULTY leading hex zeros
    let input = format!("{}:{}", challenge, nonce);
    let hash = Sha256::digest(input.as_bytes());
    let hex_hash = hex::encode(hash);
    hex_hash.starts_with(&"0".repeat(DIFFICULTY))
}
