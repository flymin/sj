use crate::util::Result;
use glob::{MatchOptions, Pattern};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Cow;

#[derive(Clone, Debug)]
struct KeyPattern(Pattern);
impl Serialize for KeyPattern {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_str())
    }
}
impl<'de> Deserialize<'de> for KeyPattern {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Pattern::new(&text)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Entries {
    include: Vec<KeyPattern>,
    exclude: Vec<KeyPattern>,
    separator: String,
    key_separator: String,
}
impl Default for Entries {
    fn default() -> Self {
        Self {
            include: Vec::new(),
            exclude: Vec::new(),
            separator: ",".into(),
            key_separator: "=".into(),
        }
    }
}
impl Entries {
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("separator", &self.separator),
            ("key_separator", &self.key_separator),
        ] {
            if value.is_empty() || value.chars().any(char::is_control) {
                return Err(format!(
                    "entries.{name} must be nonempty without control characters"
                ));
            }
        }
        if self.separator == self.key_separator {
            return Err("entries.separator and entries.key_separator must differ".into());
        }
        Ok(())
    }

    pub fn display<'a>(&self, value: &'a str) -> Cow<'a, str> {
        if self.include.is_empty() && self.exclude.is_empty() {
            return Cow::Borrowed(value);
        }
        let matches = |patterns: &[KeyPattern], key: &str| {
            patterns.iter().any(|pattern| {
                pattern.0.matches_with(
                    key,
                    MatchOptions {
                        case_sensitive: true,
                        require_literal_separator: false,
                        require_literal_leading_dot: false,
                    },
                )
            })
        };
        Cow::Owned(
            value
                .split(&self.separator)
                .map(str::trim)
                .filter(|entry| {
                    if entry.is_empty() {
                        return false;
                    }
                    let key = entry
                        .split_once(&self.key_separator)
                        .map_or(*entry, |(key, _)| key)
                        .trim();
                    (self.include.is_empty() || matches(&self.include, key))
                        && !matches(&self.exclude, key)
                })
                .collect::<Vec<_>>()
                .join(&self.separator),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(text: &str) -> Entries {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn includes_keys_with_globs_and_exclusion_wins() {
        let entries = rules("include=['cpu','mem','gres/*']\nexclude=['gres/*:hidden']");
        assert_eq!(
            entries.display(
                "cpu=8,mem=32G,node=1,billing=8,gres/gpu=2,gres/gpu:demo=2,gres/gpu:hidden=1"
            ),
            "cpu=8,mem=32G,gres/gpu=2,gres/gpu:demo=2"
        );
        assert_eq!(entries.display("CPU=8,vcpu=4,cpu_extra=1"), "");
        assert_eq!(
            rules("include=['c?u','mem[12]']").display("cpu=1,cxu=2,mem1=3,mem3=4"),
            "cpu=1,cxu=2,mem1=3"
        );
        assert_eq!(rules("include=['[!x]*']").display("x=1,cpu=2"), "cpu=2");
    }

    #[test]
    fn separators_bare_keys_and_values_preserve_order_and_duplicates() {
        let entries = rules("exclude=['drop']\nseparator=';'\nkey_separator=':'");
        assert_eq!(
            entries.display(" a:one:two ;drop:3; a:4;flag ; ;other:5 "),
            "a:one:two;a:4;flag;other:5"
        );
        let entries = rules("include=['alpha','beta']\nseparator=' | '\nkey_separator='=>'");
        assert_eq!(
            entries.display("alpha=>x=>y | no=>1 | beta=>2"),
            "alpha=>x=>y | beta=>2"
        );
        assert_eq!(entries.display(""), "");
        assert_eq!(rules("exclude=['*']").display("cpu=8,flag"), "");
    }

    #[test]
    fn empty_patterns_leave_original_text_untouched() {
        let entries = Entries::default();
        assert_eq!(entries.display(" a=1,  b=2 ,"), " a=1,  b=2 ,");
        assert!(matches!(entries.display("a=1"), Cow::Borrowed(_)));
    }

    #[test]
    fn invalid_patterns_types_and_separators_are_rejected() {
        for text in [
            "include=['[']",
            "exclude=[3]",
            "include='cpu'",
            "unknown=true",
        ] {
            assert!(toml::from_str::<Entries>(text).is_err(), "{text}");
        }
        for text in [
            "separator=''",
            "key_separator=''",
            "separator='='",
            "separator=\"\\n\"",
            "key_separator=\"\\t\"",
        ] {
            assert!(rules(text).validate().is_err(), "{text}");
        }
    }
}
