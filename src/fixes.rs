//! One-click fixes for common LaTeX errors.
//!
//! `propose` looks at the document and the compile errors and suggests
//! concrete edits, each with a plain-English description. Nothing is applied
//! until the user approves; every edit records the text it expects to
//! replace, so a fix is skipped if that part of the document has changed since.
//! Pure Rust, unit-tested.

use crate::compile::TexError;
use crate::complete;
use crate::hints;

/// Replace `doc[start..end]` (byte offsets) with `text`. `expect` is what was
/// there when the fix was proposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub expect: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fix {
    pub description: String,
    pub edits: Vec<Edit>,
    /// Ticked in the approval list by default (false for anything that deletes content).
    pub default_on: bool,
}

/// Preamble commands: lines starting with these belong before \begin{document}.
const PREAMBLE: &[&str] = &[
    "\\documentclass", "\\usepackage", "\\RequirePackage", "\\title", "\\author", "\\date", "\\newcommand",
    "\\renewcommand", "\\providecommand", "\\def", "\\let", "\\setlength", "\\addtolength", "\\geometry",
    "\\hypersetup", "\\DeclareMathOperator", "\\newtheorem", "\\theoremstyle", "\\bibliographystyle",
    "\\graphicspath", "\\definecolor", "\\pagestyle", "\\makeatletter", "\\makeatother", "\\newenvironment",
    "\\renewenvironment", "\\setcounter", "\\numberwithin", "\\lstset", "\\usetikzlibrary", "\\pgfplotsset",
    "\\sisetup", "\\addbibresource", "\\fancyhf", "\\fancyhead", "\\fancyfoot", "\\linespread", "\\input",
    "\\include", "\\newif", "\\newlength", "\\newcounter", "\\AtBeginDocument", "\\PassOptionsToPackage",
    "\\NewDocumentCommand", "\\ExplSyntaxOn", "\\ExplSyntaxOff", "\\setmainfont", "\\setsansfont",
    "\\setmonofont", "\\newfontfamily", "\\selectlanguage", "\\captionsetup", "\\setlist", "\\crefname",
];

/// Byte offset of the start of 1-based `line`.
fn line_start(doc: &str, line: usize) -> Option<usize> {
    if line == 0 {
        return None;
    }
    if line == 1 {
        return Some(0);
    }
    doc.match_indices('\n').nth(line - 2).map(|(i, _)| i + 1)
}

/// (start, end-without-newline) of 1-based `line`.
fn line_span(doc: &str, line: usize) -> Option<(usize, usize)> {
    let s = line_start(doc, line)?;
    if s > doc.len() {
        return None;
    }
    let e = doc[s..].find('\n').map(|n| s + n).unwrap_or(doc.len());
    Some((s, e))
}

fn code_part(line: &str) -> &str {
    // Text before an unescaped %.
    let b = line.as_bytes();
    for i in 0..b.len() {
        if b[i] == b'%' && (i == 0 || b[i - 1] != b'\\') {
            return &line[..i];
        }
    }
    line
}

fn preview(s: &str) -> String {
    let t = s.trim();
    let short: String = t.chars().take(32).collect();
    if t.chars().count() > 32 { format!("{short}…") } else { short }
}

