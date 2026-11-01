use crate::{
    expression::{tag_end, Expression},
    Limits, TemplateError,
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Node {
    Text(String),
    Emit {
        expression: Expression,
        escaped: bool,
    },
    Section {
        expression: Expression,
        inverted: bool,
        children: Vec<Node>,
    },
}

pub(crate) fn identifier(s: &str) -> bool {
    let mut chars = s.bytes();
    matches!(chars.next(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_'))
        && chars.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
pub(crate) fn parse(source: &str, limits: Limits) -> Result<Vec<Node>, TemplateError> {
    let mut parser = Parser {
        source,
        pos: 0,
        remaining: limits.max_nodes,
        limits,
    };
    parser.sequence(None, 0)
}
struct Parser<'a> {
    source: &'a str,
    pos: usize,
    remaining: usize,
    limits: Limits,
}
impl Parser<'_> {
    fn charge(&mut self) -> Result<(), TemplateError> {
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or(TemplateError::Limit("syntax nodes"))?;
        Ok(())
    }
    fn sequence(
        &mut self,
        expected: Option<&Expression>,
        depth: usize,
    ) -> Result<Vec<Node>, TemplateError> {
        if depth > self.limits.max_depth {
            return Err(TemplateError::Limit("syntax depth"));
        }
        let mut out = Vec::new();
        while self.pos < self.source.len() {
            let Some(relative) = self.source[self.pos..].find("{{") else {
                self.charge()?;
                out.push(Node::Text(self.source[self.pos..].to_owned()));
                self.pos = self.source.len();
                break;
            };
            let start = self.pos + relative;
            if start > self.pos {
                self.charge()?;
                out.push(Node::Text(self.source[self.pos..start].to_owned()));
            }
            self.charge()?; // Also charge comments/closing tags.
            let raw = self.source[start..].starts_with("{{{");
            let begin = start + if raw { 3 } else { 2 };
            let close = if raw { "}}}" } else { "}}" };
            let end = if !raw && self.source[begin..].trim_start().starts_with('!') {
                self.source[begin..].find(close).map(|n| begin + n)
            } else {
                tag_end(self.source, begin, close)
            }
            .ok_or(TemplateError::Syntax {
                offset: start,
                reason: "unterminated tag",
            })?;
            let tag = self.source[begin..end].trim();
            self.pos = end + close.len();
            if raw {
                out.push(Node::Emit {
                    expression: Expression::parse(tag, start, &mut self.remaining)?,
                    escaped: false,
                });
                continue;
            }
            // ASCII markers only; never byte-slice a leading Unicode character.
            match tag.as_bytes().first().copied() {
                Some(b'!') => {}
                Some(b'#' | b'^') => {
                    let expression = Expression::parse(&tag[1..], start, &mut self.remaining)?;
                    let children = self.sequence(Some(&expression), depth + 1)?;
                    out.push(Node::Section {
                        expression,
                        inverted: tag.starts_with('^'),
                        children,
                    });
                }
                Some(b'/') => {
                    let expression = Expression::parse(&tag[1..], start, &mut self.remaining)?;
                    if expected != Some(&expression) {
                        return Err(TemplateError::Syntax {
                            offset: start,
                            reason: "unexpected or mismatched section close",
                        });
                    }
                    return Ok(out);
                }
                Some(b'&') => out.push(Node::Emit {
                    expression: Expression::parse(&tag[1..], start, &mut self.remaining)?,
                    escaped: false,
                }),
                Some(b'>' | b'=' | b'$' | b'<') => {
                    return Err(TemplateError::Syntax {
                        offset: start,
                        reason: "partials, inheritance and delimiter changes are unsupported",
                    })
                }
                _ => out.push(Node::Emit {
                    expression: Expression::parse(tag, start, &mut self.remaining)?,
                    escaped: true,
                }),
            }
        }
        if expected.is_some() {
            return Err(TemplateError::Syntax {
                offset: self.pos,
                reason: "unclosed section",
            });
        }
        Ok(out)
    }
}
