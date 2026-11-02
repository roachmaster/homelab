//! Validated YAML tokens, not CSS templates or a raw style escape hatch.
use serde::Deserialize;
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub background: String,
    pub surface: String,
    pub raised: String,
    pub border: String,
    pub text: String,
    pub muted: String,
    pub accent: String,
    pub warning: String,
    pub error: String,
    pub font_px: u16,
    pub radius_px: u16,
    pub spacing_px: u16,
    pub sidebar_px: u16,
    pub details_px: u16,
}
impl Theme {
    pub fn parse(yaml: &str) -> Result<Self, &'static str> {
        if yaml.len() > 8192 {
            return Err("theme byte limit");
        }
        let t: Self = serde_yaml_ng::from_str(yaml).map_err(|_| "invalid theme fields")?;
        for color in [
            &t.background,
            &t.surface,
            &t.raised,
            &t.border,
            &t.text,
            &t.muted,
            &t.accent,
            &t.warning,
            &t.error,
        ] {
            if color.len() != 7
                || !color.starts_with('#')
                || !color[1..].bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err("theme colors require six hex digits");
            }
        }
        if !(12..=22).contains(&t.font_px)
            || t.radius_px > 24
            || !(4..=24).contains(&t.spacing_px)
            || !(160..=400).contains(&t.sidebar_px)
            || !(220..=480).contains(&t.details_px)
        {
            return Err("theme numeric limit");
        }
        Ok(t)
    }
}
