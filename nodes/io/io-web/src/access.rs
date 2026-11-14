//! An exposure's one-time link and session cookie (docs/io-web-v2-stage-2-1.md §3): their formats,
//! their MACs and their times.
//!
//! Both are signed with keys derived from the enablement's secret `K`, which SY.admin puts in the
//! vault and io.web alone reads (§3.1). Neither is kept in a table: a link's single use is the
//! redeemed set's (`exposures`), and a session's state is the cookie itself, so sessions survive a
//! restart of io.web. Every tag is compared in constant time (`Mac::verify_slice`).

use std::fmt;

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// How long a link may be redeemed after io.web minted it, in seconds (§3.2).
pub const LINK_LIFETIME_S: u64 = 120;
/// A session ends after this long without a request, in seconds (§3.3).
pub const SESSION_IDLE_S: u64 = 30 * 60;
/// Once a session's last activity is this old, an answer carries its cookie again (§3.3).
pub const SESSION_SLIDE_S: u64 = 60;
/// The session cookie. `__Host-` makes the browser keep it for this host alone, `Secure`, on `/`.
pub const SESSION_COOKIE: &str = "__Host-fluxbee_session";

/// The labels that derive the link's and the session's keys from `K` (§3.1). Two keys, so a link
/// never verifies as a cookie, nor a cookie as a link.
const LINK_KEY_LABEL: &[u8] = b"fluxbee io.web link v1";
const SESSION_KEY_LABEL: &[u8] = b"fluxbee io.web session v1";

const LINK_FORMAT: &str = "L1";
const SESSION_FORMAT: &str = "S1";

/// An enablement's two keys, derived once from its secret `K`, which is not kept (§3.1). Only the
/// tests compare two of them: io.web compares tags, in constant time, never keys.
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct EnablementKeys {
    link: [u8; 32],
    session: [u8; 32],
}

impl EnablementKeys {
    /// `K_link = HMAC-SHA256(K, "fluxbee io.web link v1")` and `K_sess = HMAC-SHA256(K, "fluxbee
    /// io.web session v1")`.
    pub fn derive(secret: &[u8; 32]) -> Self {
        Self {
            link: mac(secret, LINK_KEY_LABEL),
            session: mac(secret, SESSION_KEY_LABEL),
        }
    }
}

/// Never the keys.
impl fmt::Debug for EnablementKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EnablementKeys(..)")
    }
}

fn hmac_over(key: &[u8], message: &[u8]) -> HmacSha256 {
    // HMAC takes a key of any length, so this never fails.
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(message);
    mac
}

fn mac(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut tag = [0u8; 32];
    tag.copy_from_slice(&hmac_over(key, message).finalize().into_bytes());
    tag
}

/// Whether `tag` is the MAC of `message` under `key`, compared in constant time.
fn verifies(key: &[u8], message: &[u8], tag: &[u8; 32]) -> bool {
    hmac_over(key, message).verify_slice(tag).is_ok()
}

/// A link's code (§3.2), `L1.<x>.<e>.<jti>.<iat>.<tag>`: `x` and `e` the exposure's and the
/// enablement's uuids in 32 hex, `jti` 16 random bytes in hex, `iat` when io.web minted it in Unix
/// seconds, and `tag` the MAC of the host and the rest under `K_link`. The host is in the MAC and
/// not in the code, so a code redeemed on another host fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkCode<'a> {
    pub x: &'a str,
    pub e: &'a str,
    pub jti: &'a str,
    pub iat: u64,
    tag: [u8; 32],
}

impl<'a> LinkCode<'a> {
    /// The code for `host`, signed with `keys`.
    pub fn mint(
        keys: &EnablementKeys,
        host: &str,
        x: &str,
        e: &str,
        jti: &str,
        iat: u64,
    ) -> String {
        let tag = mac(&keys.link, link_message(host, x, e, jti, iat).as_bytes());
        format!("{LINK_FORMAT}.{x}.{e}.{jti}.{iat}.{}", hex(&tag))
    }

