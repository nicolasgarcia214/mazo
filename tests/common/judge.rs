//! The answers a stub judge gives: a valid stage-1 and stage-2 answer.

/// A judge answer citing `line` in every finding.
pub fn answer(line: impl Into<serde_json::Value>) -> serde_json::Value {
    let line = line.into();
    serde_json::json!({
        "asked": "Tidy a short text.",
        "followed": "yes",
        "outcome": "achieved",
        "redone": false,
        "strengths": [{"line": line, "text": "Loaded the skill first."}],
        "frictions": [],
        "notes": "",
    })
}

/// A stage-2 answer citing `session` everywhere a session is cited.
pub fn skill_answer(session: &str) -> serde_json::Value {
    serde_json::json!({
        "verdict": "Tidies short texts well; long ones need a second pass.",
        "patterns": ["Used on short prose."],
        "works": [{"text": "Loaded before editing.", "sessions": [session]}],
        "failures": [],
        "improvements": [
            {"change": "Add a length check.", "reason": "Long texts were redone.", "sessions": [session]},
            {"change": "Name the tone first.", "reason": "Users asked twice.", "sessions": [session]},
            {"change": "Cut the preamble rule.", "reason": "It was skipped.", "sessions": [session]},
            {"change": "Fourth idea.", "reason": "Least valuable.", "sessions": [session]},
        ],
        "versus_grades": "",
        "examples": [{"situation": "A short note.", "asked": "Tidy the note.", "happened": "It was tidied.", "tip": "Say the tone.", "session": session}],
        "for": "Tidies short prose so it reads plainly.",
        "try": ["Tidy a release note.", "Tidy a PR description."],
    })
}
