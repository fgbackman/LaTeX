//! Plain-English help for LaTeX errors, plus one-keystroke fixes for the
//! most common one: using a command whose package isn't loaded.

/// Which package provides a command or environment.
const PROVIDERS: &[(&str, &str)] = &[
    ("\\mathbb", "amssymb"),
    ("\\mathfrak", "amssymb"),
    ("\\varnothing", "amssymb"),
    ("\\leqslant", "amssymb"),
    ("\\geqslant", "amssymb"),
    ("\\text", "amsmath"),
    ("\\eqref", "amsmath"),
    ("\\dfrac", "amsmath"),
    ("\\tfrac", "amsmath"),
    ("\\binom", "amsmath"),
    ("\\operatorname", "amsmath"),
    ("\\DeclareMathOperator", "amsmath"),
    ("\\iint", "amsmath"),
    ("\\iiint", "amsmath"),
    ("\\boldsymbol", "amsmath"),
    ("\\includegraphics", "graphicx"),
    ("\\graphicspath", "graphicx"),
    ("\\href", "hyperref"),
    ("\\url", "hyperref"),
    ("\\autoref", "hyperref"),
    ("\\hypersetup", "hyperref"),
    ("\\textcolor", "xcolor"),
    ("\\color", "xcolor"),
    ("\\colorbox", "xcolor"),
    ("\\definecolor", "xcolor"),
    ("\\toprule", "booktabs"),
    ("\\midrule", "booktabs"),
    ("\\bottomrule", "booktabs"),
    ("\\SI", "siunitx"),
    ("\\si", "siunitx"),
    ("\\qty", "siunitx"),
    ("\\unit", "siunitx"),
    ("\\num", "siunitx"),
    ("\\cref", "cleveref"),
    ("\\Cref", "cleveref"),
    ("\\multirow", "multirow"),
    ("\\lstinline", "listings"),
    ("\\lstinputlisting", "listings"),
    ("\\tikz", "tikz"),
    ("\\usetikzlibrary", "tikz"),
    ("\\newtheorem", "amsthm"),
    ("\\theoremstyle", "amsthm"),
    ("\\qedhere", "amsthm"),
    ("\\subcaption", "subcaption"),
    ("\\captionof", "caption"),
    ("\\citep", "natbib"),
    ("\\citet", "natbib"),
    ("\\pagestyle{fancy}", "fancyhdr"),
    ("\\fancyhead", "fancyhdr"),
    ("\\fancyfoot", "fancyhdr"),
    ("\\setmainfont", "fontspec"),
    ("\\enquote", "csquotes"),
    ("\\coloneqq", "mathtools"),
    ("\\mathclap", "mathtools"),
];

