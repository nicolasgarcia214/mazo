use crate::config;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reads {
    Load(Vec<String>),
    NotLoad,
    Unknown,
}

const NON_READERS: [&str; 7] = ["rg", "grep", "wc", "ls", "find", "git", "apply_patch"];

pub fn reads(cmd: &str) -> Reads {
    let Some(segments) = lex(cmd) else { return Reads::Unknown };
    let mut files: Vec<String> = Vec::new();
    let mut base: Option<String> = None;
    let mut base_unknown = false;
    let mut not_load = false;
    for pipeline in &segments {
        let stages: Vec<Vec<String>> = pipeline.iter().map(|s| strip(s)).collect();
        let Some(first) = stages.first() else { continue };
        if first.is_empty() {
            continue;
        }
        if first[0] == "cd" && stages.len() == 1 {
            match first.get(1..) {
                Some([dir]) if plain(dir) => {
                    base = Some(join(base.as_deref(), &expand_home(dir)));
                    continue;
                }
                Some([_]) => {
                    base_unknown = true;
                    continue;
                }
                _ => return Reads::Unknown,
            }
        }
        match reader_files(first) {
            Some(paths) => {
                if !stages[1..].iter().all(|s| pass_through(s)) {
                    not_load = true;
                    continue;
                }
                if paths.iter().any(|p| !plain(p) || (base_unknown && !p.starts_with(['/', '~']))) {
                    return Reads::Unknown;
                }
                for p in paths {
                    let p = join(base.as_deref(), &expand_home(&p));
                    if !files.contains(&p) {
                        files.push(p);
                    }
                }
            }
            None if stages.len() > 1 || NON_READERS.contains(&first[0].as_str()) => not_load = true,
            None => return Reads::Unknown,
        }
    }
    match (not_load, files.is_empty()) {
        (true, false) | (false, true) => Reads::Unknown,
        (true, true) => Reads::NotLoad,
        (false, false) => Reads::Load(files),
    }
}

fn lex(cmd: &str) -> Option<Vec<Vec<Vec<String>>>> {
    let mut segments = vec![vec![Vec::new()]];
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = cmd.chars().peekable();
    fn end_word(word: &mut String, in_word: &mut bool, segments: &mut [Vec<Vec<String>>]) {
        if *in_word {
            segments
                .last_mut()
                .and_then(|s| s.last_mut())
                .expect("lex starts with one pipeline of one command and never empties one")
                .push(std::mem::take(word));
            *in_word = false;
        }
    }
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        ch => word.push(ch),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => word.push(chars.next()?),
                        '$' | '`' => return None,
                        ch => word.push(ch),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next()? {
                    '\n' => {}
                    ch => word.push(ch),
                }
            }
            ' ' | '\t' => end_word(&mut word, &mut in_word, &mut segments),
            '\n' | ';' => {
                end_word(&mut word, &mut in_word, &mut segments);
                segments.push(vec![Vec::new()]);
            }
            '&' => {
                if chars.next_if_eq(&'&').is_none() {
                    if word.ends_with('>') {
                        word.push('&');
                        continue;
                    }
                    return None;
                }
                end_word(&mut word, &mut in_word, &mut segments);
                segments.push(vec![Vec::new()]);
            }
            '|' => {
                if chars.peek() == Some(&'|') {
                    return None;
                }
                end_word(&mut word, &mut in_word, &mut segments);
                segments.last_mut().expect("lex starts with one pipeline and never removes one").push(Vec::new());
            }
            '(' | ')' | '`' | '{' | '}' | '<' => return None,
            ch => {
                in_word = true;
                word.push(ch);
            }
        }
    }
    end_word(&mut word, &mut in_word, &mut segments);
    Some(segments)
}