/// Levenshtein distance (small strings).
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Suggest fixes for `errors` in `doc`.
pub fn propose(doc: &str, errors: &[TexError]) -> Vec<Fix> {
    let mut fixes: Vec<Fix> = Vec::new();
    let has_class = doc.lines().any(|l| code_part(l).contains("\\documentclass"));
    let begin = doc.find("\\begin{document}");
    let end_doc = doc.find("\\end{document}");
    let msgs: Vec<String> = errors.iter().map(|e| e.message.to_ascii_lowercase()).collect();
    let any = |needle: &str| msgs.iter().any(|m| m.contains(needle));

    // ---- Document structure -------------------------------------------------
    if !has_class && begin.is_none() && !doc.trim().is_empty() {
        let mut edits = vec![Edit { start: 0, end: 0, expect: String::new(), text: "\\documentclass{article}\n\\begin{document}\n\n".into() }];
        if end_doc.is_none() {
            let nl = if doc.ends_with('\n') { "" } else { "\n" };
            edits.push(Edit { start: doc.len(), end: doc.len(), expect: String::new(), text: format!("{nl}\n\\end{{document}}\n") });
        }
        fixes.push(Fix { description: "Make this a complete LaTeX document (add \\documentclass and \\begin/\\end{document})".into(), edits, default_on: true });
    } else if any("missing \\begin{document}") || (has_class && begin.is_none()) {
        match begin {
            // Text before \begin{document}: move it into the body.
            Some(b) => {
                let mut moved = String::new();
                let mut edits = Vec::new();
                let mut first = None;
                let mut pos = 0;
                for line in doc[..b].split_inclusive('\n') {
                    let t = code_part(line).trim();
                    let is_preamble = t.is_empty() || t.starts_with('%') || PREAMBLE.iter().any(|p| t.starts_with(p));
                    if !is_preamble {
                        first.get_or_insert_with(|| line.to_string());
                        moved.push_str(line.trim_end_matches('\n'));
                        moved.push('\n');
                        edits.push(Edit { start: pos, end: pos + line.len(), expect: line.to_string(), text: String::new() });
                    }
                    pos += line.len();
                }
                if !edits.is_empty() {
                    // After \begin{document} (and \maketitle right after it, if any).
                    let after_begin = b + "\\begin{document}".len();
                    let mut at = doc[after_begin..].find('\n').map(|n| after_begin + n + 1).unwrap_or(doc.len());
                    if doc[at..].trim_start().starts_with("\\maketitle") {
                        let mt = at + doc[at..].find("\\maketitle").unwrap_or(0);
                        at = doc[mt..].find('\n').map(|n| mt + n + 1).unwrap_or(doc.len());
                    }
                    edits.push(Edit { start: at, end: at, expect: String::new(), text: format!("\n{moved}") });
                    let n = edits.len() - 1;
                    let what = if n == 1 { format!("‘{}’", preview(first.as_deref().unwrap_or(""))) } else { format!("{n} lines starting ‘{}’", preview(first.as_deref().unwrap_or(""))) };
                    fixes.push(Fix { description: format!("Move {what} from the preamble into the document body"), edits, default_on: true });
                }
            }
            // \documentclass but no \begin{document}: start the body at the first non-preamble line.
            None => {
                let mut pos = 0;
                let mut at = doc.len();
                let mut seen_class = false;
                for line in doc.split_inclusive('\n') {
                    let t = code_part(line).trim();
                    if t.starts_with("\\documentclass") {
                        seen_class = true;
                    } else if seen_class && !(t.is_empty() || PREAMBLE.iter().any(|p| t.starts_with(p))) {
                        at = pos;
                        break;
                    }
                    pos += line.len();
                }
                let prefix = if at == doc.len() && !doc.ends_with('\n') { "\n" } else { "" };
                let mut edits = vec![Edit { start: at, end: at, expect: String::new(), text: format!("{prefix}\\begin{{document}}\n") }];
                if end_doc.is_none() {
                    let nl = if doc.ends_with('\n') { "" } else { "\n" };
                    edits.push(Edit { start: doc.len(), end: doc.len(), expect: String::new(), text: format!("{nl}\\end{{document}}\n") });
                }
                fixes.push(Fix { description: "Add \\begin{document} (and \\end{document}) around the text".into(), edits, default_on: true });
            }
        }
    }
    // Missing \end{document} (when there is a \begin{document}).
    if begin.is_some() && end_doc.is_none() {
        // Close any environments still open at the end first.
        let open: Vec<String> = complete::open_environments(doc).into_iter().filter(|e| e != "document").collect();
        let mut text = String::from(if doc.ends_with('\n') { "" } else { "\n" });
        for env in open.iter().rev() {
            text.push_str(&format!("\\end{{{env}}}\n"));
        }
        text.push_str("\\end{document}\n");
        fixes.push(Fix { description: "Add the missing \\end{document}".into(), edits: vec![Edit { start: doc.len(), end: doc.len(), expect: String::new(), text }], default_on: true });
    } else if let Some(e) = end_doc {
        // Environments opened in the body but never closed.
        let open: Vec<String> = complete::open_environments(&doc[..e]).into_iter().filter(|x| x != "document").collect();
        if !open.is_empty() {
            let text: String = open.iter().rev().map(|env| format!("\\end{{{env}}}\n")).collect();
            let names = open.iter().map(|x| format!("{{{x}}}")).collect::<Vec<_>>().join(", ");
            fixes.push(Fix { description: format!("Close {names} before \\end{{document}}"), edits: vec![Edit { start: e, end: e, expect: String::new(), text }], default_on: true });
        }
    }

    // ---- Per-error fixes ------------------------------------------------------
    let found = complete::scan_document(doc, None);
    let mut packages_added: Vec<String> = Vec::new();
    for err in errors {
        let m = err.message.to_ascii_lowercase();
        let line_text = err.line.and_then(|l| line_span(doc, l)).map(|(s, e)| (s, &doc[s..e]));

        // Missing package (command or environment).
        if let Some(h) = hints::explain(&err.message, line_text.map(|(_, t)| t), doc) {
            if let Some(pkg) = h.add_package {
                if !packages_added.contains(&pkg) && !hints::has_package(doc, &pkg) {
                    let (at, text) = hints::package_insertion(doc, &pkg);
                    fixes.push(Fix { description: format!("Add \\usepackage{{{pkg}}} — {}", h.text), edits: vec![Edit { start: at, end: at, expect: String::new(), text }], default_on: true });
                    packages_added.push(pkg);
                    continue;
                }
            }
        }
        let Some((ls, text)) = line_text else {
            // Package file that doesn't exist: offer to remove its \usepackage line.
            if let Some(name) = between(&err.message, "File `", "' not found").or_else(|| between(&err.message, "File '", "' not found")) {
                if let Some(pkg) = name.strip_suffix(".sty") {
                    for (idx, l) in doc.lines().enumerate() {
                        let c = code_part(l);
                        if c.contains("\\usepackage") && c.split(['{', '}', ',']).any(|p| p.trim() == pkg) {
                            if let Some((s, e)) = line_span(doc, idx + 1) {
                                let end = if e < doc.len() { e + 1 } else { e };
                                fixes.push(Fix {
                                    description: format!("Remove \\usepackage{{{pkg}}} (line {}) — that package doesn't exist", idx + 1),
                                    edits: vec![Edit { start: s, end, expect: doc[s..end].to_string(), text: String::new() }],
                                    default_on: false,
                                });
                            }
                            break;
                        }
                    }
                }
            }
            continue;
        };
        let line_no = err.line.unwrap_or(0);

        if m.contains("undefined control sequence") {
            // A typo of a known command?
            for (cs_start, cs) in control_sequences(text) {
                if complete::COMMANDS.iter().any(|(c, _, _)| *c == cs) || found.commands.iter().any(|c| c == &cs && doc.matches(&cs).count() > 1) {
                    continue;
                }
                if cs.len() < 4 {
                    continue;
                }
                let mut best: Vec<(&str, usize)> = complete::COMMANDS
                    .iter()
                    .map(|(c, _, _)| (*c, distance(&cs, c)))
                    .filter(|(c, d)| *d <= 2 && *d > 0 && !c.ends_with('*'))
                    .collect();
                best.sort_by_key(|(_, d)| *d);
                if let Some((sugg, d)) = best.first() {
                    if best.get(1).is_none_or(|(_, d2)| d2 > d) {
                        let start = ls + cs_start;
                        fixes.push(Fix {
                            description: format!("Line {line_no}: change {cs} to {sugg}"),
                            edits: vec![Edit { start, end: start + cs.len(), expect: cs.clone(), text: sugg.to_string() }],
                            default_on: true,
                        });
                        break;
                    }
                }
            }
        } else if m.contains("missing $ inserted") {
            if let Some(edits) = math_outside_math(text, ls) {
                if !edits.is_empty() {
                    fixes.push(Fix { description: format!("Line {line_no}: put math (^, _, \\alpha…) inside $…$ or escape the underscore"), edits, default_on: true });
                }
            }
        } else if m.contains("misplaced alignment tab") {
            let edits = escape_char(text, ls, '&');
            if !edits.is_empty() {
                fixes.push(Fix { description: format!("Line {line_no}: write & as \\& (a plain ampersand)"), edits, default_on: true });
            }
        } else if m.contains("macro parameter character #") || m.contains("parameter character #") {
            let edits = escape_char(text, ls, '#');
            if !edits.is_empty() {
                fixes.push(Fix { description: format!("Line {line_no}: write # as \\#"), edits, default_on: true });
            }
        } else if m.contains("there's no line here to end") {
            let t = text.trim_start();
            if t.starts_with("\\\\") {
                let start = ls + (text.len() - t.len());
                fixes.push(Fix { description: format!("Line {line_no}: remove the \\\\ that has no line to end"), edits: vec![Edit { start, end: start + 2, expect: "\\\\".into(), text: String::new() }], default_on: true });
            }
        }
    }

    // Drop duplicate fixes (same edits), keep order.
    let mut out: Vec<Fix> = Vec::new();
    for f in fixes {
        if !out.iter().any(|o| o.edits == f.edits) {
            out.push(f);
        }
    }
    out
}

