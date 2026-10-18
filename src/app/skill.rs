//! The built-in mesh skill (SPEC §9).

use super::{VERSION, store};

const BUILTIN: &str = include_str!("../../skill/SKILL.md");

/// FNV-1a 64: enough to notice that someone edited the body.
fn hash(b: &[u8]) -> String {
    let h = b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| {
        (h ^ x as u64).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{h:016x}")
}

fn split(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    Some((&rest[..end], &rest[end + 5..]))
}

/// The skill as mesh writes it into a new mesh: frontmatter gains `metadata.mesh/version` and
/// `metadata.mesh/hash` (of the body).
pub(crate) fn builtin() -> String {
    let text = BUILTIN.replace("\r\n", "\n");
    match split(&text) {
        Some((front, body)) => format!(
            "---\n{front}\nmetadata:\n  mesh/version: \"{VERSION}\"\n  mesh/hash: \"{}\"\n---\n{body}",
            hash(body.as_bytes())
        ),
        None => text,
    }
}

/// The mesh's copy no longer matches the hash mesh wrote (the user customised it).
pub(crate) fn customised() -> bool {
    let Ok(text) = std::fs::read_to_string(store::cached("skills/mesh/SKILL.md")) else {
        return false;
    };
    let text = text.replace("\r\n", "\n");
    let Some((front, body)) = split(&text) else {
        return false;
    };
    let wrote = front
        .lines()
        .find_map(|l| l.trim().strip_prefix("mesh/hash:"))
        .map(|v| v.trim().trim_matches('"'));
    wrote.is_some_and(|h| h != hash(body.as_bytes()))
}
