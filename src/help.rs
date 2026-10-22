//! The help text: what each column and mark means and why counts look low.
//! It is the one source of the TUI's help overlay (`H`), the legend under
//! `mazo usage` and `mazo unused`, and the HTML report's legend.

/// What each column of the usage table means, in its order.
pub const USAGE_COLUMNS: [(&str, &str); 15] = [
    ("skill", "the skill's name; (project) marks a project skill"),
    ("sess", "distinct sessions with a strong load (slash, tool or typed) in the window"),
    ("7d", "distinct sessions with a strong load over the last 7 days, whatever the window"),
    ("30d", "distinct sessions with a strong load over the last 30 days, whatever the window"),
    (
        "trend",
        "30d against the 30 days before: ↑ up 20% or more, ↓ down 20% or more, → within 20%, new from none, blank for none in either",
    ),
    ("slash", "loads the user invoked: /name in Claude, $name in Codex, a <skill> block in Pi"),
    ("tool", "loads through Claude's Skill tool"),
    ("typed", "/name lines in Claude's history.jsonl whose transcript is gone"),
    ("read", "reads of its SKILL.md, a weak load"),
    ("ref", "reads of another Markdown file in its directory, a weak load"),
    ("agents", "the agents that loaded it: C Claude, X Codex, P Pi"),
    ("ctx", "what each agent it is linked into keeps of it in context: an agent letter, then its glyph"),
    ("last", "its last strong load: today or Nd ago"),
    ("grade", "the latest /grade score, then its stale mark"),
    ("audit", "the latest audit's verdict, else the vet mazo add recorded, else not judged"),
];

/// What the TUI's usage table does on a narrow terminal, said under its columns.
pub const NARROW: &str = "on a narrow terminal some columns hide in priority order, typed first, \
    and slash and tool fold into via, shown as slash/tool such as 4/78";

/// What each column of the unused list means, in its order.
pub const UNUSED_COLUMNS: [(&str, &str); 3] = [
    ("skill", "a skill with no strong load in the window"),
    (
        "status",
        "unused: nothing holds it; held: a skill that cites it was used; new: first committed inside the window",
    ),
    (
        "notes",
        "held by its holders; cluster: unused skills that cite it; named by: used skills that name it in prose; weak-only: only weak loads",
    ),
];

/// The marks and glyphs in the cells, what each means, and how the HTML
/// report says it when it draws the mark otherwise.
pub const MARKS: [(&str, &str, Option<&str>); 7] = [
    ("agents", "C Claude, X Codex, P Pi", None),
    ("●", "name and description in context: Claude on, every Codex skill, Pi by default", None),
    ("◐", "name only in context: Claude name-only", None),
    (
        "○",
        "explicit invocation only: Claude user-invocable-only or disable-model-invocation, Pi disable-model-invocation",
        None,
    ),
    ("×", "off: Claude off", None),
    ("stale", "the skill changed since the grade", None),
    ("(project)", "a project skill, counted apart", None),
];

/// The questions the help answers, each with its answer's lines.
pub const FAQ: [(&str, &[&str]); 7] = [
    (
        "Why are the counts lower than I expected?",
        &[
            "The w window: counts cover only the window in the top bar; w cycles it through 30 days, 60 days and all.",
            "Log coverage: mazo reads only the logs on disk; the coverage line says how far back each home goes.",
            "sess counts distinct sessions, not loads: five loads in one session are 1.",
            "Reads and refs are weak loads, excluded from sess, 7d, 30d, trend and last.",
            "Authoring reads, in sessions that edit or grade the skill, are hidden until A shows them.",
            "Probe sessions, from Claude's other entrypoints, are dropped; mazo usage --all counts them.",
            "Project rows: a skill both global and in the project has one row per scope, counted apart.",
        ],
    ),
    (
        "What does stale after a grade mean?",
        &[
            "The skill's content changed after the grade: the content_hash the grade recorded differs from the skill's, or the grade recorded none.",
        ],
    ),
    (
        "Why does a skill say not judged?",
        &[
            "No audit has run on it, and mazo add never vetted it: a trusted source or a skill of your own is not vetted.",
            "x audits it.",
        ],
    ),
    ("What do the agent letters mean?", &["C is Claude, X is Codex and P is Pi."]),
    (
        "Grade, retro or audit?",
        &[
            "grade is a static review of the SKILL.md: is it well written.",
            "retro judges one run in context, right after it ran.",
            "audit judges many past runs offline and takes the retros as input; a retro feeds the audit, it does not replace it.",
        ],
    ),
    (
        "Why does Codex's allow_implicit_invocation: false save no context?",
        &[
            "It stops implicit selection, so Codex never loads the skill on its own, but Codex still lists its name and description in every prompt: its ctx glyph stays ●.",
        ],
    ),
    (
        "What is the tok in context in the top bar?",
        &[
            "Each agent's skill listing: name plus description characters of its ● skills and the name of its ◐ ones, divided by 4; ○ and × cost nothing.",
            "Claude lists skills within 1% of its context window and Codex within 2%; the context_window config key sets the window, 200000 by default.",
            "Past its budget an agent trims the descriptions silently, so an agent's total turns the warning colour at 80% of it. Pi has no budget.",
        ],
    ),
];

