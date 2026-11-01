use crate::{parser::identifier, TemplateError, Value};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Expression {
    pub path: String,
    pub calls: Vec<Call>,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Call {
    pub name: String,
    pub args: Vec<Argument>,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Argument {
    Literal(Value),
    Path(String),
}

fn syntax(offset: usize, reason: &'static str) -> TemplateError {
    TemplateError::Syntax { offset, reason }
}
pub(crate) fn method_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
fn path(s: &str, offset: usize) -> Result<String, TemplateError> {
    let s = s.trim();
    if s != "." && !s.split('.').all(identifier) {
        return Err(syntax(offset, "expected ASCII dotted identifiers or '.'"));
    }
    Ok(s.to_owned())
}
impl Expression {
    pub fn parse(
        source: &str,
        offset: usize,
        remaining: &mut usize,
    ) -> Result<Self, TemplateError> {
        let source = source.trim();
        let Some(open) = source.find('(') else {
            return Ok(Self {
                path: path(source, offset)?,
                calls: vec![],
            });
        };
        let dot = source[..open]
            .rfind('.')
            .ok_or(syntax(offset, "methods require a receiver path"))?;
        let receiver = if dot == 0 { "." } else { &source[..dot] };
        let mut at = dot + 1;
        let mut calls = Vec::new();
        while at < source.len() {
            let begin = at;
            while source
                .as_bytes()
                .get(at)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
            {
                at += 1;
            }
            let name = &source[begin..at];
            if !method_name(name) || source.as_bytes().get(at) != Some(&b'(') {
                return Err(syntax(offset + at, "expected method_name(arguments)"));
            }
            at += 1;
            let args_start = at;
            let mut quote = None;
            let mut escaped = false;
            let mut end = None;
            for (i, c) in source[at..].char_indices() {
                if let Some(q) = quote {
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == q {
                        quote = None;
                    }
                } else if c == '\'' || c == '"' {
                    quote = Some(c);
                } else if c == '(' {
                    return Err(syntax(
                        offset + at + i,
                        "nested calls in arguments are unsupported",
                    ));
                } else if c == ')' {
                    end = Some(at + i);
                    break;
                }
            }
            let end = end.ok_or(syntax(offset + at, "unterminated method arguments"))?;
            charge(remaining)?;
            calls.push(Call {
                name: name.into(),
                args: arguments(&source[args_start..end], offset + args_start, remaining)?,
            });
            at = end + 1;
            if at == source.len() {
                break;
            }
            if source.as_bytes().get(at) != Some(&b'.') {
                return Err(syntax(
                    offset + at,
                    "method chains use '.' with no trailing expression",
                ));
            }
            at += 1;
            if at == source.len() {
                return Err(syntax(offset + at, "unfinished method chain"));
            }
        }
        Ok(Self {
            path: path(receiver, offset)?,
            calls,
        })
    }
}
fn arguments(
    s: &str,
    offset: usize,
    remaining: &mut usize,
) -> Result<Vec<Argument>, TemplateError> {
    if s.trim().is_empty() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c == ',' {
            charge(remaining)?;
            out.push(argument(&s[start..i], offset + start)?);
            start = i + 1;
        }
    }
    charge(remaining)?;
    out.push(argument(&s[start..], offset + start)?);
    Ok(out)
}
fn argument(s: &str, offset: usize) -> Result<Argument, TemplateError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(syntax(offset, "empty method argument"));
    }
    let value = if s.starts_with(['\'', '"']) {
        let quote = s.chars().next().expect("nonempty checked");
        if s.len() < 2 || !s.ends_with(quote) {
            return Err(syntax(offset, "unterminated string literal"));
        }
        let mut text = String::new();
        let mut escaped = false;
        for c in s[1..s.len() - 1].chars() {
            if escaped {
                text.push(match c {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '\\' => '\\',
                    '\'' => '\'',
                    '"' => '"',
                    _ => return Err(syntax(offset, "unsupported string escape")),
                });
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == quote {
                return Err(syntax(offset, "unescaped quote in literal"));
            } else {
                text.push(c);
            }
        }
        if escaped {
            return Err(syntax(offset, "unfinished string escape"));
        }
        Some(Value::Text(text))
    } else {
        match s {
            "null" => Some(Value::Null),
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ => {
                if let Ok(n) = s.parse::<i64>() {
                    Some(Value::Int(n))
                } else if let Ok(n) = s.parse::<f64>() {
                    if !n.is_finite() {
                        return Err(syntax(offset, "non-finite numeric literal"));
                    }
                    Some(Value::Float(n))
                } else {
                    None
                }
            }
        }
    };
    match value {
        Some(v) => Ok(Argument::Literal(v)),
        None => Ok(Argument::Path(path(s, offset)?)),
    }
}

/// Delimiters inside quoted method arguments are literal text, not tag endings.
pub(crate) fn tag_end(source: &str, begin: usize, close: &str) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    for (relative, c) in source[begin..].char_indices() {
        let at = begin + relative;
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
        } else {
            if source[at..].starts_with(close) {
                return Some(at);
            }
            if c == '\'' || c == '"' {
                quote = Some(c);
            }
        }
    }
    None
}

fn charge(remaining: &mut usize) -> Result<(), TemplateError> {
    *remaining = remaining
        .checked_sub(1)
        .ok_or(TemplateError::Limit("syntax nodes"))?;
    Ok(())
}
