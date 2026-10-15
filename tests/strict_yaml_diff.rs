//! Strict YAML differential (SPEC §10.5): `strict_yaml.rs` against the
//! verbatim PyYAML `PolicyLoader` in `tests/policy_loader.py`, over a synthetic
//! corpus and every skill, grade and retro file in the skills directory under
//! test (`SKIMA_TEST_SKILLS`, or `tests/fixtures/skills`).
//! Both sides must agree on accept/reject and on the typed value tree.

mod common;

use serde_json::{Value, json};
use skima::strict_yaml::{self, Yaml};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode {
    Load,
    Frontmatter,
}

/// What a corpus case must do, checked on top of agreement, so a case cannot
/// silently stop testing what its name says.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Expect {
    Accept,
    Reject,
    Either,
    /// PyYAML accepts and skima rejects: a known, accepted gap.
    Stricter,
}
use Expect::*;

struct Case {
    name: String,
    mode: Mode,
    text: String,
    expect: Expect,
}

fn synthetic() -> Vec<Case> {
    let cases: &[(&str, &str, Expect)] = &[
        // Booleans: only plain `true` / `false`.
        ("bool true", "a: true", Accept),
        ("bool false", "a: false", Accept),
        ("bool True", "a: True", Accept),
        ("bool TRUE", "a: TRUE", Accept),
        ("bool False", "a: False", Accept),
        ("bool FALSE", "a: FALSE", Accept),
        ("bool yes", "a: yes", Accept),
        ("bool Yes", "a: Yes", Accept),
        ("bool YES", "a: YES", Accept),
        ("bool no", "a: no", Accept),
        ("bool No", "a: No", Accept),
        ("bool NO", "a: NO", Accept),
        ("bool on", "a: on", Accept),
        ("bool On", "a: On", Accept),
        ("bool ON", "a: ON", Accept),
        ("bool off", "a: off", Accept),
        ("bool Off", "a: Off", Accept),
        ("bool OFF", "a: OFF", Accept),
        ("bool y", "a: y", Accept),
        ("bool n", "a: n", Accept),
        ("bool quoted true", "a: 'true'", Accept),
        ("bool double-quoted false", "a: \"false\"", Accept),
        ("bool tag true", "a: !!bool true", Accept),
        ("bool tag false", "a: !!bool false", Accept),
        ("bool tag yes", "a: !!bool yes", Reject),
        ("bool tag True", "a: !!bool True", Reject),
        ("bool tag on", "a: !!bool on", Reject),
        ("bool tag quoted true", "a: !!bool \"true\"", Accept),
        ("bool tag single-quoted false", "a: !!bool 'false'", Accept),
        ("bool tag empty", "a: !!bool", Reject),
        ("bool tag seq", "a: !!bool [true]", Reject),
        ("bool verbatim true", "a: !<tag:yaml.org,2002:bool> true", Accept),
        ("bool verbatim yes", "a: !<tag:yaml.org,2002:bool> yes", Reject),
        ("bool verbatim quoted", "a: !<tag:yaml.org,2002:bool> \"false\"", Accept),
        ("policy false", "policy:\n  allow_implicit_invocation: false\n", Accept),
        // Duplicate keys, compared after resolution.
        ("dup plain", "a: 1\na: 2", Reject),
        ("dup quoted vs plain", "a: 1\n'a': 2", Reject),
        ("dup 1 and 1.0", "1: a\n1.0: b", Reject),
        ("dup true and 1", "true: a\n1: b", Reject),
        ("dup false and 0", "false: a\n0: b", Reject),
        ("dup false and 0.0", "false: a\n-0.0: b", Reject),
        ("dup ~ and null", "~: a\nnull: b", Reject),
        ("dup null and empty", "null: a\n?\n: b", Reject),
        ("dup Null NULL", "Null: a\nNULL: b", Reject),
        ("dup 0x10 and 16", "0x10: a\n16: b", Reject),
        ("dup 010 and 8", "010: a\n8: b", Reject),
        ("dup 1:30 and 90", "1:30: a\n90: b", Reject),
        ("dup 1_000 and 1000", "1_000: a\n1000: b", Reject),
        ("dup two .nan", ".nan: a\n.NaN: b", Reject),
        ("-.nan is str", "-.nan: a\n.nan: b", Accept),
        ("explicit nan keys differ", "!!float nan: a\n!!float nan: b", Accept),
        ("dup aliased explicit nan", "&n !!float nan: a\n*n : b", Reject),
        ("dup inf", ".inf: a\n.Inf: b", Reject),
        ("inf and -inf", ".inf: a\n-.inf: b", Accept),
        ("dup big int and float", "18446744073709551616: a\n18446744073709551616.0: b", Reject),
        ("near big int and float", "18446744073709551617: a\n18446744073709551616.0: b", Accept),
        ("dup date", "2001-12-14: a\n2001-12-14: b", Reject),
        ("date vs datetime", "2001-12-14: a\n2001-12-14 00:00:00: b", Accept),
        ("dup aware instants", "2001-12-14t21:59:43.10-05:00: a\n2001-12-15 2:59:43.10Z: b", Reject),
        ("naive vs aware", "2001-12-15 2:59:43.10: a\n2001-12-15 2:59:43.10Z: b", Accept),
        ("dup binary", "? !!binary aGk=\n: a\n? !!binary aGk\n: b", Reject),
        ("binary vs str", "? !!binary aGk=\n: a\nhi: b", Accept),
        ("str 1 vs int 1", "'1': a\n1: b", Accept),
        ("dup in flow map", "{a: 1, a: 2}", Reject),
        ("dup nested", "a:\n  b: 1\n  b: 2", Reject),
        ("dup in set", "!!set {a, a}", Reject),
        // Merge and value keys.
        ("merge key", "base: &b {x: 1}\nc:\n  <<: *b\n", Reject),
        ("merge key flow", "{<<: {a: 1}}", Reject),
        ("merge value", "a: <<", Reject),
        ("merge quoted key", "'<<': 1", Accept),
        ("merge explicit tag", "!!merge x: 1", Reject),
        ("value plain", "a: =", Reject),
        ("value key", "=: 1", Reject),
        ("value quoted", "a: '='", Accept),
        ("value inside str", "a: !!str {=: x, b: c}", Either),
        ("value inside int", "a: !!int {=: '12'}", Either),
        ("yaml tag via !", "a: ! '*'", Either),
        // Ints, YAML 1.1.
        ("int decimal", "a: 15", Accept),
        ("int signed", "a: [-15, +15, -0, +0, 0]", Accept),
        ("int underscore", "a: 1_000_000", Accept),
        ("int octal", "a: 017", Accept),
        ("int octal underscore", "a: 0_17", Accept),
        ("int 08 is str", "a: 08", Accept),
        ("int 0o17 is str", "a: 0o17", Accept),
        ("int binary", "a: 0b1010", Accept),
        ("int binary empty digits", "a: 0b_", Reject),
        ("int hex", "a: 0x1F", Accept),
        ("int hex upper X is str", "a: 0X1F", Accept),
        ("int hex empty digits", "a: 0x_", Reject),
        ("int sexagesimal", "a: 1:30", Accept),
        ("int sexagesimal long", "a: -190:20:30", Accept),
        ("int sexagesimal 60 is str", "a: 1:60", Accept),
        ("int above 2^64", "a: 18446744073709551617", Accept),
        ("int huge negative", "a: -123456789012345678901234567890123456789", Accept),
        ("int 1e3 is str", "a: 1e3", Accept),
        ("int tag on str", "a: !!int abc", Reject),
        ("int tag quoted", "a: !!int '0x1F'", Accept),
        ("int tag 0o", "a: !!int '0o17'", Accept),
        ("int tag spaces", "a: !!int ' 12 '", Accept),
        ("int tag sexagesimal big part", "a: !!int '1:70'", Accept),
        ("int tag sexagesimal signed part", "a: !!int '1:-30'", Accept),
        ("int tag empty", "a: !!int", Reject),
        ("int tag float", "a: !!int 1.5", Reject),
        // Floats.
        ("float", "a: 1.5", Accept),
        ("float exp signed", "a: 1.0e+5", Accept),
        ("float exp unsigned is str", "a: 1.0e5", Accept),
        ("float leading dot", "a: .5", Accept),
        ("float trailing dot", "a: 1.", Accept),
        ("float underscore", "a: 1_000.5_5", Accept),
        ("float sexagesimal", "a: 190:20:30.15", Accept),
        ("float inf", "a: [.inf, -.Inf, +.INF]", Accept),
        ("float nan", "a: [.nan, .NaN, .NAN]", Accept),
        ("float tag on int", "a: !!float 1", Accept),
        ("float tag 1e5", "a: !!float 1e5", Accept),
        ("float tag infinity", "a: !!float -infinity", Accept),
        ("float tag junk", "a: !!float x", Reject),
        ("float negative zero", "a: -0.0", Accept),
        ("float 0.1", "a: 0.1", Accept),
        // Null.
        ("null forms", "a: [~, null, Null, NULL, nULL]", Accept),
        ("null empty", "a:", Accept),
        ("null tag on text", "a: !!null text", Accept),
        ("null tag seq", "a: !!null [x]", Reject),
        // Timestamps.
        ("date", "a: 2001-12-14", Accept),
        ("date one-digit month is str", "a: 2001-1-14", Accept),
        ("datetime canonical", "a: 2001-12-14t21:59:43.10-05:00", Accept),
        ("datetime space", "a: 2001-12-14 21:59:43.10 -5", Accept),
        ("datetime naive", "a: 2001-12-15 2:59:43.10", Accept),
        ("datetime Z", "a: 2002-12-14T00:00:00Z", Accept),
        ("datetime long fraction", "a: 2002-12-14T00:00:00.1234567+01:30", Accept),
        ("datetime no fraction digits", "a: 2002-12-14T00:00:00.", Accept),
        ("date invalid", "a: 2001-13-14", Reject),
        ("date feb 29 leap", "a: 2000-02-29", Accept),
        ("date feb 29 non-leap", "a: 1900-02-29", Reject),
        ("date year 0", "a: 0000-01-01", Reject),
        ("datetime hour 24", "a: 2001-12-14 24:00:00", Reject),
        ("datetime tz 24", "a: 2001-12-14 10:00:00 +24", Reject),
        ("timestamp tag on date", "a: !!timestamp 2001-12-14", Accept),
        ("timestamp tag one-digit", "a: !!timestamp '2001-1-2'", Accept),
        ("timestamp tag junk", "a: !!timestamp x", Reject),
        ("timestamp quoted is str", "a: '2001-12-14'", Accept),
        ("grade date", "skill: grade\ndate: 2026-10-13\nscore: 9\n", Accept),
        // Strings and quoting.
        ("str plain", "a: hello world", Accept),
        ("str tag on int", "a: !!str 12", Accept),
        ("str tag empty", "a: !!str", Accept),
        ("str tag seq", "a: !!str [x]", Reject),
        ("str quoted number", "a: '12'", Accept),
        ("str double escapes", "a: \"tab\\tnl\\nuni\\u00e9\\x41\\N\\_\\L\\P\"", Accept),
        ("str quoted tab", "a: \"x\ty\"", Accept),
        ("str quoted multiline", "a: 'one\n  two\n\n  three'", Accept),
        ("str plain multiline", "a: one\n  two\n\n  three", Accept),
        ("str literal", "a: |\n  x\n   y\n\n", Accept),
        ("str literal keep", "a: |+\n  x\n\n", Accept),
        ("str folded strip", "a: >-\n  x\n  y\n\n  z\n", Accept),
        ("str literal tab content", "a: |\n  x\n  \ty\n", Accept),
        ("str literal tab indent", "a: |\n  x\n\ty\n", Reject),
        ("str nonspecific tag quoted int", "a: ! '12'", Accept),
        ("str nonspecific tag plain", "a: ! 12", Accept),
        ("str nonspecific verbatim", "a: !<!> 12", Accept),
        ("str unicode", "a: héllo ✓ 日本", Accept),
        ("str colon inside", "a: b:c", Accept),
        ("str hash inside", "a: b#c", Accept),
        ("str comment", "a: b # c\t d", Accept),
        ("str key quoted", "\"key with: colon\": v", Accept),
        // Tabs: PyYAML rejects every tab outside quoted text, block text and comments.
        ("tab in plain scalar", "a: b\tc", Reject),
        ("tab after value", "a: b\t", Reject),
        ("tab before comment", "a: b\t# c", Reject),
        ("tab after colon", "a:\tb", Reject),
        ("tab indentation", "a:\n\tb: 1", Reject),
        ("tab only line", "a: 1\n\t\nb: 2", Reject),
        ("tab in flow", "[a,\tb]", Reject),
        ("tab after flow", "{a: 1}\t", Reject),
        ("tab in comment line", "# a\tb\na: 1", Accept),
        // Anchors and aliases.
        ("anchor alias scalar", "a: &x 1\nb: *x", Accept),
        ("anchor alias map", "a: &m {k: v}\nb: *m", Accept),
        ("anchor alias seq", "a: &s [1, 2]\nb: [*s, *s]", Accept),
        ("anchor on key", "&k a: 1\nb: *k", Accept),
        ("anchor empty node", "a: &e\nb: *e", Accept),
        ("anchor with dash and underscore", "a: &a-b_1 1\nb: *a-b_1", Accept),
        ("anchor dot name", "a: &a.b 1", Reject),
        ("anchor undefined", "a: *nope", Reject),
        ("anchor duplicate", "a: &x 1\nb: &x 2", Reject),
        ("alias as key", "&k a: 1\n*k : 2", Reject),
        ("alias explicit key", "&k a: 1\n? *k\n: 2", Reject),
        ("alias key new", "a: &k other\n*k : 1", Accept),
        // Non-scalar keys.
        ("seq key flow", "? [a, b]\n: 1", Reject),
        ("map key flow", "? {a: b}\n: 1", Reject),
        ("seq key empty", "[]: 1", Reject),
        ("set key", "? !!set {a}\n: 1", Reject),
        ("omap key", "? !!omap [a: 1]\n: 1", Reject),
        ("alias seq key", "a: &s [1]\n? *s\n: 1", Reject),
        // Collections and their tags.
        ("binary", "a: !!binary aGVsbG8=", Accept),
        ("binary multiline", "a: !!binary |\n  aGVs\n  bG8=\n", Accept),
        ("binary junk chars", "a: !!binary 'a*G-V#sbG8='", Accept),
        ("binary bad padding", "a: !!binary aGVsbG8", Reject),
        ("binary one extra", "a: !!binary aGVsb", Reject),
        ("binary data after pad", "a: !!binary 'aGk=aGk='", Reject),
        ("binary pad skipped", "a: !!binary 'aGk=x='", Accept),
        ("binary leading pad", "a: !!binary '=aGk='", Accept),
        ("binary non-ascii", "a: !!binary 'aGk=é'", Reject),
        ("set", "a: !!set {x, y, 1}", Accept),
        ("set block", "a: !!set\n  ? x\n  ? y\n", Accept),
        ("set with values", "a: !!set {x: 1}", Accept),
        ("set on seq", "a: !!set [x]", Reject),
        ("omap", "a: !!omap [x: 1, y: 2]", Accept),
        ("omap block", "a: !!omap\n  - x: 1\n  - x: 2\n", Accept),
        ("omap two items", "a: !!omap [{x: 1, y: 2}]", Reject),
        ("omap on map", "a: !!omap {x: 1}", Reject),
        ("omap seq key", "a: !!omap [[x]: 1]", Accept),
        ("omap unknown inner tag", "a: !!omap [!foo {x: 1}]", Accept),
        ("pairs", "a: !!pairs [x: 1, x: 2]", Accept),
        ("seq tag", "a: !!seq [1]", Accept),
        ("seq tag on map", "a: !!seq {x: 1}", Reject),
        ("map tag", "a: !!map {x: 1}", Accept),
        ("map tag on seq", "a: !!map [x]", Reject),
        ("map tag on scalar", "a: !!map x", Reject),
        ("nested collections", "a:\n  - b: [1, {c: d}]\n  - - e\n    - f\n", Accept),
        ("verbatim str", "a: !<tag:yaml.org,2002:str> 12", Accept),
        ("verbatim int", "a: !<tag:yaml.org,2002:int> '7'", Accept),
        ("percent-encoded tag", "a: !!%62ool true", Accept),
        ("tag directive", "%TAG !e! tag:yaml.org,2002:\n---\na: !e!int '5'", Accept),
        // Unknown tags and documents.
        ("unknown local tag", "a: !foo x", Reject),
        ("unknown core tag", "a: !!python/name:os.system x", Reject),
        ("unknown verbatim", "a: !<tag:example.com,2000:x> y", Reject),
        ("two documents", "a: 1\n---\nb: 2", Reject),
        ("two explicit documents", "---\na: 1\n...\n---\nb: 2\n", Reject),
        ("document end then content", "a: 1\n...\nb: 2", Reject),
        ("explicit document", "---\na: 1\n...\n", Accept),
        ("empty stream", "", Accept),
        ("comment only", "# nothing", Accept),
        ("bare document marker", "---", Accept),
        ("scalar document", "just text", Accept),
        ("yaml directive", "%YAML 1.1\n---\na: 1", Accept),
        // Characters.
        ("control char", "a: b\u{1}c", Reject),
        ("del char", "a: b\u{7f}", Reject),
        ("nel line break", "a: 1\u{85}b: 2", Accept),
        ("bom at start", "\u{feff}a: 1", Accept),
        ("crlf", "a: 1\r\nb: 2\r\n", Accept),
        ("cr only", "a: 1\rb: 2\r", Accept),
        ("nbsp", "a: b\u{a0}c", Accept),
        ("astral", "a: \u{1F600}", Accept),
        // Syntax errors.
        ("unclosed flow", "a: [1, 2", Reject),
        ("unclosed quote", "a: 'x", Reject),
        ("bad indentation", "a:\n  b: 1\n c: 2", Reject),
        ("mapping in plain", "a: b: c", Reject),
        ("empty key", ": a", Reject),
        ("empty flow key", "{: a}", Reject),
        ("empty key in flow seq", "[: a]", Reject),
        ("empty nested key", "x:\n  : a", Reject),
        ("explicit empty key", "? \n: a", Accept),
        ("explicit empty key comment", "? # c\n: a", Accept),
        ("explicit empty flow key", "{? : a}", Either),
        ("long key", "? aaaa\n: 1", Accept),
        ("anchor in flow seq", "a: [b, &x c, *x]", Accept),
        ("anchor on explicit key", "? &x a\n: *x", Accept),
        ("anchor on seq entry", "- &x a\n- *x", Accept),
        ("anchor followed by bracket", "a: &x[1]", Either),
        ("tab in comment after quote", "a: \"b\" # c\td", Accept),
        ("tab in comment after plain", "a: b #c\td", Accept),
        ("hash in quoted key", "\"#a\": \"b#\"", Accept),
        ("ampersand in plain", "a: b &c", Accept),
        ("ampersand in quoted", "a: ' &c'", Accept),
        ("ampersand in tag directive", "%TAG !e! tag:a&b,2000:\n---\na: !e!x 1", Reject),
        ("plain continuation with ampersand", "a: b\n  &c", Accept),
        // Known gaps: saphyr (YAML 1.2) rejects these and PyYAML accepts them.
        ("quoted continuation below indentation", "a: \"x\ny\"", Stricter),
        ("quoted tab continuation", "a: \"x\n\ty\"", Stricter),
        ("comment after quote", "a: \"b\"#c", Stricter),
        ("surrogate escape", "a: \"\\ud800\"", Stricter),
        ("nel in quoted", "a: \"x\u{85}y\"", Stricter),
        ("flow plain colon", "{a:1}", Either),
        ("flow adjacent value", "{\"a\":1}", Either),
        ("seq dash empty key", "- : a", Either),
        ("compact nested seq", "- - a\n  - b\n- c", Either),
        ("multiline key", "a\nb: c", Either),
        ("anchor before tag", "a: &x !!str 1\nb: *x", Either),
        ("tag before anchor", "a: !!str &x 1\nb: *x", Either),
        ("anchor colon", "{&a : 1}", Either),
        ("alias in flow", "a: &x 1\nb: [*x, *x]", Either),
        ("yaml 1.2 directive", "%YAML 1.2\n---\na: 1", Either),
        ("yaml 2.0 directive", "%YAML 2.0\n---\na: 1", Reject),
        ("double yaml directive", "%YAML 1.1\n%YAML 1.1\n---\na: 1", Either),
        ("escaped slash", "a: \"\\/\"", Either),
        ("escaped tab char", "a: \"\\\t\"", Either),
        ("question plain", "a: ?b", Either),
        ("dash plain", "a: -b", Either),
        ("percent plain", "a: %b", Either),
        ("at plain", "a: @b", Either),
        ("backtick plain", "a: `b", Either),
        ("doc start in plain", "a: b\n--- c", Either),
        ("block indent indicator", "a: |2\n   x\n", Either),
        ("block header comment", "a: | # c\n  x\n", Either),
        ("folded more indented", "a: >\n  x\n    y\n  z\n", Either),
        ("flow multiline", "a: [b,\n  c]", Either),
        ("flow trailing comma", "a: [b, c,]", Either),
        ("flow map trailing comma", "a: {b: 1,}", Either),
        ("flow single pair", "a: [b: 1]", Either),
        ("empty flow", "a: [ ]\nb: { }", Either),
        ("indentless seq", "a:\n- b\n- c", Either),
        ("explicit key seq", "? - a\n  - b\n: c", Either),
        ("local tag on seq", "a: !x [1]", Either),
        ("nonspecific map", "a: ! {b: 1}", Either),
        ("tag handle undeclared", "a: !e!x 1", Either),
        ("nul", "a: \"\\0\"", Either),
        ("trailing spaces", "a: b   \nc: d  ", Either),
        ("windows key", "a: 1\r\nb:\r\n  - x\r\n", Either),
        ("fffe", "a: \u{fffe}", Either),
        ("literal tab only line", "a: |\n  x\n\t\n  y\n", Either),
        ("anchor on alias line", "a: &x [1]\nb: &y *x", Either),
        ("bom mid", "a: b\u{feff}c", Either),
        ("sexagesimal float tag", "a: !!float '1:-30'", Either),
        ("ws float tag", "a: !!float ' 1.5 '", Either),
        ("! literal int", "a: ! |\n  12\n", Either),
        ("! literal timestamp", "a: ! |\n  2001-01-01\n", Either),
        ("! literal bool", "a: ! |\n  true\n", Either),
        ("!!timestamp literal", "a: !!timestamp |\n  2001-01-01\n", Either),
        ("str value map", "a: !!str {=: !!bool yes}", Either),
        ("null value map", "a: !!null {=: x, <<: y}", Either),
        ("ts value map", "a: !!timestamp {=: 2001-01-01}", Either),
        ("bool value map", "a: !!bool {=: true}", Either),
        ("explicit value tag key", "a: !!str {!!value x: y}", Either),
    ];
    let mut cases = cases.to_vec();
    let generated = generated_scalars();
    cases.extend(generated.iter().map(|(name, text)| (name.as_str(), text.as_str(), Either)));
    let mut out: Vec<Case> = cases
        .iter()
        .map(|&(name, text, expect)| Case { name: name.into(), mode: Mode::Load, text: text.into(), expect })
        .collect();

    let frontmatter: &[(&str, &str, Expect)] = &[
        ("fm plain", "---\nname: a\ndescription: b\n---\nbody\n", Accept),
        ("fm bom", "\u{feff}---\nname: a\n---\n", Accept),
        ("fm double bom", "\u{feff}\u{feff}---\nname: a\n---\n", Reject),
        ("fm crlf", "---\r\nname: a\r\n---\r\n", Accept),
        ("fm cr", "---\rname: a\r---\r", Accept),
        ("fm vt ff separators", "---\x0bname: a\x0cdescription: b\x1c---\x1d", Accept),
        ("fm unicode separators", "---\u{85}name: a\u{2028}x: y\u{2029}---", Accept),
        ("fm group separators", "---\x1ename: a\x1e---", Accept),
        ("fm no opening", "name: a\n---\n", Reject),
        ("fm opening with space", "--- \nname: a\n---\n", Reject),
        ("fm leading blank line", "\n---\nname: a\n---\n", Reject),
        ("fm missing close", "---\nname: a\n", Reject),
        ("fm close with trailing space", "---\nname: a\n--- \n", Reject),
        ("fm empty", "---\n---\n", Accept),
        ("fm empty file", "", Reject),
        ("fm only opening", "---", Reject),
        ("fm second block ignored", "---\na: 1\n---\n---\nb: [\n---\n", Accept),
        ("fm document end inside", "---\na: 1\n...\n---\n", Accept),
        ("fm yes stays str", "---\nname: a\nflag: yes\n---\n", Accept),
        ("fm duplicate", "---\nname: a\nname: b\n---\n", Reject),
        ("fm tab", "---\nname: a\tb\n---\n", Reject),
        ("fm date", "---\ndate: 2026-10-13\n---\n", Accept),
    ];
    out.extend(frontmatter.iter().map(|&(name, text, expect)| Case {
        name: name.into(),
        mode: Mode::Frontmatter,
        text: text.into(),
        expect,
    }));
    out
}