fn strip(words: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip_next = false;
    for w in words {
        if skip_next {
            skip_next = false;
            continue;
        }
        if out.is_empty() && w.contains('=') && !w.starts_with('-') && !w.starts_with('=') {
            continue;
        }
        let redirect = w.trim_start_matches(|c: char| c.is_ascii_digit());
        if redirect.starts_with('>') {
            if redirect == ">" || redirect == ">>" {
                skip_next = true;
            }
            continue;
        }
        out.push(w.clone());
    }
    out
}

fn reader_files(words: &[String]) -> Option<Vec<String>> {
    let words = if words.first().is_some_and(|w| w == "command") { &words[1..] } else { words };
    let (verb, args) = words.split_first()?;
    let verb = if verb.starts_with('/') { verb.rsplit('/').next()? } else { verb.as_str() };
    let files = match verb {
        "cat" | "bat" | "less" | "nl" => flags_then_files(args, &[])?,
        "head" | "tail" => flags_then_files(args, &["-n", "-c"])?,
        "sed" => sed_files(args)?,
        _ => return None,
    };
    (!files.is_empty()).then_some(files)
}

fn flags_then_files(args: &[String], with_value: &[&str]) -> Option<Vec<String>> {
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            files.extend(args[i + 1..].iter().cloned());
            break;
        }
        if a.starts_with('-') && a.len() > 1 {
            if with_value.contains(&a.as_str()) {
                i += 1;
            }
        } else if a.starts_with('+') && files.is_empty() {
        } else {
            files.push(a.clone());
        }
        i += 1;
    }
    Some(files)
}

fn sed_files(args: &[String]) -> Option<Vec<String>> {
    let mut quiet = false;
    let mut script = None;
    let mut files = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-n" | "--quiet" | "--silent" => quiet = true,
            "-e" => {
                script = Some(args.get(i + 1)?.clone());
                i += 1;
            }
            _ if a.starts_with('-') => return None,
            _ if script.is_none() => script = Some(a.to_string()),
            _ => files.push(a.to_string()),
        }
        i += 1;
    }
    (quiet && script.is_some_and(|s| range_print(&s))).then_some(files)
}

fn range_print(script: &str) -> bool {
    let addr = |a: &str| a == "$" || (!a.is_empty() && a.bytes().all(|b| b.is_ascii_digit()));
    let one = |cmd: &str| {
        let Some(range) = cmd.trim().strip_suffix('p') else { return false };
        match range.split_once(',') {
            Some((a, b)) => addr(a) && addr(b),
            None => addr(range),
        }
    };
    let s = script.trim().trim_end_matches(';');
    !s.is_empty() && s.split(';').all(one)
}

fn pass_through(words: &[String]) -> bool {
    let Some((verb, args)) = words.split_first() else { return false };
    match verb.as_str() {
        "nl" | "cat" => args.iter().all(|a| a.starts_with('-')),
        "head" | "tail" => flags_then_files(args, &["-n", "-c"]).is_some_and(|f| f.is_empty()),
        "sed" => sed_files(args).is_some_and(|f| f.is_empty()),
        _ => false,
    }
}

fn plain(p: &str) -> bool {
    let rest = p.strip_prefix("~/").unwrap_or(p);
    !rest.contains(['$', '*', '?', '[', '~'])
}

fn expand_home(p: &str) -> String {
    config::home_dir().map_or_else(|_| p.to_string(), |home| config::expand_tilde(p, &home))
}

