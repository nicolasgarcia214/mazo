//! The PolicyLoader port (SPEC §5.4). Stub: implemented by the strict_yaml task.

/// A loaded YAML value, typed as PyYAML's SafeLoader with literal-only booleans types it.
#[derive(Debug, Clone, PartialEq)]
pub enum Yaml {
    Null,
    Bool(bool),
    Int(String),
    Float(String),
    Str(String),
    /// `YYYY-MM-DD` (date) or a full timestamp, as written.
    Timestamp(String),
    Binary(Vec<u8>),
    Seq(Vec<Yaml>),
    Map(Vec<(Yaml, Yaml)>),
}

impl Yaml {
    /// The value under a string key of a mapping.
    pub fn get(&self, key: &str) -> Option<&Yaml> {
        match self {
            Yaml::Map(pairs) => pairs.iter().find(|(k, _)| matches!(k, Yaml::Str(s) if s == key)).map(|(_, v)| v),
            _ => None,
        }
    }
}

pub fn load(_text: &str) -> crate::Result<Yaml> {
    todo!("strict yaml")
}

pub fn frontmatter(_text: &str) -> crate::Result<Yaml> {
    todo!("frontmatter")
}