/// The [`MARKS`] as one line, each `mark meaning`, joined with `; `; in
/// the HTML report's words when `html`.
pub fn legend(html: bool) -> String {
    let entry = |(mark, meaning, in_html): &(&str, &str, Option<&str>)| match in_html {
        Some(said) if html => said.to_string(),
        _ => format!("{mark} {meaning}"),
    };
    MARKS.iter().map(entry).collect::<Vec<_>>().join("; ")
}

/// The help as lines: Columns, the unused list's first when `unused_first`,
/// the usage list's followed by the [`NARROW`] note, then Marks and glyphs, then the FAQ. A heading has no indent, an entry two
/// spaces, an answer four.
pub fn lines(unused_first: bool) -> Vec<String> {
    let mut tables = [("usage", &USAGE_COLUMNS[..]), ("unused", &UNUSED_COLUMNS[..])];
    if unused_first {
        tables.reverse();
    }
    let mut out = vec!["Columns".to_string()];
    for (table, columns) in tables {
        out.push(format!("  {table}"));
        out.extend(columns.iter().map(|(name, meaning)| format!("    {name}  {meaning}")));
        if table == "usage" {
            out.push(format!("    {NARROW}"));
        }
    }
    out.extend(["".into(), "Marks and glyphs".into()]);
    out.extend(MARKS.iter().map(|(mark, meaning, _)| format!("  {mark}  {meaning}")));
    out.extend(["".into(), "FAQ".into()]);
    for (question, answer) in FAQ {
        out.push(format!("  {question}"));
        out.extend(answer.iter().map(|a| format!("    {a}")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{UNUSED_HEADER, USAGE_HEADER};

    #[test]
    fn the_columns_match_the_table_headers_in_order() {
        let names = |c: &[(&str, &str)]| c.iter().map(|(n, _)| n.to_string()).collect::<Vec<_>>();
        let heads = |h: &[(&str, crate::render::Align)]| h.iter().map(|(n, _)| n.to_string()).collect::<Vec<_>>();
        assert_eq!(names(&USAGE_COLUMNS), heads(&USAGE_HEADER));
        assert_eq!(names(&UNUSED_COLUMNS), heads(&UNUSED_HEADER));
    }

    #[test]
    fn the_trend_column_says_its_twenty_percent_threshold() {
        assert!(lines(false).iter().any(|l| l.starts_with("    trend  ") && l.contains("20%")));
    }

    #[test]
    fn the_current_screens_columns_come_first() {
        let first_table = |unused_first| lines(unused_first)[1].clone();
        assert_eq!(first_table(false), "  usage");
        assert_eq!(first_table(true), "  unused");
    }

    #[test]
    fn the_narrow_note_follows_the_usage_columns() {
        for unused_first in [false, true] {
            let lines = lines(unused_first);
            let audit = lines.iter().position(|l| l.starts_with("    audit  ")).unwrap();
            assert_eq!(lines[audit + 1], format!("    {NARROW}"));
        }
    }
}