/// Plain, quoted and tagged scalars built from the characters the int,
/// float, timestamp and null resolvers care about, from a fixed-seed LCG.
fn generated_scalars() -> Vec<(String, String)> {
    const ALPHABET: &[&str] = &[
        "0",
        "0",
        "1",
        "1",
        "5",
        "7",
        "8",
        "9",
        "_",
        ":",
        ":",
        ".",
        ".",
        "-",
        "+",
        "e",
        "E",
        "x",
        "b",
        "o",
        "F",
        "nan",
        "inf",
        "Inf",
        ".nan",
        ".inf",
        "~",
        "null",
        "true",
        "=",
        "<<",
        "2001-12-14",
        "2001-1-2",
        " 10:00:00",
        "T1:02:03",
        ".5",
        " -5",
        "Z",
        "60",
        "59",
    ];
    const WRAP: &[(&str, &str)] = &[
        ("", ""),
        ("", ""),
        ("", ""),
        ("'", "'"),
        ("!!int '", "'"),
        ("!!float '", "'"),
        ("!!timestamp '", "'"),
        ("! '", "'"),
    ];
    let mut state: u64 = 0x5eed;
    let mut next = |n: usize| {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((state >> 33) as usize) % n
    };
    (0..1500)
        .map(|i| {
            let len = 1 + next(4);
            let body: String = (0..len).map(|_| ALPHABET[next(ALPHABET.len())]).collect();
            let (open, close) = WRAP[next(WRAP.len())];
            let body = if open.is_empty() { body.trim().to_string() } else { body };
            (format!("generated {i}"), format!("k: {open}{body}{close}\n? {open}{body}{close}\n: v"))
        })
        .collect()
}

