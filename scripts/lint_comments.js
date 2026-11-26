#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { execFileSync } = require("node:child_process");

const BASELINE_NAME = "comment-baseline.json";
const DIRECTIVES = [
    /^# shellcheck (?:disable|enable|source|source-path|shell|external-sources)=\S/,
    /^# (?:noqa|type: ignore|pragma: no cover|fmt: (?:off|on|skip))\b/,
    /^# (?:pylint|ruff|mypy|pyright|isort): ?\S/,
    /^#.*?coding[:=][ \t]*[-\w.]+/,
    /^-- ?luacheck: \S/,
    /^---@\w/,
    /^\/\/go:\w/,
    /^\/\/ ?(?:nolint|NOLINT|NOLINTNEXTLINE)\b/,
    /^\/\/ clang-format (?:off|on)$/,
    /^\/\/ qmllint (?:disable|enable)\b/,
    /^\/\/ x-release-please-(?:version|major|minor|patch|start-\w+|end)$/,
    /^\/\/@ pragma \w/,
    /^\/\/ eslint-(?:disable|enable)(?:-next-line|-line)?(?: |$)/,
    /^\/\* (?:eslint-(?:disable|enable|env)|eslint|globals?) [^]*\*\/$/,
    /^\/\/ @ts-(?:ignore|expect-error|nocheck|check)\b/,
    /^\/\/ prettier-ignore$/,
    /^(?:\/\/|#|--) SPDX-License-Identifier: \S/
];
const REGEX_PRECEDING_WORDS = new Set([
    "return", "typeof", "case", "in", "of", "void", "delete", "throw", "new",
    "else", "do", "instanceof", "yield", "await"
]);
const REGEX_PRECEDING_PUNCTUATION = new Set("(,=:[!&|?{;+-*%<>~^");
const OPERAND = "operand";

function lineEndFrom(source, index) {
    const end = source.indexOf("\n", index);
    return end < 0 ? source.length : end;
}

function scanJavaScript(source) {
    const comments = [];
    const templateDepths = [];
    let braceDepth = 0;
    let previous = "";
    let i = source.startsWith("#!") ? lineEndFrom(source, 0) : 0;

    const skipQuoted = quote => {
        i++;
        while (i < source.length && source[i] !== quote && source[i] !== "\n") i += source[i] === "\\" ? 2 : 1;
        i++;
        return OPERAND;
    };
    const skipTemplateText = () => {
        while (i < source.length) {
            if (source[i] === "\\") i += 2;
            else if (source[i] === "`") { i++; return OPERAND; }
            else if (source[i] === "$" && source[i + 1] === "{") {
                i += 2;
                templateDepths.push(braceDepth++);
                return "(";
            } else i++;
        }
        return OPERAND;
    };
    const skipRegex = () => {
        let inClass = false;
        i++;
        while (i < source.length && source[i] !== "\n") {
            if (source[i] === "\\") i++;
            else if (source[i] === "[") inClass = true;
            else if (source[i] === "]") inClass = false;
            else if (source[i] === "/" && !inClass) break;
            i++;
        }
        i++;
        return OPERAND;
    };
    const skipWord = () => {
        const start = i;
        while (i < source.length && /[\w$]/.test(source[i])) i++;
        return previous === "." ? OPERAND : source.slice(start, i);
    };
    const regexAllowed = () => previous === ""
        || REGEX_PRECEDING_PUNCTUATION.has(previous)
        || REGEX_PRECEDING_WORDS.has(previous);

    while (i < source.length) {
        const ch = source[i];
        const next = source[i + 1];
        if (/\s/.test(ch)) i++;
        else if (ch === "/" && (next === "/" || next === "*")) {
            const start = i;
            if (next === "/") i = lineEndFrom(source, i);
            else {
                const end = source.indexOf("*/", i + 2);
                i = end < 0 ? source.length : end + 2;
            }
            comments.push({ start, end: i });
        } else if (ch === "\"" || ch === "'") previous = skipQuoted(ch);
        else if (ch === "`") { i++; previous = skipTemplateText(); }
        else if (ch === "/" && regexAllowed()) previous = skipRegex();
        else if (/[\w$]/.test(ch)) previous = skipWord();
        else if ((ch === "+" || ch === "-") && next === ch) { i += 2; previous = OPERAND; }
        else if (ch === "}") {
            braceDepth--;
            i++;
            previous = ch;
            if (templateDepths.at(-1) === braceDepth) {
                templateDepths.pop();
                previous = skipTemplateText();
            }
        } else {
            if (ch === "{") braceDepth++;
            i++;
            previous = ch;
        }
    }
    return comments;
}

function scanShell(source) {
    const comments = [];
    const pendingHeredocs = [];
    let i = 0;
    let escaped = -2;

    const skipPast = (terminator, from) => {
        const end = source.indexOf(terminator, from);
        i = end < 0 ? source.length : end + terminator.length;
    };
    const skipEscape = () => {
        if (source[i + 1] !== "\n") escaped = i + 1;
        i += 2;
    };
    const skipHeredocBodies = () => {
        for (const heredoc of pendingHeredocs.splice(0)) {
            while (i < source.length) {
                const stop = lineEndFrom(source, i);
                const line = source.slice(i, stop).replace(/\r$/, "");
                i = Math.min(stop + 1, source.length);
                if ((heredoc.stripTabs ? line.replace(/^\t+/, "") : line) === heredoc.tag) break;
            }
        }
    };
    const arithmetic = from => {
        const stop = Math.min(source.length, from + 4096);
        let depth = 0;
        for (let at = from; at < stop; at++) {
            if (source[at] === "(") depth++;
            else if (source[at] === ")" && depth > 0) depth--;
            else if (source[at] === ")") {
                if (source[at + 1] === ")") i = at + 2;
                return source[at + 1] === ")";
            }
        }
        return false;
    };
    const expansion = quoted => {
        if (source.startsWith("$((", i) && arithmetic(i + 3)) return true;
        if (source.startsWith("$[", i)) skipPast("]", i + 2);
        else if (source.startsWith("$(", i)) { i += 2; command(")"); }
        else if (source.startsWith("${", i)) { i += 2; parameter(quoted); }
        else return false;
        return true;
    };
    const parameter = quoted => {
        while (i < source.length && source[i] !== "}") {
            if (source[i] === "\\") skipEscape();
            else if (source[i] === "\"") { i++; doubleQuoted(); }
            else if (source[i] === "'" && !quoted) skipPast("'", i + 1);
            else if (!expansion(quoted)) i++;
        }
        i++;
    };
    const doubleQuoted = () => {
        while (i < source.length && source[i] !== "\"") {
            if (source[i] === "\\") skipEscape();
            else if (source[i] === "`") { i++; command("`"); }
            else if (!expansion(true)) i++;
        }
        i++;
    };
    const command = closer => {
        const cases = [];
        let depth = 0;
        while (i < source.length) {
            const ch = source[i];
            if (ch === closer && depth === 0 && cases.length === 0) { i++; return; }
            if (ch === "c" && (i === 0 || /[\s;|&(]/.test(source[i - 1])) && /^case[ \t]+\S.*?[ \t]in(?![\w-])/.test(source.slice(i, lineEndFrom(source, i)))) {
                cases.push(0);
                i += 4;
                continue;
            }
            if (ch === "e" && cases.length > 0 && /^esac(?![\w-])/.test(source.slice(i, i + 5)) && /(?:^|[;\n])[ \t]*$/.test(source.slice(Math.max(0, i - 80), i))) {
                cases.pop();
                i += 4;
                continue;
            }
            if (ch === "\\") skipEscape();
            else if (ch === "\"") { i++; doubleQuoted(); }
            else if (ch === "'") skipPast("'", i + 1);
            else if (ch === "`") { i++; command("`"); }
            else if (source.startsWith("$'", i)) {
                i += 2;
                while (i < source.length && source[i] !== "'") i += source[i] === "\\" ? 2 : 1;
                i++;
            } else if (expansion(false)) continue;
            else if (source.startsWith("((", i) && arithmetic(i + 2)) continue;
            else if (source.startsWith("<<<", i)) i += 3;
            else if (source.startsWith("<<", i)) {
                const match = /^<<(-?)[ \t]*(?:\\?([\w.-]+)|'([^'\n]+)'|"([^"\n]+)")/.exec(source.slice(i, i + 200));
                if (match) pendingHeredocs.push({ tag: match[2] ?? match[3] ?? match[4], stripTabs: match[1] === "-" });
                i += match ? match[0].length : 2;
            } else if (ch === "\n" && pendingHeredocs.length > 0) {
                i++;
                skipHeredocBodies();
            } else if (ch === "#" && i - 1 !== escaped && (i === 0 || /[\s;]/.test(source[i - 1])
                || (/[|&(]/.test(source[i - 1]) && /^(?:\s|$)/.test(source.slice(i + 1, i + 2))))) {
                const start = i;
                i = lineEndFrom(source, i);
                if (start > 0 || source[1] !== "!") comments.push({ start, end: i });
            } else {
                if (cases.length > 0) cases[cases.length - 1] = Math.max(0, cases.at(-1) + (ch === "(" ? 1 : ch === ")" ? -1 : 0));
                else if (closer === ")" && ch === "(") depth++;
                else if (ch === ")" && depth > 0) depth--;
                i++;
            }
        }
    };

    command(null);
    return comments;
}

function nested(open, close) {
    return (source, from) => {
        if (!source.startsWith(open, from)) return 0;
        let depth = 0;
        let at = from;
        while (at < source.length) {
            if (source.startsWith(open, at)) { depth++; at += open.length; }
            else if (source.startsWith(close, at)) {
                at += close.length;
                if (--depth === 0) break;
            } else at++;
        }
        return at - from;
    };
}

function scanTokens(comments, literals) {
    const lengthAt = (patterns, source, i) => {
        for (const pattern of patterns) {
            if (typeof pattern === "function") {
                const length = pattern(source, i);
                if (length > 0) return length;
                continue;
            }
            pattern.lastIndex = i;
            const match = pattern.exec(source);
            if (match) return match[0].length;
        }
        return 0;
    };
    return source => {
        const found = [];
        let i = /^#!(?!\[)/.test(source) ? lineEndFrom(source, 0) : 0;
        while (i < source.length) {
            const length = lengthAt(comments, source, i);
            if (length > 0) found.push({ start: i, end: i + length });
            i += length || lengthAt(literals, source, i) || 1;
        }
        return found;
    };
}

const SLASH_LINE = /\/\/.*/y;
const SLASH_LINE_CONTINUED = /\/\/(?:\\[ \t]*\r?\n|[^\n])*/y;
const SLASH_BLOCK = /\/\*[^]*?(?:\*\/|$)/y;
const HASH_LINE = /#.*/y;
const DOUBLE_QUOTED = /"(?:\\[^]|[^"\\\n])*"?/y;
const SINGLE_QUOTED = /'(?:\\[^]|[^'\\\n])*'?/y;
const CHARACTER = /'(?:\\[^\n]+?|[^'\\\n])'/yu;
const TRIPLE_DOUBLE = /"""(?:\\[^]|[^\\])*?(?:"""|$)/y;
const TRIPLE_SINGLE = /'''(?:\\[^]|[^\\])*?(?:'''|$)/y;

