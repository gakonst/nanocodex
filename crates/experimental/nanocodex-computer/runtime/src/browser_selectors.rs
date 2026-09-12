use crate::{Error, Result};
use serde_json::{Value, json};

/// Parse the selector language emitted by the retained client without evaluating source.
pub(super) fn parse(source: &str) -> Result<(Value, Option<String>)> {
    let parts = split(source)?;
    let mut frames = vec![];
    let mut selector: Option<Value> = None;
    for part in parts {
        if part == "internal:control=enter-frame" {
            let previous = selector
                .take()
                .ok_or_else(|| Error::invalid("Frame entry has no selector"))?;
            if previous["kind"] != "css" {
                return Err(Error::unsupported("Frame selector must currently be CSS"));
            }
            frames.push(previous["value"].as_str().unwrap().to_owned());
            continue;
        }
        let (engine, body) = part
            .split_once('=')
            .filter(|(name, _)| {
                [
                    "css",
                    "xpath",
                    "nth",
                    "visible",
                    "internal:role",
                    "internal:text",
                    "internal:label",
                    "internal:attr",
                    "internal:testid",
                    "internal:has-text",
                    "internal:has-not-text",
                    "internal:has",
                    "internal:has-not",
                    "internal:and",
                    "internal:or",
                ]
                .contains(name)
            })
            .unwrap_or(("css", part));
        match engine {
            "visible" => {
                let value = body
                    .parse::<bool>()
                    .map_err(|_| Error::invalid("Invalid visibility filter"))?;
                selector = Some(
                    json!({"kind":"filter","base":selector.take().ok_or_else(||Error::invalid("Visibility requires an earlier selector"))?,"visible":value}),
                );
            }
            "nth" => {
                let index = body
                    .parse::<i64>()
                    .map_err(|_| Error::invalid("Invalid nth index"))?;
                selector = Some(
                    json!({"kind":"nth","base":selector.take().ok_or_else(||Error::invalid("nth requires an earlier selector"))?,"index":index}),
                );
            }
            "internal:has-text" | "internal:has-not-text" => {
                let (value, _) = matcher(body)?;
                let key = if engine == "internal:has-text" {
                    "hasText"
                } else {
                    "hasNotText"
                };
                let mut node = json!({"kind":"filter","base":selector.take().ok_or_else(||Error::invalid("Text filter requires an earlier selector"))?});
                node[key] = value;
                selector = Some(node);
            }
            "internal:has" | "internal:has-not" | "internal:and" | "internal:or" => {
                let encoded: String = serde_json::from_str(body)
                    .map_err(|_| Error::invalid("Nested selector must be a JSON string"))?;
                let (nested, frame) = parse(&encoded)?;
                if frame.is_some() {
                    return Err(Error::invalid("Nested locator cannot cross a frame"));
                }
                let base = selector
                    .take()
                    .ok_or_else(|| Error::invalid("Nested filter requires an earlier selector"))?;
                let node = match engine {
                    "internal:and" => json!({"kind":"and","left":base,"right":nested}),
                    "internal:or" => json!({"kind":"or","left":base,"right":nested}),
                    _ => {
                        let mut v = json!({"kind":"filter","base":base});
                        v[if engine == "internal:has" {
                            "has"
                        } else {
                            "hasNot"
                        }] = nested;
                        v
                    }
                };
                selector = Some(node);
            }
            _ => {
                let next = match engine {
                    "css" => json!({"kind":"css","value":body}),
                    "xpath" => json!({"kind":"xpath","value":body}),
                    "internal:text" | "internal:label" => {
                        let (value, exact) = matcher(body)?;
                        json!({"kind":if engine=="internal:text"{"text"}else{"label"},"value":value,"exact":exact})
                    }
                    "internal:role" => {
                        let role = body.split('[').next().unwrap_or("");
                        if role.is_empty() {
                            return Err(Error::invalid("Role missing"));
                        }
                        let mut v = json!({"kind":"role","value":role});
                        for (name, value, exact) in attributes(&body[role.len()..])? {
                            let key = if name == "include-hidden" {
                                "includeHidden"
                            } else {
                                &name
                            };
                            v[key] = value;
                            if name == "name" {
                                v["exact"] = json!(exact);
                            }
                        }
                        v
                    }
                    "internal:testid" | "internal:attr" => {
                        let attrs = attributes(body)?;
                        if attrs.len() != 1 {
                            return Err(Error::invalid(
                                "Attribute selector requires exactly one attribute",
                            ));
                        }
                        let (name, value, exact) = attrs.into_iter().next().unwrap();
                        if engine == "internal:testid" {
                            json!({"kind":"testid","attribute":name,"value":value})
                        } else {
                            let kind = match name.as_str() {
                                "placeholder" => "placeholder",
                                "alt" => "alt",
                                "title" => "title",
                                _ => {
                                    return Err(Error::unsupported(
                                        "Unsupported internal attribute locator",
                                    ));
                                }
                            };
                            json!({"kind":kind,"value":value,"exact":exact})
                        }
                    }
                    _ => unreachable!(),
                };
                selector = Some(match selector.take() {
                    None => next,
                    Some(previous) => json!({"kind":"chain","steps":[previous,next]}),
                });
            }
        }
    }
    Ok((
        selector.ok_or_else(|| Error::invalid("Selector is empty"))?,
        (!frames.is_empty()).then(|| frames.join(" >> ")),
    ))
}
fn split(source: &str) -> Result<Vec<&str>> {
    let bytes = source.as_bytes();
    let mut start = 0;
    let mut index = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut depth = 0;
    let mut result = vec![];
    while index < bytes.len() {
        let c = bytes[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        if c == b'\\' {
            escaped = true;
            index += 1;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            index += 1;
            continue;
        }
        if c == b'"' || c == b'\'' || (c == b'/' && index > 0 && bytes[index - 1] == b'=') {
            quote = Some(c);
            index += 1;
            continue;
        }
        match c {
            b'[' | b'(' | b'{' => depth += 1,
            b']' | b')' | b'}' => {
                if depth == 0 {
                    return Err(Error::invalid("Unbalanced selector"));
                }
                depth -= 1
            }
            _ => {}
        }
        if depth == 0 && source[index..].starts_with(" >> ") {
            result.push(source[start..index].trim());
            index += 4;
            start = index;
        } else {
            index += 1;
            while index < bytes.len() && !source.is_char_boundary(index) {
                index += 1;
            }
        }
    }
    if quote.is_some() || depth != 0 {
        return Err(Error::invalid("Unterminated selector"));
    }
    result.push(source[start..].trim());
    if result.iter().any(|s| s.is_empty()) {
        return Err(Error::invalid("Empty selector step"));
    }
    Ok(result)
}
fn matcher(value: &str) -> Result<(Value, bool)> {
    if let Some(regex) = value.strip_prefix('/') {
        let end = regex
            .rfind('/')
            .ok_or_else(|| Error::invalid("Unterminated regex matcher"))?;
        return Ok((
            json!({"regex":&regex[..end],"flags":&regex[end+1..]}),
            false,
        ));
    }
    let (exact, encoded) = if let Some(s) = value.strip_suffix('s') {
        (true, s)
    } else if let Some(s) = value.strip_suffix('i') {
        (false, s)
    } else {
        (true, value)
    };
    let decoded: String = serde_json::from_str(encoded)
        .map_err(|_| Error::invalid("Text matcher must be a quoted string or regex"))?;
    Ok((json!(decoded), exact))
}
fn attributes(mut value: &str) -> Result<Vec<(String, Value, bool)>> {
    let mut out = vec![];
    while !value.is_empty() {
        if !value.starts_with('[') {
            return Err(Error::invalid("Invalid role attribute"));
        }
        let mut quote = None;
        let mut escaped = false;
        let mut end = None;
        for (i, c) in value.char_indices().skip(1) {
            if escaped {
                escaped = false;
                continue;
            }
            if c == '\\' {
                escaped = true;
                continue;
            }
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
                continue;
            }
            if c == '"' || c == '\'' || c == '/' {
                quote = Some(c);
                continue;
            }
            if c == ']' {
                end = Some(i);
                break;
            }
        }
        let end = end.ok_or_else(|| Error::invalid("Unterminated role attribute"))?;
        let (name, raw) = value[1..end]
            .split_once('=')
            .ok_or_else(|| Error::invalid("Role attribute requires a value"))?;
        let (parsed, exact) = if raw == "true" || raw == "false" {
            (json!(raw == "true"), true)
        } else if let Ok(number) = raw.parse::<i64>() {
            (json!(number), true)
        } else {
            matcher(raw)?
        };
        out.push((name.into(), parsed, exact));
        value = &value[end + 1..];
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_quoted_delimiters_and_unicode() {
        let (v, f) = parse("css=section >> internal:text=\"α >> β\"i >> nth=1").unwrap();
        assert!(f.is_none());
        assert_eq!(v["base"]["steps"][1]["value"], "α >> β");
        assert_eq!(v["index"], 1);
    }
    #[test]
    fn decodes_roles_and_nested_filters() {
        let(v,_)=parse("internal:role=button[name=\"Save\"s][disabled=false] >> internal:has-not-text=\"Later\"i").unwrap();
        assert_eq!(v["base"]["exact"], true);
        assert_eq!(v["base"]["disabled"], false);
        assert_eq!(v["hasNotText"], "Later");
    }
    #[test]
    fn parses_frames_and_rejects_cross_frame_filters() {
        let (v, f) =
            parse("iframe#login >> internal:control=enter-frame >> internal:label=\"Password\"i")
                .unwrap();
        assert_eq!(f.as_deref(), Some("iframe#login"));
        assert_eq!(v["kind"], "label");
        assert!(
            parse("button >> internal:has=\"iframe >> internal:control=enter-frame >> input\"")
                .is_err()
        );
    }
}