    /// The code, read strictly: its format, three fields of 32 lowercase hex, decimal seconds and
    /// a tag of 64 lowercase hex, and nothing more.
    pub fn parse(code: &'a str) -> Option<Self> {
        let mut parts = code.split('.');
        let (Some(LINK_FORMAT), Some(x), Some(e), Some(jti), Some(iat), Some(tag), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return None;
        };
        if !(is_lower_hex(x, 32) && is_lower_hex(e, 32) && is_lower_hex(jti, 32)) {
            return None;
        }
        Some(Self {
            x,
            e,
            jti,
            iat: parse_seconds(iat)?,
            tag: lower_hex_32(tag)?,
        })
    }

    /// Whether its tag is the MAC of `host` and its fields under `keys`.
    pub fn verifies(&self, keys: &EnablementKeys, host: &str) -> bool {
        let message = link_message(host, self.x, self.e, self.jti, self.iat);
        verifies(&keys.link, message.as_bytes(), &self.tag)
    }
}

fn link_message(host: &str, x: &str, e: &str, jti: &str, iat: u64) -> String {
    format!("{LINK_FORMAT}|{host}|{x}|{e}|{jti}|{iat}")
}

/// A session cookie's value (§3.3), `S1.<x>.<e>.<sid>.<issued>.<active>.<tag>`: the exposure's
/// and the enablement's uuids in 32 hex, the session's id (16 random bytes in hex), when it was
/// minted and when it was last carried again, in Unix seconds, and the MAC of the host and the
/// rest under `K_sess`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionToken<'a> {
    pub x: &'a str,
    pub e: &'a str,
    pub sid: &'a str,
    pub issued: u64,
    pub active: u64,
    tag: [u8; 32],
}

impl<'a> SessionToken<'a> {
    /// The cookie's value for `host`, signed with `keys`.
    pub fn mint(
        keys: &EnablementKeys,
        host: &str,
        x: &str,
        e: &str,
        sid: &str,
        issued: u64,
        active: u64,
    ) -> String {
        let message = session_message(host, x, e, sid, issued, active);
        let tag = mac(&keys.session, message.as_bytes());
        format!(
            "{SESSION_FORMAT}.{x}.{e}.{sid}.{issued}.{active}.{}",
            hex(&tag)
        )
    }

    /// The value, read strictly, as `LinkCode::parse` reads a code.
    pub fn parse(value: &'a str) -> Option<Self> {
        let mut parts = value.split('.');
        let (Some(SESSION_FORMAT), Some(x), Some(e), Some(sid), Some(issued), Some(active)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return None;
        };
        let (Some(tag), None) = (parts.next(), parts.next()) else {
            return None;
        };
        if !(is_lower_hex(x, 32) && is_lower_hex(e, 32) && is_lower_hex(sid, 32)) {
            return None;
        }
        Some(Self {
            x,
            e,
            sid,
            issued: parse_seconds(issued)?,
            active: parse_seconds(active)?,
            tag: lower_hex_32(tag)?,
        })
    }

    /// Whether its tag is the MAC of `host` and its fields under `keys`.
    pub fn verifies(&self, keys: &EnablementKeys, host: &str) -> bool {
        let message = session_message(host, self.x, self.e, self.sid, self.issued, self.active);
        verifies(&keys.session, message.as_bytes(), &self.tag)
    }
}

fn session_message(host: &str, x: &str, e: &str, sid: &str, issued: u64, active: u64) -> String {
    format!("{SESSION_FORMAT}|{host}|{x}|{e}|{sid}|{issued}|{active}")
}

/// The `Set-Cookie` value of a session (§3.3): host-only (no `Domain`, as `__Host-` requires), on
/// `/`, `Secure`, out of scripts' reach and sent only on the exposure's own navigations.
pub fn session_set_cookie(value: &str, max_age: u64) -> String {
    format!(
        "{SESSION_COOKIE}={value}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={max_age}"
    )
}

/// The `Set-Cookie` value that clears a session cookie that is not valid.
pub fn session_clear_cookie() -> String {
    session_set_cookie("", 0)
}

