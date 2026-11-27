//! Ids and names (docs/digital-executive-v1-stage-2.md §3.2): a grouping's short id, a
//! deployment's id, the ILKs SY.deploy mints, and the node names that come from the short id.

use rand::Rng;

/// The characters of an id's tail: `[a-z2-7]`, base32's lowercase alphabet.
pub const ID_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";

/// The digit after an id's two letters: 2 to 7, so an ordinary word (`inventory2`) never reads as
/// one.
const ID_DIGITS: &[u8] = b"234567";

/// The characters after the digit.
const ID_TAIL_LEN: usize = 7;

/// A deployment's id: `dp` and the same eight characters' form as a short id's.
pub const DEPLOYMENT_PREFIX: &str = "dp";

/// The two letters of a grouping kind's short ids: `ed` for an ED; `in` and `ts` kept for infra and
/// test.
pub fn grouping_prefix(kind: &str) -> Option<&'static str> {
    match kind {
        "ed" => Some("ed"),
        "infra" => Some("in"),
        "test" => Some("ts"),
        _ => None,
    }
}

fn mint_with(rng: &mut impl Rng, prefix: &str) -> String {
    let mut id = String::with_capacity(prefix.len() + 1 + ID_TAIL_LEN);
    id.push_str(prefix);
    id.push(ID_DIGITS[rng.gen_range(0..ID_DIGITS.len())] as char);
    for _ in 0..ID_TAIL_LEN {
        id.push(ID_ALPHABET[rng.gen_range(0..ID_ALPHABET.len())] as char);
    }
    id
}

/// A new short id for a grouping of `kind`; `None` for a kind with no prefix.
pub fn mint_short_id(rng: &mut impl Rng, kind: &str) -> Option<String> {
    grouping_prefix(kind).map(|prefix| mint_with(rng, prefix))
}

/// A new deployment id.
pub fn mint_deployment_id(rng: &mut impl Rng) -> String {
    mint_with(rng, DEPLOYMENT_PREFIX)
}

/// A new ILK id, `ilk:<uuid>`.
pub fn mint_ilk_id() -> String {
    format!("ilk:{}", uuid::Uuid::new_v4())
}

/// Whether `id` has an id's form after `prefix`: its digit and its seven characters.
fn has_id_form(id: &str, prefix: &str) -> bool {
    let Some(rest) = id.strip_prefix(prefix) else {
        return false;
    };
    let bytes = rest.as_bytes();
    bytes.len() == 1 + ID_TAIL_LEN
        && ID_DIGITS.contains(&bytes[0])
        && bytes[1..].iter().all(|byte| ID_ALPHABET.contains(byte))
}

/// Whether `id` is a grouping's short id of `kind`.
pub fn is_short_id(id: &str, kind: &str) -> bool {
    grouping_prefix(kind).is_some_and(|prefix| has_id_form(id, prefix))
}

/// Whether `id` is a deployment's id.
pub fn is_deployment_id(id: &str) -> bool {
    has_id_form(id, DEPLOYMENT_PREFIX)
}

/// Whether `id` is an ILK id, `ilk:<uuid>`.
pub fn is_ilk_id(id: &str) -> bool {
    id.strip_prefix("ilk:")
        .is_some_and(|uuid| uuid::Uuid::parse_str(uuid).is_ok())
}

/// Whether `id` is a tenant id, `tnt:<uuid>`.
pub fn is_tenant_id(id: &str) -> bool {
    id.strip_prefix("tnt:")
        .is_some_and(|uuid| uuid::Uuid::parse_str(uuid).is_ok())
}

/// A node's name (design §5.5): `<KIND>.<middle.>?<short_id>@<hive>`, as `AI.ed4qm7k2xa@worker1` and
/// `IO.wapp.ed4qm7k2xa@worker1`.
pub fn node_name(kind: &str, middle: Option<&str>, short_id: &str, hive: &str) -> String {
    match middle {
        Some(middle) => format!("{kind}.{middle}.{short_id}@{hive}"),
        None => format!("{kind}.{short_id}@{hive}"),
    }
}

/// Whether `key` is a request key (§2.1): the caller's own id, 1 to 64 characters of
/// `[A-Za-z0-9._:-]`.
pub fn is_request_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    #[test]
    fn minted_ids_have_their_form() {
        let mut rng = StdRng::seed_from_u64(7);
        for _ in 0..500 {
            let short = mint_short_id(&mut rng, "ed").expect("an ed prefix");
            assert_eq!(short.len(), 10, "{short}");
            assert!(short.starts_with("ed"), "{short}");
            assert!(matches!(short.as_bytes()[2], b'2'..=b'7'), "{short}");
            assert!(is_short_id(&short, "ed"), "{short}");
            assert!(!is_short_id(&short, "infra"), "{short}");
            let deployment = mint_deployment_id(&mut rng);
            assert_eq!(deployment.len(), 10, "{deployment}");
            assert!(is_deployment_id(&deployment), "{deployment}");
        }
        assert!(mint_short_id(&mut rng, "custom").is_none());
        assert!(is_short_id("in2abcdefg", "infra"));
        assert!(is_short_id("ts7aaaaaaa", "test"));
        assert!(is_ilk_id(&mint_ilk_id()));
    }

    #[test]
    fn an_ordinary_word_is_never_an_id() {
        for word in [
            "inventory2",
            "ed1abcdefg",
            "ed8abcdefg",
            "ed2abcdef",
            "ed2abcdefgh",
            "ed2abcdef1",
            "ED2ABCDEFG",
            "dp2abcdef0",
        ] {
            assert!(
                !is_short_id(word, "ed") && !is_short_id(word, "infra") && !is_deployment_id(word),
                "{word}"
            );
        }
    }

    #[test]
    fn node_names_come_from_the_short_id() {
        assert_eq!(
            node_name("AI", None, "ed4qm7k2xa", "worker1"),
            "AI.ed4qm7k2xa@worker1"
        );
        assert_eq!(
            node_name("IO", Some("wapp"), "ed4qm7k2xa", "worker1"),
            "IO.wapp.ed4qm7k2xa@worker1"
        );
    }

    #[test]
    fn a_request_key_is_the_caller_s_own_short_id() {
        assert!(is_request_key("cloud-ed-7f3a"));
        assert!(is_request_key("a.b_c:d-E9"));
        assert!(is_request_key(&"k".repeat(64)));
        assert!(!is_request_key(&"k".repeat(65)));
        assert!(!is_request_key(""));
        assert!(!is_request_key("with space"));
        assert!(!is_request_key("slash/key"));
        assert!(is_tenant_id("tnt:43d576a3-d712-4d91-9245-5d5463dd693e"));
        assert!(!is_tenant_id("tnt:nope"));
        assert!(!is_ilk_id("tnt:43d576a3-d712-4d91-9245-5d5463dd693e"));
    }
}