const LANGUAGES = [
    {
        extensions: [".js", ".cjs", ".mjs", ".ts", ".cts", ".mts", ".qml"],
        interpreters: ["node", "nodejs"],
        scan: scanJavaScript
    },
    {
        extensions: [".sh", ".bash", ".zsh"],
        files: /^\.(?:(?:bash|zsh|ksh)rc|zshenv|zprofile|zlogin|zlogout|bash_profile|bash_login|bash_logout|bash_aliases|profile|envrc)$/,
        interpreters: ["sh", "bash", "zsh", "dash", "ksh"],
        scan: scanShell
    },
    {
        extensions: [".py", ".pyi"],
        interpreters: ["python", "pypy"],
        scan: scanTokens([HASH_LINE], [TRIPLE_DOUBLE, TRIPLE_SINGLE, DOUBLE_QUOTED, SINGLE_QUOTED])
    },
    {
        extensions: [".lua"],
        interpreters: ["lua", "luajit"],
        scan: scanTokens(
            [/--\[(=*)\[[^]*?(?:\]\1\]|$)/y, /--.*/y],
            [/\[(=*)\[[^]*?(?:\]\1\]|$)/y, DOUBLE_QUOTED, SINGLE_QUOTED])
    },
    {
        extensions: [".go"],
        scan: scanTokens([SLASH_LINE, SLASH_BLOCK], [DOUBLE_QUOTED, /`[^`]*`?/y, CHARACTER])
    },
    {
        extensions: [".rs"],
        scan: scanTokens([SLASH_LINE, nested("/*", "*/")], [/b?r(#*)"[^]*?(?:"\1|$)/y, /"(?:\\[^]|[^"\\])*"?/y, CHARACTER])
    },
    {
        extensions: [".c", ".h", ".cc", ".cpp", ".cxx", ".hh", ".hpp"],
        scan: scanTokens([SLASH_LINE_CONTINUED, SLASH_BLOCK], [/R"([^(\s]*)\([^]*?(?:\)\1"|$)/y, DOUBLE_QUOTED, CHARACTER])
    },
    {
        extensions: [".java"],
        scan: scanTokens([SLASH_LINE, SLASH_BLOCK], [TRIPLE_DOUBLE, DOUBLE_QUOTED, CHARACTER])
    }
];

function scannerFor(file, source) {
    const extension = path.extname(file).toLowerCase();
    const name = path.basename(file);
    const interpreter = /^#!\s*(?:\S*\/env\s+(?:(?:-\S+|\w+=\S*)\s+)*)?(?:\S*\/)?([a-zA-Z_]+)/.exec(source)?.[1];
    const language = LANGUAGES.find(({ extensions, files }) => extensions?.includes(extension) || files?.test(name))
        ?? LANGUAGES.find(({ interpreters }) => interpreters?.includes(interpreter));
    return language?.scan ?? null;
}

function commentsIn(file, source) {
    const scan = scannerFor(file, source);
    if (scan === null) return null;
    const newlinesIn = (from, to) => {
        let count = 0;
        for (let at = source.indexOf("\n", from); at >= 0 && at < to; at = source.indexOf("\n", at + 1)) count++;
        return count;
    };
    let line = 1;
    let counted = 0;
    return scan(source)
        .map(({ start, end: scannedEnd }) => {
            const end = source[scannedEnd - 1] === "\r" ? scannedEnd - 1 : scannedEnd;
            const lineStart = Math.max(source.lastIndexOf("\n", start - 1) + 1, source.startsWith("\uFEFF") ? 1 : 0);
            const lineEnd = lineEndFrom(source, end);
            line += newlinesIn(counted, start);
            counted = start;
            return {
                line,
                lines: newlinesIn(start, end) + 1,
                standalone: source.slice(lineStart, start).trim() === "" && source.slice(end, lineEnd).trim() === "",
                text: source.slice(start, end),
                start,
                end,
                lineStart,
                lineEnd
            };
        })
        .filter(comment => !DIRECTIVES.some(directive => directive.test(comment.text)));
}

function strip(source, comments) {
    const parts = [];
    let tail = "";
    let cursor = 0;
    const append = text => {
        parts.push(text);
        tail = (tail + text).slice(-256);
    };
    const dropLast = count => {
        tail = tail.slice(0, tail.length - count);
        for (let left = count; left > 0;) {
            const part = parts.pop();
            if (part.length > left) parts.push(part.slice(0, part.length - left));
            left -= part.length;
        }
    };
    for (const comment of comments) {
        if (!comment.standalone) {
            const before = source.slice(cursor, comment.start);
            const gap = /^[ \t]*/.exec(source.slice(comment.end, comment.lineEnd))[0].length;
            const leads = source.slice(comment.lineStart, comment.start).trim() === "";
            const spaceBefore = /[ \t]$/.test(before.slice(-1));
            if (source.slice(comment.end, comment.lineEnd).trim() === "") append(before.replace(/[ \t]+$/, ""));
            else append(leads || spaceBefore || gap > 0 ? before : `${before} `);
            cursor = leads || spaceBefore ? comment.end + gap : comment.end;
            continue;
        }
        append(source.slice(cursor, comment.lineStart));
        cursor = Math.min(comment.lineEnd + 1, source.length);
        const opensGap = parts.every(part => part === "") || /(?:\n|[{[(])[ \t]*\n$/.test(tail);
        const rest = source.slice(cursor, lineEndFrom(source, cursor) + 1);
        const blank = /\n[ \t]*\n$/.exec(tail);
        if (opensGap && /^[ \t]*\n/.test(rest)) cursor += rest.length;
        else if (blank && /^[ \t]*(?:[}\])]|$)/.test(rest)) {
            dropLast(blank[0].length);
            append("\n");
        }
    }
    return parts.join("") + source.slice(cursor);
}

function git(root, ...args) {
    return execFileSync("git", args, { cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
}

function locate() {
    const root = git(process.cwd(), "rev-parse", "--show-toplevel").trim();
    const scriptInRepo = !path.relative(root, __dirname).startsWith("..");
    return { root, baselinePath: scriptInRepo ? path.join(__dirname, BASELINE_NAME) : null };
}

function readSource(target) {
    const bytes = fs.readFileSync(target);
    if (bytes.includes(0)) return { source: null };
    const encoding = Buffer.from(bytes.toString("utf8"), "utf8").equals(bytes) ? "utf8" : "latin1";
    return { source: bytes.toString(encoding), encoding };
}

function survey(root) {
    const report = {};
    const unscanned = {};
    for (const file of git(root, "ls-files", "-z").split("\0")) {
        const target = path.join(root, file);
        if (file === "" || !fs.statSync(target, { throwIfNoEntry: false })?.isFile()) continue;
        const { source } = readSource(target);
        const comments = source === null ? null : commentsIn(file, source);
        if (comments !== null) report[file] = comments;
        else {
            const kind = path.extname(file).toLowerCase() || path.basename(file);
            unscanned[kind] = (unscanned[kind] ?? 0) + 1;
        }
    }
    return { report, unscanned };
}

function describeCoverage({ report, unscanned }, rows) {
    const kinds = Object.entries(unscanned).sort(([, a], [, b]) => b - a).map(([kind, count]) => `${kind} ${count}`);
    const lines = rows.reduce((sum, row) => sum + row.lines, 0);
    return `listed ${lines} comment lines; scanned ${Object.keys(report).length} files; no scanner for ${kinds.join(", ") || "none"}`;
}

function countsOf(report) {
    const counts = {};
    for (const [file, comments] of Object.entries(report)) {
        const lines = comments.reduce((sum, comment) => sum + comment.lines, 0);
        if (lines > 0) counts[file] = lines;
    }
    return counts;
}

function check(counts, baseline) {
    const problems = [];
    for (const file of new Set([...Object.keys(counts), ...Object.keys(baseline)])) {
        const count = counts[file] ?? 0;
        const allowed = baseline[file] ?? 0;
        if (count !== allowed) problems.push({ file, count, allowed });
    }
    return problems;
}

function tighten(counts, baseline) {
    const tightened = {};
    for (const file of Object.keys(counts).sort()) {
        const allowed = Math.min(counts[file], baseline[file] ?? 0);
        if (allowed > 0) tightened[file] = allowed;
    }
    return tightened;
}

function readBaseline(baselinePath) {
    return baselinePath !== null && fs.existsSync(baselinePath) ? JSON.parse(fs.readFileSync(baselinePath, "utf8")) : {};
}

function writeBaseline(baselinePath, baseline) {
    if (Object.keys(baseline).length > 0) fs.writeFileSync(baselinePath, `${JSON.stringify(baseline, null, 2)}\n`);
    else fs.rmSync(baselinePath, { force: true });
}

function storedBaselinePath(baselinePath, mode) {
    if (baselinePath === null) throw new Error(`${mode} keeps ${BASELINE_NAME} beside the script, so run a copy stored in the repository`);
    return baselinePath;
}

function describeProblem({ file, count, allowed }, comments) {
    if (count < allowed)
        return [`${file}: ${count} comment lines, baseline still allows ${allowed}; run this script with --tighten and commit the result`];
    return [
        `${file}: ${count} comment lines, baseline allows ${allowed}`,
        ...comments.map(comment => `  ${file}:${comment.line}: ${comment.text.split("\n")[0].slice(0, 100)}`)
    ];
}

function selectFiles(root, report, args) {
    if (args.length === 0) return Object.keys(report);
    const selections = args.map(arg => {
        const wanted = path.relative(root, path.resolve(arg));
        const inside = wanted === "" ? "" : `${wanted}/`;
        return { arg, files: Object.keys(report).filter(file => file === wanted || file.startsWith(inside)) };
    });
    const empty = selections.filter(selection => selection.files.length === 0).map(selection => selection.arg);
    if (empty.length > 0) throw new Error(`no tracked file with a scanner at: ${empty.join(", ")}`);
    return [...new Set(selections.flatMap(selection => selection.files))];
}

const MODES = {
    "--check"({ report, baselinePath }) {
        const problems = check(countsOf(report), readBaseline(baselinePath));
        if (problems.length === 0) return 0;
        const lines = problems.flatMap(problem => describeProblem(problem, report[problem.file] ?? []));
        process.stderr.write(`Comments are banned.\n${lines.join("\n")}\n`);
        return 1;
    },
    "--tighten"(context) {
        const baselinePath = storedBaselinePath(context.baselinePath, "--tighten");
        writeBaseline(baselinePath, tighten(countsOf(context.report), readBaseline(baselinePath)));
        return MODES["--check"](context);
    },
    "--adopt"(context, args) {
        const baselinePath = storedBaselinePath(context.baselinePath, "--adopt");
        if (args.length === 0 && fs.existsSync(baselinePath))
            throw new Error(`${BASELINE_NAME} exists; name the files to add, or lower it with --tighten`);
        const counts = countsOf(context.report);
        const baseline = readBaseline(baselinePath);
        for (const file of selectFiles(context.root, context.report, args)) {
            if (!(file in baseline) && counts[file] > 0) baseline[file] = counts[file];
        }
        writeBaseline(baselinePath, baseline);
        return MODES["--check"](context);
    },
    "--list"(context, args) {
        const { root, report } = context;
        const rows = selectFiles(root, report, args).flatMap(file => report[file].map(
            ({ line, lines, standalone, text }) => ({ file, line, lines, standalone, text })));
        process.stdout.write(`${JSON.stringify(rows, null, 2)}\n`);
        process.stderr.write(`lint_comments: ${describeCoverage(context, rows)}\n`);
        return 0;
    },
    "--strip"({ root, report }, args) {
        if (args.length === 0) throw new Error("--strip needs at least one file");
        for (const file of selectFiles(root, report, args)) {
            const target = path.join(root, file);
            const { source, encoding } = readSource(target);
            fs.writeFileSync(target, strip(source, report[file]), encoding);
        }
        return 0;
    }
};

function main([mode = "--check", ...args]) {
    try {
        if (!(mode in MODES)) throw new Error(`unknown mode ${mode}; expected one of ${Object.keys(MODES).join(", ")}`);
        const location = locate();
        return MODES[mode]({ ...location, ...survey(location.root) }, args);
    } catch (error) {
        process.stderr.write(`lint_comments: ${error.message}\n`);
        return 2;
    }
}

if (require.main === module) process.exitCode = main(process.argv.slice(2));

module.exports = { commentsIn, strip, check, tighten };