fn between(s: &str, a: &str, b: &str) -> Option<String> {
    let i = s.find(a)? + a.len();
    let j = s[i..].find(b)? + i;
    Some(s[i..j].to_string())
}

/// (byte offset in line, "\name") for every control word in the code part.
fn control_sequences(line: &str) -> Vec<(usize, String)> {
    let code = code_part(line);
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            let s = i;
            i += 1;
            while i < b.len() && b[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i > s + 1 {
                out.push((s, code[s..i].to_string()));
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Escape every unescaped `ch` in the code part of a line.
fn escape_char(line: &str, line_start: usize, ch: char) -> Vec<Edit> {
    let code = code_part(line);
    let mut edits = Vec::new();
    let b = code.as_bytes();
    for (i, c) in code.char_indices() {
        if c == ch && (i == 0 || b[i - 1] != b'\\') {
            edits.push(Edit { start: line_start + i, end: line_start + i + 1, expect: ch.to_string(), text: format!("\\{ch}") });
        }
    }
    edits
}

/// Math used outside math mode on one line: wrap each such word in $…$, or
/// escape underscores in plain names (snake_case, file_name).
fn math_outside_math(line: &str, line_start: usize) -> Option<Vec<Edit>> {
    const MATHY: &[&str] = &[
        "\\alpha", "\\beta", "\\gamma", "\\delta", "\\epsilon", "\\theta", "\\lambda", "\\mu", "\\pi", "\\sigma",
        "\\omega", "\\phi", "\\infty", "\\sum", "\\int", "\\frac", "\\sqrt", "\\leq", "\\geq", "\\neq", "\\cdot",
        "\\times", "\\pm", "\\approx", "\\to", "\\in", "\\partial", "\\nabla", "\\Delta", "\\Omega", "\\mathbb",
    ];
    let code = code_part(line);
    let mut edits = Vec::new();
    let mut in_math = false;
    let mut i = 0;
    let bytes = code.as_bytes();
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\\' && i + 1 < bytes.len() && bytes[i + 1] == b'$' {
            i += 2;
            continue;
        }
        if c == b'$' {
            in_math = !in_math;
            i += 1;
            continue;
        }
        if in_math || c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // A "word": up to whitespace or $.
        let s = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'$' {
            i += 1;
        }
        let word = &code[s..i];
        // Leave punctuation at the end outside the math.
        let core = word.trim_end_matches(['.', ',', ';', ':', '!', '?', ')']);
        let has_script = core.contains('^') || core.contains('_');
        let has_cmd = MATHY.iter().any(|m| core.starts_with(m) || core.contains(m));
        if !(has_script || has_cmd) || core.starts_with("\\\\") {
            continue;
        }
        let plain_name = !core.contains('^')
            && !core.contains('\\')
            && core.split('_').all(|p| !p.is_empty() && p.chars().all(|ch| ch.is_alphanumeric() || ch == '-' || ch == '.'))
            && core.split('_').any(|p| p.chars().count() > 1);
        if plain_name {
            for (j, ch) in core.char_indices() {
                if ch == '_' {
                    edits.push(Edit { start: line_start + s + j, end: line_start + s + j + 1, expect: "_".into(), text: "\\_".into() });
                }
            }
        } else {
            edits.push(Edit { start: line_start + s, end: line_start + s + core.len(), expect: core.to_string(), text: format!("${core}$") });
        }
    }
    Some(edits)
}

/// Choose which edits to make. Each fix is all-or-nothing: it's taken only if
/// every one of its edits still matches the document and none overlaps an
/// edit already taken (earlier fixes win; a skipped fix is proposed again,
/// at its new position, after the next compile). Returned last-first, so
/// applying them one by one never shifts an offset still to be used.
pub fn applicable_edits<'a>(doc: &str, fixes: &[&'a Fix]) -> Vec<&'a Edit> {
    let mut taken: Vec<&Edit> = Vec::new();
    let overlaps = |a: &Edit, b: &Edit| {
        // Two insertions at the same point are fine (applied in order); ranges must not intersect.
        if a.start == a.end && b.start == b.end {
            return false;
        }
        a.start < b.end && b.start < a.end || (a.start == a.end && b.start < a.start && a.start < b.end) || (b.start == b.end && a.start < b.start && b.start < a.end)
    };
    for fix in fixes {
        let ok = fix.edits.iter().all(|e| {
            e.end <= doc.len()
                && e.start <= e.end
                && doc.is_char_boundary(e.start)
                && doc.is_char_boundary(e.end)
                && doc[e.start..e.end] == e.expect
                && !taken.iter().any(|t| overlaps(t, e))
        });
        if ok {
            taken.extend(fix.edits.iter());
        }
    }
    // Last first; for equal starts, later-taken insertions go first so the
    // earlier-taken text ends up in front.
    let mut ordered: Vec<(usize, &Edit)> = taken.into_iter().enumerate().collect();
    ordered.sort_by(|(ia, a), (ib, b)| b.start.cmp(&a.start).then(b.end.cmp(&a.end)).then(ib.cmp(ia)));
    ordered.into_iter().map(|(_, e)| e).collect()
}