const ENV_PROVIDERS: &[(&str, &str)] = &[
    ("align", "amsmath"),
    ("align*", "amsmath"),
    ("equation*", "amsmath"),
    ("gather", "amsmath"),
    ("gather*", "amsmath"),
    ("multline", "amsmath"),
    ("split", "amsmath"),
    ("cases", "amsmath"),
    ("pmatrix", "amsmath"),
    ("bmatrix", "amsmath"),
    ("vmatrix", "amsmath"),
    ("matrix", "amsmath"),
    ("proof", "amsthm"),
    ("tikzpicture", "tikz"),
    ("axis", "pgfplots"),
    ("lstlisting", "listings"),
    ("subfigure", "subcaption"),
    ("tabularx", "tabularx"),
    ("longtable", "longtable"),
    ("algorithm", "algorithm2e"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Help {
    /// Short, plain-English explanation + suggestion.
    pub text: String,
    /// A package that, if added, should fix the error.
    pub add_package: Option<String>,
}

/// Explain a TeX error. `source_line` is the text of the line the error points at.
pub fn explain(message: &str, source_line: Option<&str>, doc: &str) -> Option<Help> {
    let m = message.to_ascii_lowercase();
    let line = source_line.unwrap_or("");

    if m.contains("undefined control sequence") {
        // Which command on that line has a known package that isn't loaded?
        for cmd in control_sequences(line) {
            if let Some((_, pkg)) = PROVIDERS.iter().find(|(c, _)| *c == cmd) {
                if !has_package(doc, pkg) {
                    return Some(Help {
                        text: format!("{cmd} needs the {pkg} package"),
                        add_package: Some(pkg.to_string()),
                    });
                }
            }
        }
        let unknown = control_sequences(line)
            .into_iter()
            .rfind(|c| !crate::complete::COMMANDS.iter().any(|(k, _, _)| k == c));
        return Some(Help {
            text: match unknown {
                Some(c) => format!("LaTeX doesn't know {c} — check the spelling, or a \\usepackage may be missing"),
                None => "A command isn't defined — check the spelling, or a \\usepackage may be missing".into(),
            },
            add_package: None,
        });
    }
    if m.contains("environment") && m.contains("undefined") {
        let env = between(message, "Environment ", " undefined").unwrap_or_default();
        if let Some((_, pkg)) = ENV_PROVIDERS.iter().find(|(e, _)| *e == env) {
            if !has_package(doc, pkg) {
                return Some(Help {
                    text: format!("The {env} environment needs the {pkg} package"),
                    add_package: Some(pkg.to_string()),
                });
            }
        }
        return Some(Help { text: format!("Unknown environment {env} — check the spelling or load its package"), add_package: None });
    }
    let plain = |t: &str| Some(Help { text: t.to_string(), add_package: None });
    if m.contains("missing $ inserted") {
        return plain("Math (like ^, _, or \\alpha) used outside math mode — wrap it in $ … $");
    }
    if m.contains("file") && m.contains("not found") {
        let f = between(message, "File `", "'").unwrap_or_default();
        if f.ends_with(".sty") || f.ends_with(".cls") {
            return plain(&format!("{f} isn't a package Tectonic knows — check the name"));
        }
        return plain(&format!("Can't find {f} — save the document next to it, or check the path"));
    }
    if m.contains("missing } inserted") || m.contains("extra }") || m.contains("runaway argument")
        || m.contains("missing { inserted")
    {
        return plain("Braces don't match — every { needs a }");
    }
    if m.contains("\\begin{") && m.contains("ended by \\end{") {
        return plain("An environment was closed with the wrong \\end — check your \\begin/\\end pairs");
    }
    if m.contains("misplaced alignment tab") {
        return plain("& only works inside tables and align — write \\& for an ampersand");
    }
    if m.contains("there's no line here to end") {
        return plain("\\\\ used where there's no line to break — leave a blank line for a new paragraph");
    }
    if m.contains("missing \\begin{document}") {
        return plain("Text before \\begin{document} — move it into the document body");
    }
    if m.contains("double superscript") || m.contains("double subscript") {
        return plain("Two ^ or _ in a row — group them with braces: x^{ab}");
    }
    if m.contains("missing number") {
        return plain("A length or number is missing — e.g. \\vspace{1em}");
    }
    if m.contains("can be used only in preamble") {
        return plain("This command belongs before \\begin{document}");
    }
    None
}

/// All `\commands` on a line, in order (with `\pagestyle{fancy}` special-cased).
fn control_sequences(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            let s = i;
            i += 1;
            while i < b.len() && b[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i > s + 1 {
                out.push(line[s..i].to_string());
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    if line.contains("\\pagestyle{fancy}") {
        out.push("\\pagestyle{fancy}".into());
    }
    out
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let i = s.find(a)? + a.len();
    let j = s[i..].find(b)? + i;
    Some(&s[i..j])
}

/// Is `pkg` already loaded by a \usepackage line?
pub fn has_package(doc: &str, pkg: &str) -> bool {
    doc.lines().any(|l| {
        let l = l.split('%').next().unwrap_or("");
        (l.contains("\\usepackage") || l.contains("\\RequirePackage"))
            && l.split(['{', '}', ',']).any(|p| p.trim() == pkg)
    })
}

/// Where to insert `\usepackage{pkg}`: (byte offset, text). After the last
/// \usepackage line, or right after \documentclass.
#[cfg_attr(not(test), allow(dead_code))]
pub fn package_insertion(doc: &str, pkg: &str) -> (usize, String) {
    line_insertion(doc, &format!("\\usepackage{{{pkg}}}"))
}

/// Where to insert a preamble line (e.g. `\usepackage[normalem]{ulem}`):
/// after the last \usepackage line, or right after \documentclass.
pub fn line_insertion(doc: &str, preamble_line: &str) -> (usize, String) {
    let mut offset = 0;
    let mut anchor: Option<usize> = None;
    for line in doc.split_inclusive('\n') {
        let code = line.split('%').next().unwrap_or("");
        if code.contains("\\begin{document}") {
            break;
        }
        if code.contains("\\usepackage") || code.contains("\\documentclass") {
            anchor = Some(offset + line.len());
        }
        offset += line.len();
    }
    let at = anchor.unwrap_or(0);
    let needs_newline = at > 0 && !doc[..at].ends_with('\n');
    let text = format!("{}{preamble_line}\n", if needs_newline { "\n" } else { "" });
    (at, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "\\documentclass{article}\n\\usepackage{amsmath}\n\\begin{document}\nSets: $\\mathbb{R}$\n\\end{document}\n";

    #[test]
    fn missing_package_for_command() {
        let h = explain("Undefined control sequence.", Some("Sets: $\\mathbb{R}$"), DOC).unwrap();
        assert_eq!(h.add_package.as_deref(), Some("amssymb"));
        assert!(h.text.contains("\\mathbb"));
    }

    #[test]
    fn typo_command() {
        let h = explain("Undefined control sequence.", Some("\\secton{Intro}"), DOC).unwrap();
        assert!(h.add_package.is_none());
        assert!(h.text.contains("\\secton"));
    }

    #[test]
    fn environment_needs_package() {
        let doc = "\\documentclass{article}\n\\begin{document}\n";
        let h = explain("LaTeX Error: Environment align undefined.", None, doc).unwrap();
        assert_eq!(h.add_package.as_deref(), Some("amsmath"));
        // already loaded → no package fix
        let h = explain("LaTeX Error: Environment align undefined.", None, DOC).unwrap();
        assert!(h.add_package.is_none());
    }

    #[test]
    fn common_messages() {
        assert!(explain("Missing $ inserted.", None, DOC).unwrap().text.contains("$"));
        assert!(explain("Misplaced alignment tab character &.", None, DOC).unwrap().text.contains("\\&"));
        assert!(explain("LaTeX Error: File `foo.sty' not found.", None, DOC).unwrap().text.contains("foo.sty"));
        assert!(explain("Something new", None, DOC).is_none());
    }

    #[test]
    fn inserts_package_in_preamble() {
        let (at, text) = package_insertion(DOC, "amssymb");
        let mut d = DOC.to_string();
        d.insert_str(at, &text);
        assert!(d.starts_with("\\documentclass{article}\n\\usepackage{amsmath}\n\\usepackage{amssymb}\n\\begin{document}"));
        assert!(has_package(&d, "amssymb"));
        assert!(!has_package("% \\usepackage{amssymb}", "amssymb"));
    }
}
