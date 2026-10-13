//! The shared reader grammar (SPEC §4.5). Stub: implemented by the readcmd task.

/// What a shell command string does to files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reads {
    /// Only reader verbs (and pass-through pipeline stages): the files read, in order.
    Load(Vec<String>),
    /// A recognised non-reader (`rg`, `grep`, `wc`, `ls`, `find`, `git`, `apply_patch`,
    /// or a pipeline whose first stage is not a reader).
    NotLoad,
    /// Anything else; a caller counts it as unparsed if it names a skill file.
    Unknown,
}

pub fn reads(_cmd: &str) -> Reads {
    todo!("reader grammar")
}