/// Apply the chosen fixes to `doc`; returns the new text and how many edits were made.
#[cfg_attr(not(test), allow(dead_code))]
pub fn apply(doc: &str, fixes: &[&Fix]) -> (String, usize) {
    let edits = applicable_edits(doc, fixes);
    let mut out = doc.to_string();
    for e in &edits {
        out.replace_range(e.start..e.end, &e.text);
    }
    (out, edits.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(line: Option<usize>, msg: &str) -> TexError {
        TexError { message: msg.into(), line }
    }

    fn fix_all(doc: &str, errors: &[TexError]) -> String {
        let fixes = propose(doc, errors);
        let chosen: Vec<&Fix> = fixes.iter().filter(|f| f.default_on).collect();
        apply(doc, &chosen).0
    }

    #[test]
    fn moves_text_out_of_the_preamble() {
        let doc = "\\documentclass{article}\n\\usepackage{amsmath}\nDear team, here are the notes.\nSecond line.\n\\begin{document}\n\\maketitle\nBody.\n\\end{document}\n";
        let out = fix_all(doc, &[err(Some(3), "LaTeX Error: Missing \\begin{document}.")]);
        assert_eq!(out, "\\documentclass{article}\n\\usepackage{amsmath}\n\\begin{document}\n\\maketitle\n\nDear team, here are the notes.\nSecond line.\nBody.\n\\end{document}\n");
        let fixes = propose(doc, &[err(Some(3), "LaTeX Error: Missing \\begin{document}.")]);
        assert!(fixes[0].description.contains("Dear team"), "{}", fixes[0].description);
    }

    #[test]
    fn wraps_bare_text_into_a_document() {
        let out = fix_all("Just some notes\nwith x_1 values", &[err(Some(1), "LaTeX Error: Missing \\begin{document}.")]);
        assert!(out.starts_with("\\documentclass{article}\n\\begin{document}\n\nJust some notes"));
        assert!(out.trim_end().ends_with("\\end{document}"));
    }

    #[test]
    fn adds_begin_document_when_missing() {
        let doc = "\\documentclass{article}\n\\usepackage{graphicx}\n\nHello world.\n";
        let out = fix_all(doc, &[err(Some(4), "LaTeX Error: Missing \\begin{document}.")]);
        assert_eq!(out, "\\documentclass{article}\n\\usepackage{graphicx}\n\n\\begin{document}\nHello world.\n\\end{document}\n");
    }

    #[test]
    fn closes_environments_and_document() {
        let doc = "\\documentclass{article}\n\\begin{document}\n\\begin{itemize}\n\\item a\n";
        let out = fix_all(doc, &[]);
        assert!(out.ends_with("\\item a\n\\end{itemize}\n\\end{document}\n"), "{out}");
        let doc2 = "\\documentclass{article}\n\\begin{document}\n\\begin{center}\nx\n\\end{document}\n";
        let out2 = fix_all(doc2, &[]);
        assert!(out2.contains("x\n\\end{center}\n\\end{document}"), "{out2}");
    }

    #[test]
    fn fixes_typos_and_missing_packages() {
        let doc = "\\documentclass{article}\n\\begin{document}\n\\secton{Intro}\nSets $\\mathbb{R}$.\n\\end{document}\n";
        let out = fix_all(doc, &[
            err(Some(3), "Undefined control sequence."),
            err(Some(4), "Undefined control sequence."),
        ]);
        assert!(out.contains("\\section{Intro}"), "{out}");
        assert!(out.contains("\\usepackage{amssymb}"), "{out}");
    }

    #[test]
    fn math_outside_math_mode() {
        let doc = "\\documentclass{article}\n\\begin{document}\nThe value x^2 and \\alpha, file my_data_file.csv and $y_1$ ok.\n\\end{document}\n";
        let out = fix_all(doc, &[err(Some(3), "Missing $ inserted.")]);
        assert!(out.contains("The value $x^2$ and $\\alpha$, file my\\_data\\_file.csv and $y_1$ ok."), "{out}");
    }

    #[test]
    fn escapes_ampersand_and_hash() {
        let doc = "\\documentclass{article}\n\\begin{document}\nSales & Marketing #1 \\& done\n\\end{document}\n";
        let out = fix_all(doc, &[err(Some(3), "Misplaced alignment tab character &."), err(Some(3), "You can't use `macro parameter character #' in horizontal mode.")]);
        assert!(out.contains("Sales \\& Marketing \\#1 \\& done"), "{out}");
    }

    #[test]
    fn removes_stray_line_break_and_offers_package_removal() {
        let doc = "\\documentclass{article}\n\\usepackage{nosuchpkg}\n\\begin{document}\n\\\\ Hello\n\\end{document}\n";
        let fixes = propose(doc, &[err(Some(4), "There's no line here to end."), err(None, "LaTeX Error: File `nosuchpkg.sty' not found.")]);
        let removal = fixes.iter().find(|f| f.description.contains("nosuchpkg")).unwrap();
        assert!(!removal.default_on, "deleting needs an explicit tick");
        let all: Vec<&Fix> = fixes.iter().collect();
        let (out, _) = apply(doc, &all);
        assert_eq!(out, "\\documentclass{article}\n\\begin{document}\n Hello\n\\end{document}\n");
    }

    #[test]
    fn skips_edits_when_the_text_changed() {
        let doc = "\\documentclass{article}\n\\begin{document}\n\\secton{A}\n\\end{document}\n";
        let fixes = propose(doc, &[err(Some(3), "Undefined control sequence.")]);
        let changed = doc.replace("\\secton", "\\sectoX");
        let chosen: Vec<&Fix> = fixes.iter().collect();
        let (out, n) = apply(&changed, &chosen);
        assert_eq!(n, 0);
        assert_eq!(out, changed);
    }

    #[test]
    fn overlapping_fixes_are_all_or_nothing() {
        let doc = "\\documentclass{article}\nDear team & co.\n\\begin{document}\nBody.\n\\end{document}\n";
        let errs = [err(Some(2), "LaTeX Error: Missing \\begin{document}."), err(Some(2), "Misplaced alignment tab character &.")];
        let out = fix_all(doc, &errs);
        // The move wins whole; the & fix waits for the next compile.
        assert_eq!(out, "\\documentclass{article}\n\\begin{document}\n\nDear team & co.\nBody.\n\\end{document}\n");
        let next = fix_all(&out, &[err(Some(4), "Misplaced alignment tab character &.")]);
        assert!(next.contains("Dear team \\& co."), "{next}");
    }

    #[test]
    fn nothing_to_fix_in_a_good_document() {
        let doc = "\\documentclass{article}\n\\begin{document}\nFine.\n\\end{document}\n";
        assert!(propose(doc, &[]).is_empty());
    }

    /// TYPESET_DUMP=/dir cargo test fixes_dump -- --ignored  → broken docs, fixed, for a real TeX run.
    #[test]
    #[ignore]
    fn fixes_dump() {
        let Some(dir) = std::env::var_os("TYPESET_DUMP") else { return };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cases: Vec<(&str, &str, Vec<TexError>)> = vec![
            ("preamble_text", "\\documentclass{article}\n\\usepackage{amsmath}\nDear team & co, 50% done.\n\\begin{document}\n\\maketitle\nBody.\n\\end{document}\n", vec![err(Some(3), "LaTeX Error: Missing \\begin{document}."), err(Some(3), "Misplaced alignment tab character &.")]),
            ("bare_notes", "Just some notes\nmore text", vec![err(Some(1), "LaTeX Error: Missing \\begin{document}.")]),
            ("no_begin", "\\documentclass{article}\n\\usepackage{graphicx}\n\nHello world.\n", vec![err(Some(4), "LaTeX Error: Missing \\begin{document}.")]),
            ("unclosed", "\\documentclass{article}\n\\begin{document}\n\\begin{itemize}\n\\item a\n\\begin{enumerate}\n\\item b\n", vec![]),
            ("typo_pkg", "\\documentclass{article}\n\\begin{document}\n\\secton{Intro}\nSets $\\mathbb{R}$.\n\\end{document}\n", vec![err(Some(3), "Undefined control sequence."), err(Some(4), "Undefined control sequence.")]),
            ("math", "\\documentclass{article}\n\\begin{document}\nThe value x^2 and \\alpha, file my_data_file.csv, a_b^c.\n\\end{document}\n", vec![err(Some(3), "Missing $ inserted.")]),
            ("chars", "\\documentclass{article}\n\\begin{document}\nSales & Marketing #1\n\\\\ start\n\\end{document}\n", vec![err(Some(3), "Misplaced alignment tab character &."), err(Some(3), "You can't use `macro parameter character #' in horizontal mode."), err(Some(4), "There's no line here to end.")]),
        ];
        for (name, doc, errors) in cases {
            std::fs::write(dir.join(format!("{name}.tex")), fix_all(doc, &errors)).unwrap();
        }
    }

    /// Full loop with a real TeX engine (XeTeX here; Tectonic is built on it):
    /// compile → parse errors → apply default fixes → repeat, until it compiles.
    #[test]
    #[ignore]
    fn fix_loop_with_real_tex() {
        let Ok(dir) = std::env::var("TYPESET_DUMP") else { return };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cases = [
            ("preamble_text", "\\documentclass{article}\n\\usepackage{amsmath}\n\\title{T}\nDear team & co, sales #1 rose x^2 times.\n\\begin{document}\n\\maketitle\nBody.\n\\end{document}\n"),
            ("bare_notes", "Just some notes\nwith a file_name.txt and 5 & 6\n"),
            ("no_begin", "\\documentclass{article}\n\\usepackage{graphicx}\n\nHello \\alpha world.\n"),
            ("unclosed", "\\documentclass{article}\n\\begin{document}\n\\begin{itemize}\n\\item a\n\\begin{enumerate}\n\\item b\n"),
            ("typos", "\\documentclass{article}\n\\begin{document}\n\\secton{Intro}\n\\textbff{x} Sets $\\mathbb{R}$.\n\\end{document}\n"),
        ];
        for (name, original) in cases {
            let mut doc = original.to_string();
            let mut ok = false;
            for round in 0..5 {
                let tex = dir.join(format!("{name}.tex"));
                std::fs::write(&tex, &doc).unwrap();
                let status = std::process::Command::new("xelatex")
                    .args(["-interaction=nonstopmode", "-file-line-error"])
                    .arg(&tex)
                    .current_dir(&dir)
                    .stdout(std::process::Stdio::null())
                    .status()
                    .expect("xelatex");
                let log = std::fs::read_to_string(dir.join(format!("{name}.log"))).unwrap_or_default();
                // "./name.tex:12: Message" (file-line-error style) → TexError
                let marker = format!("{name}.tex:");
                let errors: Vec<TexError> = log
                    .lines()
                    .filter_map(|l| l.find(&marker).map(|i| &l[i + marker.len()..]))
                    .filter_map(|rest| {
                        let (n, msg) = rest.split_once(": ")?;
                        Some(err(n.parse().ok(), msg))
                    })
                    .collect();
                if status.success() && errors.is_empty() {
                    ok = true;
                    eprintln!("{name}: clean after {round} round(s)");
                    break;
                }
                let before = doc.clone();
                doc = fix_all(&doc, &errors);
                if doc == before {
                    eprintln!("{name}: stuck with {errors:?}");
                    break;
                }
            }
            assert!(ok, "{name} still fails:\n{doc}");
        }
    }
}
