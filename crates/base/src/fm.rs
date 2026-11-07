//! A deliberately small YAML-frontmatter subset.
//!
//! Supported, and nothing else:
//!   key: scalar
//!   key: [a, b, c]
//!   key:
//!     - a
//!     - b
//!
//! Scalars may be quoted with `"` when they would otherwise be ambiguous.
//! Anything richer belongs in the Markdown body, not the frontmatter.

#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    S(String),
    L(Vec<String>),
}

impl Val {
    pub fn as_str(&self) -> String {
        match self {
            Val::S(s) => s.clone(),
            Val::L(v) => v.join(", "),
        }
    }
    pub fn as_list(&self) -> Vec<String> {
        match self {
            Val::S(s) if s.is_empty() => vec![],
            Val::S(s) => vec![s.clone()],
            Val::L(v) => v.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Doc {
    pub fields: Vec<(String, Val)>,
    pub body: String,
}

impl Doc {
    pub fn get(&self, key: &str) -> Option<&Val> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn str(&self, key: &str) -> String {
        self.get(key).map(|v| v.as_str()).unwrap_or_default()
    }

    pub fn opt(&self, key: &str) -> Option<String> {
        match self.get(key).map(|v| v.as_str()) {
            Some(s) if !s.is_empty() && s != "none" && s != "null" => Some(s),
            _ => None,
        }
    }

    pub fn list(&self, key: &str) -> Vec<String> {
        self.get(key).map(|v| v.as_list()).unwrap_or_default()
    }

    pub fn set(&mut self, key: &str, val: Val) {
        if let Some(slot) = self.fields.iter_mut().find(|(k, _)| k == key) {
            slot.1 = val;
        } else {
            self.fields.push((key.to_string(), val));
        }
    }

    pub fn set_str(&mut self, key: &str, val: &str) {
        self.set(key, Val::S(val.to_string()));
    }

    pub fn set_list(&mut self, key: &str, val: &[String]) {
        self.set(key, Val::L(val.to_vec()));
    }

}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        let inner = &s[1..s.len() - 1];
        return inner.replace("\\\"", "\"").replace("\\\\", "\\");
    }
    s.to_string()
}

fn parse_inline_list(s: &str) -> Vec<String> {
    let inner = s.trim().trim_start_matches('[').trim_end_matches(']');
    inner
        .split(',')
        .map(unquote)
        .filter(|x| !x.is_empty())
        .collect()
}

pub fn parse(text: &str) -> Doc {
    let mut doc = Doc::default();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    let rest = match text.strip_prefix("---\n") {
        Some(r) => r,
        None => {
            doc.body = text.to_string();
            return doc;
        }
    };

    let (head, body) = match rest.find("\n---") {
        Some(i) => {
            let after = &rest[i + 4..];
            (&rest[..i], after.strip_prefix('\n').unwrap_or(after))
        }
        None => (rest, ""),
    };

    let mut pending: Option<String> = None;
    for line in head.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() || trimmed.trim_start().starts_with('#') {
            continue;
        }

        // Continuation of a block list: "  - value"
        if line.starts_with(char::is_whitespace) || trimmed.trim_start().starts_with("- ") {
            if let Some(key) = pending.clone() {
                let item = unquote(trimmed.trim().trim_start_matches("- "));
                if !item.is_empty() {
                    let mut cur = doc.get(&key).map(|v| v.as_list()).unwrap_or_default();
                    cur.push(item);
                    doc.set(&key, Val::L(cur));
                }
                continue;
            }
        }

        let Some(colon) = trimmed.find(':') else { continue };
        let key = trimmed[..colon].trim().to_string();
        if key.is_empty() {
            continue;
        }
        let value = trimmed[colon + 1..].trim();

        if value.is_empty() {
            // Either an empty scalar or the head of a block list; decided by
            // whatever follows.
            doc.set(&key, Val::L(vec![]));
            pending = Some(key);
        } else if value.starts_with('[') {
            doc.set(&key, Val::L(parse_inline_list(value)));
            pending = None;
        } else {
            doc.set(&key, Val::S(unquote(value)));
            pending = None;
        }
    }

    // An empty block-list that never got items reads better as an empty scalar.
    for (_, v) in doc.fields.iter_mut() {
        if let Val::L(items) = v {
            if items.is_empty() {
                *v = Val::S(String::new());
            }
        }
    }

    doc.body = body.trim_start_matches('\n').to_string();
    doc
}

fn needs_quotes(s: &str) -> bool {
    s.is_empty()
        || s != s.trim()
        || s.starts_with(['[', ']', '{', '}', '#', '&', '*', '!', '|', '>', '%', '@', '`', '"', '\''])
        || s.contains(": ")
        || s.ends_with(':')
        || s.contains('\n')
}

fn emit_scalar(s: &str) -> String {
    if needs_quotes(s) {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

pub fn emit(doc: &Doc) -> String {
    let mut out = String::from("---\n");
    for (k, v) in &doc.fields {
        match v {
            Val::S(s) => out.push_str(&format!("{k}: {}\n", emit_scalar(s))),
            Val::L(items) => {
                let rendered: Vec<String> = items.iter().map(|i| emit_scalar(i)).collect();
                out.push_str(&format!("{k}: [{}]\n", rendered.join(", ")));
            }
        }
    }
    out.push_str("---\n\n");
    out.push_str(doc.body.trim_start_matches('\n'));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}