/// Every input from the skills directory under test: SKILL.md, grade and retro
/// frontmatter, and `agents/openai.yaml`.
fn real_files() -> Vec<Case> {
    let root = common::real_skills();
    assert!(root.is_dir(), "{} not found", root.display());
    let mut paths: Vec<(Mode, PathBuf)> = Vec::new();
    for skill in sorted_dirs(&root) {
        for (mode, file) in [(Mode::Frontmatter, "SKILL.md"), (Mode::Load, "agents/openai.yaml")] {
            let path = skill.join(file);
            if path.is_file() {
                paths.push((mode, path));
            }
        }
    }
    for log in [".grades", ".retros"] {
        for dir in sorted_dirs(&root.join(log)) {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.extension().is_some_and(|x| x == "md"))
                .collect();
            files.sort();
            paths.extend(files.into_iter().map(|p| (Mode::Frontmatter, p)));
        }
    }
    paths
        .into_iter()
        .map(|(mode, path)| Case {
            name: path.strip_prefix(&root).unwrap().display().to_string(),
            mode,
            text: std::fs::read_to_string(&path).unwrap(),
            // Grade and retro files need not carry frontmatter.
            expect: if path.ends_with("SKILL.md") || path.ends_with("openai.yaml") { Accept } else { Either },
        })
        .collect()
}

fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = entries.map(|e| e.unwrap().path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    dirs
}

/// The same canonical form `policy_loader.py` prints.
fn canon(value: &Yaml) -> Value {
    match value {
        Yaml::Null => json!({"null": null}),
        Yaml::Bool(b) => json!({"bool": b}),
        Yaml::Int(n) => json!({"int": n}),
        Yaml::Float(x) if x.is_nan() => json!({"float": "nan"}),
        Yaml::Float(x) => json!({"float": format!("{:016x}", x.to_bits())}),
        Yaml::Str(s) => json!({"str": s}),
        Yaml::Date(d) => json!({"date": d}),
        Yaml::DateTime(dt) => json!({"datetime": dt}),
        Yaml::Binary(b) => json!({"binary": b.iter().map(|x| format!("{x:02x}")).collect::<String>()}),
        Yaml::Seq(items) => json!({"seq": items.iter().map(canon).collect::<Vec<_>>()}),
        Yaml::Pairs(pairs) => {
            json!({"seq": pairs.iter().map(|(k, v)| json!({"tuple": [canon(k), canon(v)]})).collect::<Vec<_>>()})
        }
        Yaml::Set(items) => json!({"set": items.iter().map(canon).collect::<Vec<_>>()}),
        Yaml::Map(pairs) => json!({"map": pairs.iter().map(|(k, v)| json!([canon(k), canon(v)])).collect::<Vec<_>>()}),
    }
}

