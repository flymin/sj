use crate::util::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Color {
    Name(String),
    Index(u8),
}

impl Color {
    fn code(&self, background: bool) -> Result<String> {
        let base = if background { 48 } else { 38 };
        match self {
            Self::Index(n) => Ok(format!("{base};5;{n}")),
            Self::Name(name) => {
                if name == "reset" {
                    return Ok(if background { "49" } else { "39" }.into());
                }
                if name.starts_with('#') && name.len() == 7 && name.is_ascii() {
                    let channel = |offset| u8::from_str_radix(&name[offset..offset + 2], 16);
                    if let (Ok(r), Ok(g), Ok(b)) = (channel(1), channel(3), channel(5)) {
                        return Ok(format!("{base};2;{r};{g};{b}"));
                    }
                }
                let colors = [
                    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
                ];
                let (plain, bright) = name
                    .strip_prefix("bright_")
                    .map_or((name.as_str(), false), |s| (s, true));
                if let Some(index) = colors.iter().position(|s| *s == plain) {
                    return Ok(((if background { 40 } else { 30 })
                        + if bright { 60 } else { 0 }
                        + index)
                        .to_string());
                }
                Err(format!(
                    "invalid color {name:?}; use a named color, 0..255, #RRGGBB, or reset"
                ))
            }
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: Option<bool>,
    pub dim: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
}

impl Style {
    pub fn overlay(&mut self, other: &Self) {
        macro_rules! copy { ($($key:ident),*) => { $(if other.$key.is_some() { self.$key = other.$key.clone(); })* }; }
        copy!(fg, bg, bold, dim, italic, underline);
    }
    pub fn validate(&self) -> Result<()> {
        for color in [&self.fg, &self.bg].into_iter().flatten() {
            color.code(false)?;
        }
        Ok(())
    }
    pub fn paint(&self, text: &str, enabled: bool) -> String {
        if !enabled || text.is_empty() {
            return text.into();
        }
        let mut codes = Vec::new();
        for (color, background) in [(&self.fg, false), (&self.bg, true)] {
            if let Some(color) = color {
                codes.push(color.code(background).expect("validated color"));
            }
        }
        // SGR 22 clears both bold and dim; apply it before re-enabling either.
        if self.bold == Some(false) || self.dim == Some(false) {
            codes.push("22".into());
        }
        if self.bold == Some(true) {
            codes.push("1".into());
        }
        if self.dim == Some(true) {
            codes.push("2".into());
        }
        for (value, on, off) in [(self.italic, "3", "23"), (self.underline, "4", "24")] {
            if let Some(value) = value {
                codes.push(if value { on } else { off }.into());
            }
        }
        if codes.is_empty() {
            text.into()
        } else {
            format!("\x1b[{}m{text}\x1b[0m", codes.join(";"))
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StateScope {
    #[default]
    State,
    Row,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub header: Style,
    pub row: Style,
    pub status: Style,
    pub state_scope: Option<StateScope>,
    pub states: BTreeMap<String, Style>,
}
impl Theme {
    pub fn overlay(&mut self, other: &Self) {
        self.header.overlay(&other.header);
        self.row.overlay(&other.row);
        self.status.overlay(&other.status);
        if other.state_scope.is_some() {
            self.state_scope = other.state_scope;
        }
        for (name, style) in &other.states {
            self.states.entry(name.clone()).or_default().overlay(style);
        }
    }
    pub fn validate(&self) -> Result<()> {
        for style in [&self.header, &self.row, &self.status]
            .into_iter()
            .chain(self.states.values())
        {
            style.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}
impl ColorMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "auto" => Ok(Self::Auto),
            "always" => Ok(Self::Always),
            "never" => Ok(Self::Never),
            _ => Err("--color requires auto, always, or never".into()),
        }
    }
    pub fn enabled(self, tty: bool) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => {
                tty && std::env::var_os("NO_COLOR").is_none()
                    && std::env::var("TERM").as_deref() != Ok("dumb")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn palette_and_overlay() {
        for name in [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ] {
            for name in [name.to_owned(), format!("bright_{name}")] {
                assert!(Color::Name(name).code(false).is_ok());
            }
        }
        assert_eq!(Color::Index(255).code(true).unwrap(), "48;5;255");
        assert_eq!(
            Color::Name("#12aBff".into()).code(false).unwrap(),
            "38;2;18;171;255"
        );
        assert!(Color::Name("#zzzzzz".into()).code(false).is_err());
        let mut style = Style {
            bold: Some(true),
            fg: Some(Color::Name("cyan".into())),
            ..Style::default()
        };
        style.overlay(&Style {
            bold: Some(false),
            ..Style::default()
        });
        assert_eq!(style.paint("a", false), "a");
        assert_eq!(style.paint("a", true), "\x1b[36;22ma\x1b[0m");
        assert_eq!(Style::default().paint("plain", true), "plain");
    }
}
