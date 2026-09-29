//! Language selection and desktop messages.

mod messages;

use std::{fmt, path::Path, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::{Result, preferences::Preferences, workspace::BackupStore};

/// A supported display language. Stored data is language independent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Language {
    /// English, also used when a translation is unavailable.
    #[default]
    #[serde(rename = "en")]
    English,
    /// Simplified Chinese.
    #[serde(rename = "zh-CN")]
    Chinese,
}

impl Language {
    /// Returns the language identifier used by saved preferences.
    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Chinese => "zh-CN",
        }
    }

    /// Returns the language's name in its own language.
    pub fn name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Chinese => "简体中文",
        }
    }

    /// Resolves explicit, environment, saved and system preferences, in that order.
    /// An unsupported explicit language is an error; unsupported system locales use English.
    pub fn resolve(explicit: Option<&str>, directory: Option<&Path>) -> Result<Self> {
        if let Some(value) = explicit {
            return value.parse();
        }
        if let Ok(value) = std::env::var("GITWATCH_LANG")
            && !value.is_empty()
        {
            return value.parse();
        }
        let directory = match directory {
            Some(path) => path.to_owned(),
            None => BackupStore::default_directory()?,
        };
        if let Some(language) = Self::saved(&directory)? {
            return Ok(language);
        }
        Ok(sys_locale::get_locale()
            .and_then(|locale| locale.parse().ok())
            .unwrap_or_default())
    }

    fn saved(directory: &Path) -> Result<Option<Self>> {
        Ok(Preferences::load(directory)?.language)
    }

    /// Saves the language in the local store without opening or modifying a Git repository.
    pub fn save(self, directory: &Path) -> Result<()> {
        Preferences::update(directory, |preferences| preferences.language = Some(self))
    }

    /// Looks up a message, falling back to its English source text.
    pub fn text(self, source: &str) -> &str {
        if self == Self::Chinese {
            messages::TEXT
                .iter()
                .find(|(key, _)| *key == source || *key == source.trim_end_matches('.'))
                .map(|(_, value)| *value)
                .unwrap_or(source)
        } else {
            source
        }
    }

    /// Formats numbered placeholders without interpreting the inserted values.
    pub fn format(self, source: &str, values: &[&str]) -> String {
        substitute(self.text(source), values)
    }

    /// Translates application diagnostics, preserving unknown external diagnostics and paths.
    pub fn error(self, error: &str) -> String {
        if self == Self::English {
            return error.into();
        }
        if self.text(error) != error {
            return self.text(error).into();
        }
        if let Some(values) = captures("Git {} failed (exit {}): {}", error) {
            return self.format(
                "Git {0} failed (exit {1}): {2}",
                &[values[0], values[1], &self.error(values[2])],
            );
        }
        for (source, translated) in messages::ERRORS {
            if let Some(values) = captures(source, error) {
                return substitute(translated, &values);
            }
        }
        if let Some((context, cause)) = error.split_once(": ") {
            let translated = self.text(context);
            if translated != context {
                return format!("{translated}: {}", self.error(cause));
            }
        }
        error.into()
    }
}

impl FromStr for Language {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let value = value
            .split(['.', '@'])
            .next()
            .unwrap_or(value)
            .replace('_', "-");
        let primary = value.split('-').next().unwrap_or("");
        if primary.eq_ignore_ascii_case("en") {
            Ok(Self::English)
        } else if primary.eq_ignore_ascii_case("zh") {
            Ok(Self::Chinese)
        } else {
            anyhow::bail!("Unsupported language; use en or zh-CN")
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

fn substitute(template: &str, values: &[&str]) -> String {
    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        output.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find('}') else { break };
        match rest[1..end]
            .parse::<usize>()
            .ok()
            .and_then(|index| values.get(index))
        {
            Some(value) => output.push_str(value),
            None => output.push_str(&rest[..=end]),
        }
        rest = &rest[end + 1..];
    }
    output.push_str(rest);
    output
}

fn captures<'a>(template: &str, text: &'a str) -> Option<Vec<&'a str>> {
    let mut rest = text;
    let mut parts = template.split("{}");
    rest = rest.strip_prefix(parts.next()?)?;
    let mut values = Vec::new();
    let suffixes: Vec<_> = parts.collect();
    for (index, suffix) in suffixes.iter().enumerate() {
        if index + 1 == suffixes.len() {
            values.push(rest.strip_suffix(suffix)?);
            rest = "";
        } else {
            let end = rest.find(suffix)?;
            values.push(&rest[..end]);
            rest = &rest[end + suffix.len()..];
        }
    }
    rest.is_empty().then_some(values)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Text {
    Message(&'static str, Vec<Text>),
    Value(String),
    Error(String),
}

impl Text {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Value(value) if value.is_empty())
    }

    pub fn value(value: impl fmt::Display) -> Self {
        Self::Value(value.to_string())
    }

    pub fn format(template: &'static str, values: impl IntoIterator<Item = Text>) -> Self {
        Self::Message(template, values.into_iter().collect())
    }

    pub fn render(&self, language: Language) -> String {
        match self {
            Self::Message(template, values) => {
                let values: Vec<_> = values.iter().map(|value| value.render(language)).collect();
                language.format(
                    template,
                    &values.iter().map(String::as_str).collect::<Vec<_>>(),
                )
            }
            Self::Value(value) => value.clone(),
            Self::Error(error) => language.error(error),
        }
    }
}

impl Default for Text {
    fn default() -> Self {
        Self::Value(String::new())
    }
}

impl From<&'static str> for Text {
    fn from(value: &'static str) -> Self {
        Self::Message(value, Vec::new())
    }
}

impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render(Language::English))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translation_preserves_values_and_preferences_are_local() {
        let language = Language::Chinese;
        assert_eq!("zh_Hans_CN.UTF-8".parse::<Language>().unwrap(), language);
        assert!("fr".parse::<Language>().is_err());
        assert_eq!(language.text("unknown message"), "unknown message");
        assert_eq!(
            language.format("Destination: {0}", &["{1}/Running"]),
            "目标位置：{1}/Running"
        );
        assert_eq!(
            language.error("Symbolic link is not allowed: C:/Running/{0}"),
            "不允许使用符号链接：C:/Running/{0}"
        );
        let temp = tempfile::TempDir::new().unwrap();
        language.save(temp.path()).unwrap();
        assert_eq!(Language::saved(temp.path()).unwrap(), Some(language));
        assert!(!temp.path().join("backup.git").exists());
    }

    #[test]
    fn catalog_has_unique_keys_and_matching_placeholders() {
        let mut keys = std::collections::HashSet::new();
        let pattern = regex::Regex::new(r"\{\d+\}").unwrap();
        for (source, translated) in messages::TEXT {
            assert!(keys.insert(source), "Duplicate message: {source}");
            let placeholders = |text: &str| {
                pattern
                    .find_iter(text)
                    .map(|m| m.as_str().to_owned())
                    .collect::<std::collections::BTreeSet<_>>()
            };
            assert_eq!(placeholders(source), placeholders(translated), "{source}");
            assert!(!translated.is_empty(), "{source}");
        }
    }
}