/// Python iterates a set in hash order; sort set members on both sides.
fn sort_sets(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(sort_sets),
        Value::Object(map) => {
            for (key, inner) in map.iter_mut() {
                sort_sets(inner);
                if key == "set"
                    && let Value::Array(items) = inner
                {
                    items.sort_by_key(|v| v.to_string());
                }
            }
        }
        _ => {}
    }
}

fn skip_allowed() -> bool {
    std::env::var("SKIMA_SKIP_PYDIFF").as_deref() == Ok("1")
}

fn python_available() -> bool {
    Command::new("python3")
        .args(["-c", "import yaml"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn run_python(cases: &[Case]) -> Vec<Value> {
    let input: Vec<Value> = cases
        .iter()
        .map(|c| json!({"mode": if c.mode == Mode::Load { "load" } else { "frontmatter" }, "text": c.text}))
        .collect();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/policy_loader.py");
    let mut child = Command::new("python3")
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn python3");
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(serde_json::to_string(&input).unwrap().as_bytes()).unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "policy_loader.py failed");
    serde_json::from_slice(&output.stdout).expect("policy_loader.py output is JSON")
}

#[test]
fn strict_yaml_matches_pyyaml_policy_loader() {
    if !python_available() {
        if skip_allowed() {
            eprintln!("strict_yaml_diff: SKIPPED (python3 with PyYAML not found, SKIMA_SKIP_PYDIFF=1)");
            return;
        }
        panic!("strict_yaml_diff needs python3 with PyYAML; set SKIMA_SKIP_PYDIFF=1 to skip");
    }
    let mut cases = synthetic();
    let synthetic_count = cases.len();
    cases.extend(real_files());
    let python = run_python(&cases);
    assert_eq!(python.len(), cases.len());

    let mut failures = Vec::new();
    for (case, mut want) in cases.iter().zip(python) {
        let result = match case.mode {
            Mode::Load => strict_yaml::load(&case.text),
            Mode::Frontmatter => strict_yaml::frontmatter(&case.text),
        };
        let mut got = result.as_ref().map_or_else(|_| json!({"error": true}), canon);
        sort_sets(&mut got);
        sort_sets(&mut want);
        let rejected = want == json!({"error": true});
        if case.expect == Stricter {
            if rejected || got != json!({"error": true}) {
                failures.push(format!(
                    "{}: {:?}\n  expected PyYAML to accept and skima to reject\n  pyyaml: {want}\n  skima:  {got}",
                    case.name, case.text
                ));
            }
        } else if got != want {
            failures.push(format!(
                "{}: {:?}\n  pyyaml: {want}\n  skima:  {got} ({:?})",
                case.name,
                case.text,
                result.err()
            ));
        } else if (case.expect == Accept && rejected) || (case.expect == Reject && !rejected) {
            failures.push(format!(
                "{}: {:?}\n  both loaders gave {want}, expected {:?}",
                case.name, case.text, case.expect
            ));
        }
    }
    eprintln!(
        "strict_yaml_diff: {} cases ({} synthetic, {} real files)",
        cases.len(),
        synthetic_count,
        cases.len() - synthetic_count
    );
    assert!(failures.is_empty(), "{} of {} cases differ:\n{}", failures.len(), cases.len(), failures.join("\n"));
}
