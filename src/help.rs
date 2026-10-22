//! The help text: what each column and mark means and why counts look low.
//! It is the one source of the TUI's help overlay (`H`), the legend under
//! `mazo usage` and `mazo unused`, and the HTML report's legend.

/// What each column of the usage table means, in its order.
pub const USAGE_COLUMNS: [(&str, &str); 12] = [
    ("skill", "the skill's name; (project) marks a project skill"),
    ("sess", "distinct sessions with a strong load (slash, tool or typed) in the window"),
    ("slash", "loads the user invoked: /name in Claude, $name in Codex, a <skill> block in Pi"),
    ("tool", "loads through Claude's Skill tool"),
    ("typed", "/name lines in Claude's history.jsonl whose transcript is gone"),
    ("read", "reads of its SKILL.md, a weak load"),
    ("ref", "reads of another Markdown file in its directory, a weak load"),
    ("agents", "the agents that loaded it: C Claude, X Codex, P Pi"),
    ("last", "its last strong load: today or Nd ago"),
    ("30d", "its strong loads per day over the last 30 days, · for none"),
    ("grade", "the latest /grade score, then its stale mark"),
    ("audit", "the latest audit's verdict, else the vet mazo add recorded, else not judged"),
];

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

/// The marks and glyphs in the cells, and what each means.
pub const MARKS: [(&str, &str); 5] = [
    ("agents", "C Claude, X Codex, P Pi"),
    ("·", "no events that day (a grey tick in HTML)"),
    ("+N", "commits since the grade"),
    ("+wip", "uncommitted changes since the grade"),
    ("(project)", "a project skill, counted apart"),
];

/// The questions the help answers, each with its answer's lines.
pub const FAQ: [(&str, &[&str]); 6] = [
    (
        "Why are the counts lower than I expected?",
        &[
            "The w window: counts cover only the window in the top bar; w widens it to 60 days, then all.",
            "Log coverage: mazo reads only the logs on disk; the coverage line says how far back each home goes.",
            "sess counts distinct sessions, not loads: five loads in one session are 1.",
            "Reads and refs are weak loads, excluded from sess, last and 30d.",
            "Authoring reads, in sessions that edit or grade the skill, are hidden until A shows them.",
            "Probe sessions, from Claude's other entrypoints, are dropped; mazo usage --all counts them.",
            "Project rows: a skill both global and in the project has one row per scope, counted apart.",
        ],
    ),
    (
        "What does +N or +wip after a grade mean?",
        &["The grade is stale: N commits changed the skill after it, or the skill has uncommitted changes."],
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
            "Codex still lists the skill's name and description in every prompt; the policy only stops Codex loading it on its own.",
        ],
    ),
];

/// The [`MARKS`] as one line, each `mark meaning`, joined with `sep`.
pub fn legend(sep: &str) -> String {
    MARKS.iter().map(|(mark, meaning)| format!("{mark} {meaning}")).collect::<Vec<_>>().join(sep)
}

/// The help as lines: Columns, the unused list's first when `unused_first`,
/// then Marks and glyphs, then the FAQ. A heading has no indent, an entry two
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
    }
    out.extend(["".into(), "Marks and glyphs".into()]);
    out.extend(MARKS.iter().map(|(mark, meaning)| format!("  {mark}  {meaning}")));
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
    fn the_current_screens_columns_come_first() {
        let first_table = |unused_first| lines(unused_first)[1].clone();
        assert_eq!(first_table(false), "  usage");
        assert_eq!(first_table(true), "  unused");
    }
}