/// What the logs name a link by (§3.2): the first 16 hex of the SHA-256 of its `jti`, as the code
/// writes it. Never the code.
pub fn link_id(jti: &str) -> String {
    let mut id = hex(&Sha256::digest(jti.as_bytes()));
    id.truncate(16);
    id
}

/// A session's id as the logs show it: its first 8 hex.
pub fn sid_prefix(sid: &str) -> &str {
    sid.get(..8).unwrap_or(sid)
}

/// 16 random bytes in 32 lowercase hex: a link's `jti`, a session's `sid`. From a v4 uuid, whose
/// 122 random bits come from the operating system's generator (§5.5).
pub fn fresh_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// `bytes` in lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

/// The 32 bytes of a vault secret written as 64 hex digits, in either case (§3.1).
pub fn secret_from_hex(text: &str) -> Option<[u8; 32]> {
    hex_32(text.as_bytes())
}

/// `text` is exactly `len` lowercase hex digits.
fn is_lower_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 64 lowercase hex digits as 32 bytes.
fn lower_hex_32(text: &str) -> Option<[u8; 32]> {
    if !is_lower_hex(text, 64) {
        return None;
    }
    hex_32(text.as_bytes())
}

/// 64 hex digits, in either case, as 32 bytes.
fn hex_32(text: &[u8]) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let digit = |b: u8| char::from(b).to_digit(16);
    let mut bytes = [0u8; 32];
    for (byte, pair) in bytes.iter_mut().zip(text.chunks_exact(2)) {
        let value = (digit(pair[0])? << 4) | digit(pair[1])?;
        *byte = u8::try_from(value).ok()?;
    }
    Some(bytes)
}

