//! The built-in mesh skill (SPEC §9).

use super::{VERSION, store};
use crate::model::cmp_versions;
use std::cmp::Ordering;

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

fn meta<'a>(front: &'a str, key: &str) -> Option<&'a str> {
    front
        .lines()
        .find_map(|l| l.trim().strip_prefix(key)?.strip_prefix(':'))
        .map(|v| v.trim().trim_matches('"'))
}

/// The skill as mesh writes it: frontmatter gains `metadata.mesh/version` and `metadata.mesh/hash`.
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

fn mesh_copy() -> Option<String> {
    std::fs::read_to_string(store::cached("skills/mesh/SKILL.md"))
        .ok()
        .map(|t| t.replace("\r\n", "\n"))
}

/// What `mesh update` should do with the mesh's copy.
pub(crate) enum Publish {
    Write(String),
    UpToDate,
    /// The mesh's copy is from a newer mesh: never downgrade.
    Newer(String),
    /// The user edited it (or wrote their own): leave it.
    Customised,
}

pub(crate) fn publish() -> Publish {
    let Some(text) = mesh_copy() else {
        return Publish::Write(builtin());
    };
    let builtin_text = BUILTIN.replace("\r\n", "\n");
    let builtin_body = split(&builtin_text).map(|(_, b)| b).unwrap_or_default();
    let Some((front, body)) = split(&text) else {
        return Publish::Customised;
    };
    match (meta(front, "mesh/version"), meta(front, "mesh/hash")) {
        (_, Some(h)) if h != hash(body.as_bytes()) => Publish::Customised,
        (Some(v), _) if cmp_versions(v, VERSION) == Ordering::Greater => {
            Publish::Newer(v.to_string())
        }
        (Some(v), _) if cmp_versions(v, VERSION) == Ordering::Equal => Publish::UpToDate,
        // never written by mesh: adopt it only if it is the built-in text
        (None, None) if body != builtin_body => Publish::Customised,
        _ => Publish::Write(builtin()),
    }
}

/// The mesh's copy no longer matches the hash mesh wrote (the user customised it).
pub(crate) fn customised() -> bool {
    let Some(text) = mesh_copy() else {
        return false;
    };
    let Some((front, body)) = split(&text) else {
        return false;
    };
    meta(front, "mesh/hash").is_some_and(|h| h != hash(body.as_bytes()))
}
