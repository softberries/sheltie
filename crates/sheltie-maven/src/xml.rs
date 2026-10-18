//! A small XML reader for POMs: elements and their text. Attributes are skipped; comments,
//! processing instructions and the document type are ignored; CDATA counts as text.

/// An element: its local name (namespace prefix dropped), children and text.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Element {
    pub(crate) name: String,
    pub(crate) children: Vec<Element>,
    pub(crate) text: String,
}

impl Element {
    /// The first child named `name`.
    pub(crate) fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Every child named `name`.
    pub(crate) fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// The trimmed text of the first child named `name`, if not empty.
    pub(crate) fn text_of(&self, name: &str) -> Option<&str> {
        self.child(name)
            .map(|c| c.text.trim())
            .filter(|t| !t.is_empty())
    }

    /// The element at a path of child names.
    pub(crate) fn at(&self, path: &[&str]) -> Option<&Element> {
        path.iter().try_fold(self, |e, name| e.child(name))
    }
}

/// Parses a document and returns its root element.
pub(crate) fn parse(src: &str) -> Result<Element, String> {
    let mut stack: Vec<Element> = vec![Element::default()];
    let mut rest = src;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            append_text(&mut stack, &decode(rest));
            break;
        };
        if lt > 0 {
            append_text(&mut stack, &decode(&rest[..lt]));
        }
        rest = &rest[lt..];
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = skip_past(r, "-->")?;
        } else if let Some(r) = rest.strip_prefix("<![CDATA[") {
            let end = r.find("]]>").ok_or("unterminated CDATA section")?;
            append_text(&mut stack, &r[..end]);
            rest = &r[end + 3..];
        } else if let Some(r) = rest.strip_prefix("<?") {
            rest = skip_past(r, "?>")?;
        } else if let Some(r) = rest.strip_prefix("<!") {
            rest = skip_declaration(r)?;
        } else if let Some(r) = rest.strip_prefix("</") {
            let end = r.find('>').ok_or("unterminated end tag")?;
            let name = local(r[..end].trim());
            rest = &r[end + 1..];
            if stack.len() < 2 {
                return Err(format!("unexpected </{name}>"));
            }
            let done = stack.pop().ok_or("unbalanced tags")?;
            if done.name != name {
                return Err(format!("<{}> closed by </{name}>", done.name));
            }
            if let Some(parent) = stack.last_mut() {
                parent.children.push(done);
            }
        } else {
            let (inner, after) = tag(&rest[1..])?;
            rest = after;
            let self_closing = inner.ends_with('/');
            let inner = inner.trim_end_matches('/');
            let name_end = inner
                .find(|c: char| c.is_whitespace())
                .unwrap_or(inner.len());
            let element = Element {
                name: local(&inner[..name_end]).to_owned(),
                ..Element::default()
            };
            if element.name.is_empty() {
                return Err("empty tag name".into());
            }
            if self_closing {
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(element);
                }
            } else {
                stack.push(element);
            }
        }
    }
    if stack.len() != 1 {
        return Err("unclosed elements".into());
    }
    stack
        .pop()
        .and_then(|doc| doc.children.into_iter().next())
        .ok_or_else(|| "no root element".into())
}

fn append_text(stack: &mut [Element], text: &str) {
    if let Some(e) = stack.last_mut() {
        e.text.push_str(text);
    }
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn skip_past<'a>(s: &'a str, end: &str) -> Result<&'a str, String> {
    s.find(end)
        .map(|i| &s[i + end.len()..])
        .ok_or_else(|| format!("missing {end}"))
}

/// Skips `<!DOCTYPE …>`, including an internal subset in brackets.
fn skip_declaration(s: &str) -> Result<&str, String> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            '>' if depth == 0 => return Ok(&s[i + 1..]),
            _ => {}
        }
    }
    Err("unterminated declaration".into())
}

/// The inside of a start tag (after `<`, up to `>`, quoted attribute values skipped) and the
/// rest.
fn tag(s: &str) -> Result<(&str, &str), String> {
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => return Ok((s[..i].trim(), &s[i + 1..])),
            _ => {}
        }
    }
    Err("unterminated start tag".into())
}

/// Replaces the predefined and numeric character references.
fn decode(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let Some(semi) = after.find(';').filter(|&s| s <= 10) else {
            out.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..semi];
        let ch = match entity {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e => e
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| e.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_elements_text_and_skips_the_rest() {
        let doc = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE project [ <!ENTITY x "y"> ]>
<!-- a comment with <tags> -->
<project xmlns="http://maven.apache.org/POM/4.0.0" a='>'>
  <groupId>com.acme</groupId>
  <!-- <artifactId>commented</artifactId> -->
  <artifactId>a&amp;b&#x41;</artifactId>
  <description><![CDATA[<raw> & text]]></description>
  <modules><module>core</module><module>api</module></modules>
  <empty/>
  <pom:prefixed xmlns:pom="x">v</pom:prefixed>
</project>"#;
        let p = parse(doc).unwrap();
        assert_eq!(p.name, "project");
        assert_eq!(p.text_of("groupId"), Some("com.acme"));
        assert_eq!(p.text_of("artifactId"), Some("a&bA"));
        assert_eq!(p.text_of("description"), Some("<raw> & text"));
        let modules: Vec<_> = p
            .child("modules")
            .unwrap()
            .all("module")
            .map(|m| m.text.as_str())
            .collect();
        assert_eq!(modules, ["core", "api"]);
        assert!(p.child("empty").is_some() && p.text_of("empty").is_none());
        assert_eq!(p.text_of("prefixed"), Some("v"));
        assert_eq!(
            p.at(&["modules", "module"]).map(|m| m.text.as_str()),
            Some("core")
        );
    }

    #[test]
    fn rejects_broken_documents() {
        assert!(parse("<a><b></a>").is_err());
        assert!(parse("<a>").is_err());
        assert!(parse("<!-- x").is_err());
        assert!(parse("text only").is_err());
    }
}