fn join(base: Option<&str>, p: &str) -> String {
    match base {
        Some(b) if !p.starts_with('/') => format!("{}/{p}", b.trim_end_matches('/')),
        _ => p.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(v: &[&str]) -> Reads {
        Reads::Load(v.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn reader_verbs_load_their_files() {
        assert_eq!(reads("cat /s/grade/SKILL.md"), load(&["/s/grade/SKILL.md"]));
        assert_eq!(reads("sed -n '1,220p' /s/a/SKILL.md"), load(&["/s/a/SKILL.md"]));
        assert_eq!(reads("sed -n 1,200p SKILL.md"), load(&["SKILL.md"]));
        assert_eq!(reads("head -n 40 x/SKILL.md"), load(&["x/SKILL.md"]));
        assert_eq!(reads("nl -ba /s/a/SKILL.md"), load(&["/s/a/SKILL.md"]));
        assert_eq!(reads("bat --plain /s/a/SKILL.md"), load(&["/s/a/SKILL.md"]));
        assert_eq!(reads("sed -n '1,80p;120,300p' /s/a/x.sh"), load(&["/s/a/x.sh"]));
        assert_eq!(reads("/usr/bin/cat /s/a/SKILL.md"), load(&["/s/a/SKILL.md"]));
        assert_eq!(reads("command cat /s/a/SKILL.md"), load(&["/s/a/SKILL.md"]));
        let home = std::env::var("HOME").unwrap();
        assert_eq!(reads("cat ~/s/a/SKILL.md"), load(&[&format!("{home}/s/a/SKILL.md")]));
    }

    #[test]
    fn chains_and_chunks_collapse_to_distinct_files() {
        assert_eq!(reads("sed -n '1,220p' /s/a/SKILL.md && sed -n '220,440p' /s/a/SKILL.md"), load(&["/s/a/SKILL.md"]));
        assert_eq!(
            reads("cat /s/a/SKILL.md; cat /s/b/references/x.md"),
            load(&["/s/a/SKILL.md", "/s/b/references/x.md"])
        );
        assert_eq!(reads("cd /s/a && sed -n '1,80p' SKILL.md"), load(&["/s/a/SKILL.md"]));
        assert_eq!(reads("cat /s/a/SKILL.md 2>/dev/null"), load(&["/s/a/SKILL.md"]));
    }

    #[test]
    fn pipelines_load_only_through_pass_through_stages() {
        assert_eq!(reads("sed -n '1,200p' /s/a/SKILL.md | nl -ba"), load(&["/s/a/SKILL.md"]));
        assert_eq!(reads("cat /s/a/SKILL.md | head -50"), load(&["/s/a/SKILL.md"]));
        assert_eq!(reads("sed -n '1,200p' /s/a/SKILL.md | wc -l"), Reads::NotLoad);
        assert_eq!(reads("cat /s/a/SKILL.md | rg foo"), Reads::NotLoad);
        assert_eq!(reads("rg -n foo /s | head"), Reads::NotLoad);
    }

    #[test]
    fn non_readers_and_unknowns() {
        assert_eq!(reads("rg -n blast-radius /s/a/SKILL.md"), Reads::NotLoad);
        assert_eq!(reads("git diff /s/a/SKILL.md"), Reads::NotLoad);
        assert_eq!(reads("wc -l /s/*/SKILL.md"), Reads::NotLoad);
        assert_eq!(reads("awk 'NR<10' /s/a/SKILL.md"), Reads::Unknown);
        assert_eq!(reads("python -c 'print(open(\"SKILL.md\").read())'"), Reads::Unknown);
        assert_eq!(reads("cat /s/*/SKILL.md"), Reads::Unknown);
        assert_eq!(reads("cat \"$HOME/s/SKILL.md\""), Reads::Unknown);
        assert_eq!(reads("sed -i 's/a/b/' /s/a/SKILL.md"), Reads::Unknown);
        assert_eq!(reads("for f in a b; do cat $f; done"), Reads::Unknown);
        assert_eq!(reads("ls /s && cat /s/a/SKILL.md"), Reads::Unknown);
        assert_eq!(reads("cd $D && cat SKILL.md"), Reads::Unknown);
        assert_eq!(reads("cat /s/a/x.md; cd $D/scope; cat /s/a/SKILL.md"), load(&["/s/a/x.md", "/s/a/SKILL.md"]));
        let home = std::env::var("HOME").unwrap();
        assert_eq!(reads("cd ~/s && cat a/SKILL.md"), load(&[&format!("{home}/s/a/SKILL.md")]));
    }
}