/// Unix seconds as the codes write them: decimal digits only, no sign and no leading zero.
fn parse_seconds(text: &str) -> Option<u64> {
    let canonical = !text.is_empty()
        && text.len() <= 20
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));
    if !canonical {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "shop.hive-k3m9x7q2.fluxbee.ai";
    const X: &str = "6f1c2a3b4d5e4f608a7b9c0d1e2f3a4b";
    const E: &str = "0a1b2c3d4e5f4a6b8c7d8e9fa0b1c2d3";
    const JTI: &str = "00112233445566778899aabbccddeeff";

    fn keys(fill: u8) -> EnablementKeys {
        EnablementKeys::derive(&[fill; 32])
    }

    /// The domain separation of §3.1: the two keys differ, and so a link's tag is never a
    /// cookie's over the same text.
    #[test]
    fn the_link_and_the_session_keys_are_separate() {
        let keys = keys(7);
        assert_ne!(keys.link, keys.session);
        assert_eq!(keys, EnablementKeys::derive(&[7; 32]));
        assert_ne!(keys, EnablementKeys::derive(&[8; 32]));
        assert_eq!(format!("{keys:?}"), "EnablementKeys(..)");
    }

    #[test]
    fn a_link_code_reads_back_and_verifies_only_on_its_host_with_its_keys() {
        let code = LinkCode::mint(&keys(1), HOST, X, E, JTI, 1_763_121_700);
        assert!(code.starts_with(&format!("L1.{X}.{E}.{JTI}.1763121700.")));
        let parsed = LinkCode::parse(&code).expect("a minted code parses");
        assert_eq!(
            (parsed.x, parsed.e, parsed.jti, parsed.iat),
            (X, E, JTI, 1_763_121_700)
        );
        assert!(parsed.verifies(&keys(1), HOST));
        assert!(!parsed.verifies(&keys(2), HOST));
        assert!(!parsed.verifies(&keys(1), "erp.hive-k3m9x7q2.fluxbee.ai"));
        // A session's value over the same fields is not a link.
        let session = SessionToken::mint(&keys(1), HOST, X, E, JTI, 1, 1);
        assert!(LinkCode::parse(&session).is_none());
    }

    #[test]
    fn a_link_code_parses_strictly() {
        let code = LinkCode::mint(&keys(1), HOST, X, E, JTI, 1_763_121_700);
        let tag = code.rsplit('.').next().unwrap().to_string();
        for bad in [
            String::new(),
            format!("L2.{X}.{E}.{JTI}.1763121700.{tag}"),
            format!("l1.{X}.{E}.{JTI}.1763121700.{tag}"),
            format!("L1.{}.{E}.{JTI}.1763121700.{tag}", X.to_uppercase()),
            format!("L1.{X}.{E}.{JTI}.01763121700.{tag}"),
            format!("L1.{X}.{E}.{JTI}.+1763121700.{tag}"),
            format!("L1.{X}.{E}.{JTI}.99999999999999999999.{tag}"),
            format!("L1.{X}.{E}.{JTI}.1763121700.{}", tag.to_uppercase()),
            format!("L1.{X}.{E}.{JTI}.1763121700.{}", &tag[..63]),
            format!("L1.{X}.{E}.{JTI}.1763121700.{tag}."),
            format!("L1.{X}.{E}.{JTI}.1763121700"),
            format!("L1.{X}.{E}.{}.1763121700.{tag}", &JTI[..31]),
            format!("L1.6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b.{E}.{JTI}.1763121700.{tag}"),
            format!(" {code}"),
        ] {
            assert!(LinkCode::parse(&bad).is_none(), "{bad}");
        }
        assert!(LinkCode::parse(&code).is_some());
    }

    #[test]
    fn a_session_value_reads_back_and_verifies_only_on_its_host_with_its_keys() {
        let value = SessionToken::mint(&keys(3), HOST, X, E, JTI, 100, 160);
        let token = SessionToken::parse(&value).expect("a minted value parses");
        assert_eq!(
            (token.x, token.e, token.sid, token.issued, token.active),
            (X, E, JTI, 100, 160)
        );
        assert!(token.verifies(&keys(3), HOST));
        assert!(!token.verifies(&keys(4), HOST));
        assert!(!token.verifies(&keys(3), "archi.hive-k3m9x7q2.fluxbee.ai"));
        let link = LinkCode::mint(&keys(3), HOST, X, E, JTI, 100);
        assert!(SessionToken::parse(&link).is_none());
        // A slid value keeps the session and moves its activity only by a new tag.
        let forged = value.replace(".160.", ".999.");
        assert!(!SessionToken::parse(&forged)
            .unwrap()
            .verifies(&keys(3), HOST));
        for bad in [
            format!("{value}.x"),
            value.replace("S1.", "L1."),
            value.replacen(".100.", ".0100.", 1),
            String::from("S1"),
        ] {
            assert!(SessionToken::parse(&bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn the_cookie_carries_every_attribute_and_is_cleared_with_max_age_0() {
        assert_eq!(
            session_set_cookie("S1.v", 1800),
            "__Host-fluxbee_session=S1.v; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=1800"
        );
        assert_eq!(
            session_clear_cookie(),
            "__Host-fluxbee_session=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0"
        );
    }

    #[test]
    fn a_link_id_is_16_hex_of_the_jti_s_hash_and_tokens_are_fresh() {
        let id = link_id(JTI);
        assert_eq!(id.len(), 16);
        assert!(is_lower_hex(&id, 16));
        assert_eq!(id, link_id(JTI));
        assert_ne!(id, link_id(&JTI.replace('0', "1")));
        assert!(!JTI.contains(&id));
        let (one, two) = (fresh_token(), fresh_token());
        assert!(is_lower_hex(&one, 32) && is_lower_hex(&two, 32));
        assert_ne!(one, two);
        assert_eq!(sid_prefix(&one), &one[..8]);
    }

    #[test]
    fn a_vault_secret_is_64_hex_in_either_case() {
        let lower = "00ff".repeat(16);
        let bytes = secret_from_hex(&lower).expect("64 hex");
        assert_eq!(bytes[..2], [0x00, 0xff]);
        assert_eq!(secret_from_hex(&lower.to_uppercase()), Some(bytes));
        assert_eq!(hex(&bytes), lower);
        for bad in ["", &lower[..62], &format!("{lower}00"), &"zz".repeat(32)] {
            assert_eq!(secret_from_hex(bad), None, "{bad}");
        }
    }
}
