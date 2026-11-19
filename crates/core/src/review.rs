use crate::{Document, Error, OVERRIDE_SCHEMA, OVERRIDE_VALIDATOR, invalid, validate};
use serde_json::Value;

#[derive(Debug, PartialEq)]
pub struct Conflict {
    pub path: String,
    pub expected: Value,
    pub observed: Value,
}

/// Applies replacements using JSON Pointer escaping. Within record arrays, use
/// an @id token to identify a record independently of its position.
pub fn apply_overrides(
    document: &Document,
    overrides: &Value,
) -> Result<(Document, Vec<Conflict>), Error> {
    let (source, conflicts) = merge_overrides(document.source(), overrides)?;
    Ok((Document::from_value(source)?, conflicts))
}

/// Applies the same replacements to an incoming scrape before validation.
pub fn merge_overrides(source: &Value, overrides: &Value) -> Result<(Value, Vec<Conflict>), Error> {
    validate(overrides, OVERRIDE_SCHEMA, &OVERRIDE_VALIDATOR)?;
    let patches = overrides["patches"]
        .as_array()
        .ok_or_else(|| invalid("/patches", "expected array"))?;
    let mut result = source.clone();
    let mut conflicts = Vec::new();
    for patch in patches {
        let path = patch["path"]
            .as_str()
            .ok_or_else(|| invalid("/path", "expected string"))?;
        let tokens = path
            .strip_prefix('/')
            .ok_or_else(|| invalid(path, "invalid JSON Pointer"))?
            .split('/')
            .map(decode)
            .collect::<Result<Vec<_>, _>>()?;
        let mut node = &mut result;
        for token in &tokens {
            node = child(node, token)
                .ok_or_else(|| invalid(path, format!("missing pointer token {token}")))?;
        }
        let replacement = &patch["value"];
        if let Some(expected) = patch.get("expected") {
            if node != expected && node != replacement {
                tracing::warn!(path, "scrape disagrees with override baseline");
                conflicts.push(Conflict {
                    path: path.to_owned(),
                    expected: expected.clone(),
                    observed: node.clone(),
                });
            }
        }
        *node = replacement.clone();
    }
    Ok((result, conflicts))
}

fn decode(part: &str) -> Result<String, Error> {
    let mut result = String::new();
    let mut chars = part.chars();
    while let Some(character) = chars.next() {
        match character {
            '~' => match chars.next() {
                Some('0') => result.push('~'),
                Some('1') => result.push('/'),
                _ => return Err(invalid(part, "invalid JSON Pointer escape")),
            },
            _ => result.push(character),
        }
    }
    Ok(result)
}

fn child<'a>(node: &'a mut Value, token: &str) -> Option<&'a mut Value> {
    match node {
        Value::Object(object) => object.get_mut(token),
        Value::Array(entries) => {
            if let Some(id) = token.strip_prefix('@') {
                let mut matches = entries
                    .iter_mut()
                    .filter(|entry| entry.get("id").and_then(Value::as_str) == Some(id));
                let entry = matches.next()?;
                return matches.next().is_none().then_some(entry);
            }
            if token.len() > 1 && token.starts_with('0')
                || !token.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            entries.get_mut(token.parse::<usize>().ok()?)
        }
        _ => None,
    }
}
