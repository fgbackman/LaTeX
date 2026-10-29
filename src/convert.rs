//! Paste & Format: turn documents and notes into LaTeX.
//!
//! Two inputs:
//!   * rich text — Word, Pages, Google Docs, web pages, RTF/DOCX files. The
//!     app reads these with AppKit and hands us plain paragraphs + styles
//!     ([`RichPara`]); we work out headings, lists, tables and code.
//!   * text — Markdown or an ordinary note, parsed with pulldown-cmark.
//!
//! Everything is escaped so characters like % $ & # _ can't break the document.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use pulldown_cmark::{Alignment, Event, LinkType, Options, Parser, Tag, TagEnd};

/// The result of a conversion.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Converted {
    /// LaTeX for the document body.
    pub body: String,
    /// Packages the body needs (e.g. "hyperref").
    pub packages: BTreeSet<&'static str>,
    /// A title, when the source clearly had one and `as_document` was set.
    pub title: Option<String>,
}

/// The `\usepackage` line for a package.
pub fn package_line(pkg: &str) -> String {
    match pkg {
        "ulem" => "\\usepackage[normalem]{ulem}".into(),
        _ => format!("\\usepackage{{{pkg}}}"),
    }
}

/// A complete, compilable document around a conversion.
pub fn wrap_document(c: &Converted) -> String {
    let mut s = String::from("\\documentclass{article}\n");
    // hyperref should be loaded last.
    let mut pkgs: Vec<&str> = c.packages.iter().copied().filter(|p| *p != "hyperref").collect();
    if c.packages.contains("hyperref") {
        pkgs.push("hyperref");
    }
    for p in pkgs {
        s.push_str(&package_line(p));
        s.push('\n');
    }
    if let Some(t) = &c.title {
        let _ = write!(s, "\n\\title{{{t}}}\n\\date{{}}\n");
    }
    s.push_str("\n\\begin{document}\n");
    if c.title.is_some() {
        s.push_str("\\maketitle\n");
    }
    s.push('\n');
    s.push_str(c.body.trim());
    s.push_str("\n\n\\end{document}\n");
    s
}

// ---------------------------------------------------------------------------
// Escaping
// ---------------------------------------------------------------------------

/// Escape text for LaTeX and convert typographic characters.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\textbackslash{}"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '$' => out.push_str("\\$"),
            '&' => out.push_str("\\&"),
            '%' => out.push_str("\\%"),
            '#' => out.push_str("\\#"),
            '_' => out.push_str("\\_"),
            '^' => out.push_str("\\^{}"),
            '~' => out.push_str("\\textasciitilde{}"),
            '<' => out.push_str("\\textless{}"),
            '>' => out.push_str("\\textgreater{}"),
            '|' => out.push_str("\\textbar{}"),
            '“' => out.push_str("``"),
            '”' => out.push_str("''"),
            '‘' => out.push('`'),
            '’' => out.push('\''),
            '–' => out.push_str("--"),
            '—' => out.push_str("---"),
            '…' => out.push_str("\\ldots{}"),
            '\u{a0}' => out.push('~'),
            '\u{200b}' | '\u{feff}' | '\u{fffc}' => {}
            '\t' => out.push(' '),
            '\n' => out.push('\n'),
            // Control characters and hidden text-direction overrides ("Trojan
            // Source") never belong in a document.
            c if is_hidden_control(c) => {}
            _ => out.push(c),
        }
    }
    out
}

/// Control characters and invisible bidi overrides.
pub fn is_hidden_control(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t')
        || ('\u{202a}'..='\u{202e}').contains(&c)
        || ('\u{2066}'..='\u{2069}').contains(&c)
}

/// True if text contains hidden text-direction overrides (used to disguise code).
pub fn has_bidi_overrides(text: &str) -> bool {
    text.chars().any(|c| ('\u{202a}'..='\u{202e}').contains(&c) || ('\u{2066}'..='\u{2069}').contains(&c))
}

/// Only plain web and mail links become clickable; `javascript:`, `file:`,
/// app-specific schemes and relative links are dropped (text kept).
pub fn safe_url(u: &str) -> Option<String> {
    let u = u.trim();
    if u.chars().any(|c| c.is_control() || c == ' ') {
        return None;
    }
    let lower = u.to_ascii_lowercase();
    if ["http://", "https://", "mailto:"].iter().any(|p| lower.starts_with(p)) && u.len() > 8 {
        Some(u.to_string())
    } else {
        None
    }
}

/// Escape for the first argument of \href / \url (only a few characters matter).
fn escape_url(u: &str) -> String {
    let mut out = String::new();
    for c in u.chars() {
        match c {
            '%' | '#' => {
                out.push('\\');
                out.push(c);
            }
            '\\' | '{' | '}' => {}
            _ => out.push(c),
        }
    }
    out
}

/// Pasted text that already is LaTeX should go in untouched.
pub fn looks_like_latex(text: &str) -> bool {
    if text.contains("\\begin{") || text.contains("\\section") || text.contains("\\documentclass") {
        return true;
    }
    let mut cmds = 0;
    let b = text.as_bytes();
    for i in 0..b.len().saturating_sub(1) {
        if b[i] == b'\\' && b[i + 1].is_ascii_alphabetic() {
            cmds += 1;
        }
    }
    cmds >= 2
}

// ---------------------------------------------------------------------------
// Rich text (paragraphs extracted from an NSAttributedString by the app)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub mono: bool,
    /// >0 superscript, <0 subscript
    pub script: i8,
    pub link: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct RichRun {
    pub text: String,
    pub style: Style,
    pub size: f64,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct TableCell {
    /// Identifies the table (cells with the same id belong together).
    pub table: usize,
    pub row: usize,
    pub col: usize,
    pub columns: usize,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct RichPara {
    pub runs: Vec<RichRun>,
    /// Some((ordered, nesting level starting at 1)) for list items.
    pub list: Option<(bool, usize)>,
    pub cell: Option<TableCell>,
}

impl RichPara {
    fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
    fn is_blank(&self) -> bool {
        self.text().trim().is_empty()
    }
    /// Largest font size of the visible text.
    fn size(&self) -> f64 {
        self.runs.iter().filter(|r| !r.text.trim().is_empty()).map(|r| r.size).fold(0.0, f64::max)
    }
    fn all(&self, f: impl Fn(&Style) -> bool) -> bool {
        let mut any = false;
        for r in self.runs.iter().filter(|r| !r.text.trim().is_empty()) {
            if !f(&r.style) {
                return false;
            }
            any = true;
        }
        any
    }
    /// True if the text has no visible styling at all (so it may be Markdown/plain).
    pub fn is_unstyled(&self) -> bool {
        self.list.is_none()
            && self.cell.is_none()
            && self.runs.iter().all(|r| r.style == Style::default())
    }
}

/// True when rich text carries no formatting worth keeping — then the plain
/// text (which may be Markdown) is the better thing to convert.
pub fn rich_is_plain(paras: &[RichPara]) -> bool {
    let sizes: BTreeSet<i64> = paras
        .iter()
        .flat_map(|p| p.runs.iter())
        .filter(|r| !r.text.trim().is_empty())
        .map(|r| (r.size * 2.0).round() as i64)
        .collect();
    sizes.len() <= 1 && paras.iter().all(RichPara::is_unstyled)
}

/// Convert rich paragraphs. `as_document`: the result will become a whole new
/// document, so a single leading title can become `\title{}`.
pub fn from_rich(paras: &[RichPara], as_document: bool) -> Converted {
    let mut out = Converted::default();
    let paras: Vec<&RichPara> = paras.iter().collect();

    // Body size = the font size carrying the most text.
    let mut weight: Vec<(i64, usize)> = Vec::new();
    for p in &paras {
        for r in &p.runs {
            let n = r.text.chars().filter(|c| !c.is_whitespace()).count();
            let k = (r.size * 2.0).round() as i64;
            match weight.iter_mut().find(|(s, _)| *s == k) {
                Some(e) => e.1 += n,
                None => weight.push((k, n)),
            }
        }
    }
    let body = weight.iter().max_by_key(|(_, n)| *n).map(|(s, _)| *s as f64 / 2.0).unwrap_or(12.0);

    // Heading candidates.
    let is_heading = |p: &RichPara| -> Option<bool> {
        // Some(true) = by size, Some(false) = bold-only
        if p.list.is_some() || p.cell.is_some() || p.is_blank() || p.all(|s| s.mono) {
            return None;
        }
        let t = p.text();
        let t = t.trim();
        if t.chars().count() > 120 || t.ends_with('.') || t.ends_with(',') || t.ends_with(';') {
            return None;
        }
        if p.size() >= body * 1.15 && p.size() > 0.0 {
            return Some(true);
        }
        if p.all(|s| s.bold) && t.chars().count() <= 80 && !body.is_nan() {
            return Some(false);
        }
        None
    };
    let mut size_levels: Vec<i64> = paras
        .iter()
        .filter(|p| is_heading(p) == Some(true))
        .map(|p| (p.size() * 2.0).round() as i64)
        .collect();
    size_levels.sort_unstable_by(|a, b| b.cmp(a));
    size_levels.dedup();

    // A title: the first visible paragraph, alone at the biggest size.
    let mut skip_first = false;
    if as_document {
        if let Some(first) = paras.iter().find(|p| !p.is_blank()) {
            if is_heading(first) == Some(true) {
                let k = (first.size() * 2.0).round() as i64;
                let count = paras.iter().filter(|p| is_heading(p) == Some(true) && (p.size() * 2.0).round() as i64 == k).count();
                if count == 1 && size_levels.first() == Some(&k) {
                    out.title = Some(render_runs(&first.runs, &mut out.packages, true));
                    skip_first = true;
                    size_levels.remove(0);
                }
            }
        }
    }
    let bold_level = (size_levels.len() + 1).min(4) as u8;

    let mut lists: Vec<bool> = Vec::new(); // open list environments (ordered?)
    let mut i = 0;
    let mut first_seen = false;
    while i < paras.len() {
        let p = paras[i];
        if !first_seen && !p.is_blank() {
            first_seen = true;
            if skip_first {
                i += 1;
                continue;
            }
        }

        // Close lists when leaving them.
        let level = p.list.map(|(_, l)| l.max(1)).unwrap_or(0);
        if p.list.is_none() && !p.is_blank() || p.list.is_some() && lists.len() > level {
            while lists.len() > level {
                let ordered = lists.pop().unwrap_or(false);
                let _ = writeln!(out.body, "{}\\end{{{}}}", "  ".repeat(lists.len()), env(ordered));
            }
            if lists.is_empty() && level == 0 {
                out.body.push('\n');
            }
        }

        if let Some((ordered, _)) = p.list {
            while lists.len() < level {
                let _ = writeln!(out.body, "{}\\begin{{{}}}", "  ".repeat(lists.len()), env(ordered));
                lists.push(ordered);
            }
            let _ = writeln!(out.body, "{}\\item {}", "  ".repeat(level), render_runs(&p.runs, &mut out.packages, false).trim());
            i += 1;
            continue;
        }
        if p.is_blank() {
            i += 1;
            continue;
        }

        // Tables: gather all consecutive cells of the same table.
        if let Some(cell) = &p.cell {
            let id = cell.table;
            let columns = cell.columns.max(1);
            let mut grid: Vec<Vec<String>> = Vec::new();
            while i < paras.len() {
                let Some(c) = &paras[i].cell else { break };
                if c.table != id {
                    break;
                }
                while grid.len() <= c.row {
                    grid.push(vec![String::new(); columns]);
                }
                let text = render_runs(&paras[i].runs, &mut out.packages, false);
                let slot = &mut grid[c.row][c.col.min(columns - 1)];
                if !slot.is_empty() && !text.trim().is_empty() {
                    slot.push(' ');
                }
                slot.push_str(text.trim());
                i += 1;
            }
            write_table(&mut out.body, &grid, &vec!['l'; columns]);
            continue;
        }

        // Code: consecutive all-monospace paragraphs.
        if p.all(|s| s.mono) {
            out.body.push_str("\\begin{verbatim}\n");
            while i < paras.len() && paras[i].list.is_none() && paras[i].cell.is_none() && (paras[i].all(|s| s.mono) || paras[i].is_blank()) {
                out.body.push_str(paras[i].text().trim_end_matches(['\n', '\u{2029}']));
                out.body.push('\n');
                i += 1;
            }
            out.body.push_str("\\end{verbatim}\n\n");
            continue;
        }

        match is_heading(p) {
            Some(by_size) => {
                let level = if by_size {
                    let k = (p.size() * 2.0).round() as i64;
                    size_levels.iter().position(|s| *s == k).map(|x| x as u8 + 1).unwrap_or(1)
                } else {
                    bold_level
                };
                let text = strip_numbering(&render_runs(&p.runs, &mut out.packages, true));
                let _ = write!(out.body, "\\{}{{{}}}\n\n", heading_cmd(level), text.trim());
            }
            None => {
                out.body.push_str(render_runs(&p.runs, &mut out.packages, false).trim());
                out.body.push_str("\n\n");
            }
        }
        i += 1;
    }
    while let Some(ordered) = lists.pop() {
        let _ = writeln!(out.body, "{}\\end{{{}}}", "  ".repeat(lists.len()), env(ordered));
    }
    out.body = tidy(&out.body);
    out
}

fn env(ordered: bool) -> &'static str {
    if ordered { "enumerate" } else { "itemize" }
}

fn heading_cmd(level: u8) -> &'static str {
    match level {
        0 | 1 => "section",
        2 => "subsection",
        3 => "subsubsection",
        _ => "paragraph",
    }
}

/// "1.2 Methods" → "Methods" (LaTeX numbers sections itself).
fn strip_numbering(s: &str) -> String {
    let t = s.trim_start();
    let num_len = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.').len();
    if num_len > 0 && t[..num_len].chars().next().is_some_and(|c| c.is_ascii_digit()) {
        let rest = &t[num_len..];
        if rest.starts_with(' ') || rest.starts_with('\t') {
            return rest.trim_start().to_string();
        }
    }
    s.to_string()
}

/// Render styled runs as inline LaTeX, merging neighbours with the same style.
fn render_runs(runs: &[RichRun], pkgs: &mut BTreeSet<&'static str>, in_heading: bool) -> String {
    let mut merged: Vec<(String, Style)> = Vec::new();
    for r in runs {
        let text = r.text.replace(['\u{2029}', '\n'], " ").replace('\u{2028}', "\n");
        let mut st = r.style.clone();
        if in_heading {
            st.bold = false; // headings are bold already
        }
        // Whitespace-only runs don't need styling.
        if text.trim().is_empty() {
            st = Style { link: st.link.clone(), ..Style::default() };
        }
        match merged.last_mut() {
            Some((t, s)) if *s == st => t.push_str(&text),
            _ => merged.push((text, st)),
        }
    }
    let mut out = String::new();
    for (text, st) in merged {
        // Keep leading/trailing spaces outside the commands: \textbf{word} not \textbf{word }.
        let lead = &text[..text.len() - text.trim_start().len()];
        let trail = &text[text.trim_end().len()..];
        let core = text.trim();
        if core.is_empty() {
            out.push_str(&escape(&text).replace('\n', " "));
            continue;
        }
        let mut s = if st.mono { format!("\\texttt{{{}}}", escape(core)) } else { escape(core) };
        s = s.replace('\n', "\\\\\n");
        if st.script > 0 {
            s = format!("\\textsuperscript{{{s}}}");
        } else if st.script < 0 {
            s = format!("\\textsubscript{{{s}}}");
        }
        if st.strike {
            pkgs.insert("ulem");
            s = format!("\\sout{{{s}}}");
        }
        if st.underline && st.link.is_none() {
            s = format!("\\underline{{{s}}}");
        }
        if st.italic {
            s = format!("\\emph{{{s}}}");
        }
        if st.bold {
            s = format!("\\textbf{{{s}}}");
        }
        if let Some(url) = st.link.as_deref().and_then(safe_url) {
            let url = &url;
            pkgs.insert("hyperref");
            s = if core == url || core == url.trim_start_matches("mailto:") || url.trim_end_matches('/') == core {
                format!("\\url{{{}}}", escape_url(url))
            } else {
                format!("\\href{{{}}}{{{s}}}", escape_url(url))
            };
        }
        out.push_str(&escape(lead).replace('\n', " "));
        out.push_str(&s);
        out.push_str(&escape(trail).replace('\n', " "));
    }
    out
}

fn write_table(out: &mut String, grid: &[Vec<String>], align: &[char]) {
    if grid.is_empty() {
        return;
    }
    let spec: String = align.iter().collect();
    let _ = writeln!(out, "\\begin{{center}}\n\\begin{{tabular}}{{{spec}}}\n\\hline");
    for (r, row) in grid.iter().enumerate() {
        let cells: Vec<&str> = row.iter().map(|c| c.trim()).collect();
        let _ = writeln!(out, "{} \\\\", cells.join(" & "));
        if r == 0 && grid.len() > 1 {
            out.push_str("\\hline\n");
        }
    }
    out.push_str("\\hline\n\\end{tabular}\n\\end{center}\n\n");
}

/// Collapse runs of blank lines.
fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank = 0;
    for line in s.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.trim().to_string() + "\n"
}

// ---------------------------------------------------------------------------
// Safety checks for TeX that comes from pasted text
// ---------------------------------------------------------------------------

/// Math commands allowed through from pasted Markdown. Anything else (e.g.
/// `\input`, `\write`, `\def`, `\catcode`) means the "math" is shown as text.
const MATH_ALLOWED: &[&str] = &[
    "frac", "dfrac", "tfrac", "sqrt", "sum", "prod", "coprod", "int", "iint", "iiint", "oint", "lim",
    "limsup", "liminf", "sup", "inf", "max", "min", "arg", "det", "exp", "log", "ln", "lg", "sin", "cos",
    "tan", "cot", "sec", "csc", "arcsin", "arccos", "arctan", "sinh", "cosh", "tanh", "gcd", "deg", "dim",
    "ker", "hom", "Pr", "mod", "bmod", "pmod", "infty", "partial", "nabla", "cdot", "cdots", "ldots",
    "dots", "vdots", "ddots", "times", "div", "pm", "mp", "leq", "le", "geq", "ge", "neq", "ne", "ll", "gg",
    "approx", "equiv", "sim", "simeq", "cong", "propto", "in", "notin", "ni", "subset", "subseteq",
    "supset", "supseteq", "cup", "cap", "bigcup", "bigcap", "setminus", "emptyset", "varnothing",
    "forall", "exists", "nexists", "neg", "lnot", "land", "lor", "wedge", "vee", "oplus", "otimes", "to",
    "gets", "rightarrow", "leftarrow", "Rightarrow", "Leftarrow", "leftrightarrow", "Leftrightarrow",
    "longrightarrow", "Longrightarrow", "mapsto", "implies", "iff", "uparrow", "downarrow", "left",
    "right", "middle", "big", "Big", "bigg", "Bigg", "bigl", "bigr", "Bigl", "Bigr", "langle", "rangle",
    "lfloor", "rfloor", "lceil", "rceil", "vert", "Vert", "mid", "parallel", "perp", "angle", "circ",
    "bullet", "star", "ast", "dagger", "ell", "hbar", "Re", "Im", "aleph", "mathbb", "mathcal", "mathbf",
    "mathrm", "mathit", "mathsf", "mathtt", "mathfrak", "boldsymbol", "text", "textrm", "textbf",
    "textit", "operatorname", "hat", "bar", "vec", "dot", "ddot", "tilde", "widehat", "widetilde",
    "overline", "underline", "overbrace", "underbrace", "overrightarrow", "binom", "choose", "over",
    "quad", "qquad", "displaystyle", "textstyle", "prime", "colon", "begin", "end", "cdotp", "nonumber",
    "alpha", "beta", "gamma", "delta", "epsilon", "varepsilon", "zeta", "eta", "theta", "vartheta",
    "iota", "kappa", "lambda", "mu", "nu", "xi", "pi", "varpi", "rho", "varrho", "sigma", "varsigma",
    "tau", "upsilon", "phi", "varphi", "chi", "psi", "omega", "Gamma", "Delta", "Theta", "Lambda", "Xi",
    "Pi", "Sigma", "Upsilon", "Phi", "Psi", "Omega",
];

const MATH_ENVS: &[&str] = &[
    "matrix", "pmatrix", "bmatrix", "Bmatrix", "vmatrix", "Vmatrix", "smallmatrix", "cases", "aligned",
    "gathered", "split", "array",
];

/// Is this pasted math plain math (and nothing that reads/writes files or
/// redefines TeX)? Also requires balanced braces so it can't close outer groups.
pub fn math_is_safe(m: &str) -> bool {
    if m.contains("^^") || m.contains('%') || m.contains('#') || m.chars().any(is_hidden_control) {
        return false;
    }
    let b = m.as_bytes();
    let mut depth: i32 = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                let s = i + 1;
                let mut j = s;
                while j < b.len() && b[j].is_ascii_alphabetic() {
                    j += 1;
                }
                if j == s {
                    // control symbol: \, \; \! \{ \} \| \\ etc.
                    if j < b.len() && !b",;!: {}|\\_&$".contains(&b[j]) {
                        return false;
                    }
                    i = j + 1;
                    continue;
                }
                let name = &m[s..j];
                if !MATH_ALLOWED.contains(&name) {
                    return false;
                }
                if name == "begin" || name == "end" {
                    let rest = &m[j..];
                    let Some(env) = rest.strip_prefix('{').and_then(|r| r.split('}').next()) else { return false };
                    if !MATH_ENVS.contains(&env) {
                        return false;
                    }
                }
                i = j;
                continue;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
        i += 1;
    }
    depth == 0
}

/// TeX commands in pasted LaTeX that touch files or change how TeX reads the
/// document — worth a warning so the user looks before compiling.
pub fn risky_latex(text: &str) -> Vec<&'static str> {
    const RISKY: &[(&str, &str)] = &[
        ("\\input", "\\input"), ("\\include", "\\include"), ("\\openin", "\\openin"),
        ("\\openout", "\\openout"), ("\\write", "\\write"), ("\\immediate", "\\immediate"),
        ("\\read", "\\read"), ("\\catcode", "\\catcode"), ("\\csname", "\\csname"),
        ("\\directlua", "\\directlua"), ("\\special", "\\special"), ("\\usepackage", "\\usepackage"),
        ("^^", "^^ escapes"), ("\\lstinputlisting", "\\lstinputlisting"), ("\\verbatiminput", "\\verbatiminput"),
    ];
    let mut found = Vec::new();
    for (needle, label) in RISKY {
        if text.contains(needle) && !found.contains(label) {
            found.push(*label);
        }
    }
    found
}

// ---------------------------------------------------------------------------
// HTML sanitising (before AppKit/WebKit sees pasted or imported HTML)
// ---------------------------------------------------------------------------

/// Formatting tags we keep. Everything else is dropped (its text is kept).
const HTML_KEEP: &[&str] = &[
    "html", "body", "div", "span", "p", "br", "hr", "b", "strong", "i", "em", "u", "s", "strike", "del",
    "ins", "sup", "sub", "small", "big", "code", "pre", "tt", "kbd", "samp", "blockquote", "q", "cite",
    "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "dl", "dt", "dd", "table", "thead", "tbody",
    "tfoot", "tr", "td", "th", "caption", "col", "colgroup", "a", "font", "center", "section", "article",
    "header", "footer", "main", "aside", "nav", "figure", "figcaption", "mark", "abbr", "address",
];
/// Tags dropped together with everything inside them.
const HTML_DROP_ALL: &[&str] = &[
    "script", "style", "iframe", "object", "embed", "noscript", "template", "svg", "math", "video",
    "audio", "canvas", "frame", "frameset", "applet", "head", "title", "select", "textarea", "noembed",
    "noframes", "xmp", "plaintext",
];

/// Make HTML safe to hand to the system's HTML reader: no scripts, frames,
/// images or any other way to load remote content; only formatting tags and
/// a few harmless attributes survive. Text and entities are untouched.
pub fn sanitize_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let b = html.as_bytes();
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            // Copy text up to the next tag.
            let next = html[i..].find('<').map(|n| i + n).unwrap_or(b.len());
            out.push_str(&html[i..next]);
            i = next;
            continue;
        }
        // Comments, doctype, processing instructions, CDATA: drop.
        if lower[i..].starts_with("<!--") {
            i = lower[i + 4..].find("-->").map(|n| i + 4 + n + 3).unwrap_or(b.len());
            continue;
        }
        if lower[i..].starts_with("<!") || lower[i..].starts_with("<?") {
            i = html[i..].find('>').map(|n| i + n + 1).unwrap_or(b.len());
            continue;
        }
        // A '<' not followed by a tag name is just text ("3 < 5").
        let starts_tag = match b.get(i + 1) {
            Some(c) if c.is_ascii_alphabetic() => true,
            Some(b'/') => b.get(i + 2).is_some_and(|c| c.is_ascii_alphabetic()),
            _ => false,
        };
        if !starts_tag {
            out.push_str("&lt;");
            i += 1;
            continue;
        }
        // Find the end of the tag, respecting quoted attribute values.
        let mut j = i + 1;
        let mut quote: Option<u8> = None;
        while j < b.len() {
            match (quote, b[j]) {
                (None, b'"') | (None, b'\'') => quote = Some(b[j]),
                (Some(q), c) if c == q => quote = None,
                (None, b'>') => break,
                _ => {}
            }
            j += 1;
        }
        if j >= b.len() {
            break; // a tag that never closes: drop it and everything after
        }
        let end = j + 1;
        let tag = &html[i + 1..j];
        i = end;

        let closing = tag.starts_with('/');
        let body = tag.trim_start_matches('/');
        let name_len = body.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(body.len());
        let name = body[..name_len].to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        if !closing && HTML_DROP_ALL.contains(&name.as_str()) {
            // Skip to the matching close tag (or the end).
            let close = format!("</{name}");
            i = lower[i..].find(&close).map(|n| {
                let k = i + n;
                lower[k..].find('>').map(|m| k + m + 1).unwrap_or(b.len())
            }).unwrap_or(b.len());
            continue;
        }
        if !HTML_KEEP.contains(&name.as_str()) {
            continue; // drop the tag, keep its text
        }
        if closing {
            out.push_str(&format!("</{name}>"));
            continue;
        }
        out.push('<');
        out.push_str(&name);
        for (k, v) in parse_attrs(&body[name_len..]) {
            let keep = match k.as_str() {
                "href" if name == "a" => safe_url(&decode_entities(&v)).is_some(),
                "style" => safe_css(&v),
                "colspan" | "rowspan" | "align" | "start" => v.chars().all(|c| c.is_ascii_alphanumeric()),
                _ => false,
            };
            if keep {
                let v = v.replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;");
                out.push_str(&format!(" {k}=\"{v}\""));
            }
        }
        out.push('>');
    }
    out
}

/// Inline CSS without any way to load or run anything.
fn safe_css(v: &str) -> bool {
    let l = decode_entities(v).to_ascii_lowercase();
    !(l.contains("url(") || l.contains("image") || l.contains("expression") || l.contains("@import")
        || l.contains("behavior") || l.contains("binding") || l.contains("javascript") || l.contains('\\')
        || l.contains("src"))
}

/// Minimal entity decoding for attribute checks (so `&#106;avascript:` can't hide).
fn decode_entities(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        let after = &rest[p + 1..];
        let semi = after.find(';').filter(|n| *n <= 10);
        let decoded = semi.and_then(|n| {
            let ent = &after[..n];
            let code = if let Some(h) = ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")) {
                u32::from_str_radix(h, 16).ok()
            } else if let Some(d) = ent.strip_prefix('#') {
                d.parse().ok()
            } else {
                match ent {
                    "amp" => Some('&' as u32), "lt" => Some('<' as u32), "gt" => Some('>' as u32),
                    "quot" => Some('"' as u32), "apos" => Some('\'' as u32), "colon" => Some(':' as u32),
                    "Tab" => Some(9), "NewLine" => Some(10),
                    _ => None,
                }
            };
            code.and_then(char::from_u32).map(|c| (c, n))
        });
        match decoded {
            Some((c, n)) => {
                out.push(c);
                rest = &after[n + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    // Browsers ignore tabs/newlines inside URLs ("java\tscript:").
    out.chars().filter(|c| !c.is_whitespace() || *c == ' ').collect()
}

/// `a="1" b='2' c=3 d` → [(a,1), (b,2), (c,3), (d,"")], names lowercased.
fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let ns = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' {
            i += 1;
        }
        if ns == i {
            i += 1;
            continue;
        }
        let name = s[ns..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                let vs = i + 1;
                i = vs;
                while i < b.len() && b[i] != q {
                    i += 1;
                }
                value = s[vs..i.min(b.len())].to_string();
                i += 1;
            } else {
                let vs = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = s[vs..i].to_string();
            }
        }
        out.push((name, value));
    }
    out
}

// ---------------------------------------------------------------------------
// Markdown and plain notes
// ---------------------------------------------------------------------------

/// Plain notes → Markdown-ish text: bullets become list items, a short first
/// line becomes a heading, and single line breaks are kept.
fn normalise_plain(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let has_md_heading = text.lines().any(|l| l.trim_start().starts_with('#'));
    let has_blank_lines = text.contains("\n\n");
    let is_item = |l: &str| {
        let t = l.trim_start();
        t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ")
            || t.split_once(". ").is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    };

    // Pass 1: bullet characters from Notes, Word, Docs… become Markdown items.
    let mut lines: Vec<String> = text
        .lines()
        .map(|raw| {
            let indent = &raw[..raw.len() - raw.trim_start().len()];
            let t = raw.trim_start();
            match t.chars().next() {
                Some('•' | '◦' | '▪' | '‣' | '·' | '–' | '○' | '■') if t.chars().nth(1).is_some_and(char::is_whitespace) => {
                    format!("{indent}- {}", t.chars().skip(1).collect::<String>().trim_start())
                }
                _ => {
                    // "1) item" → "1. item"
                    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
                    if digits > 0 && t[digits..].starts_with(") ") {
                        format!("{indent}{}. {}", &t[..digits], &t[digits + 2..])
                    } else {
                        raw.to_string()
                    }
                }
            }
        })
        .collect();

    // Apple Notes style: a short first line is the title.
    if !has_md_heading && lines.len() > 2 && !is_item(&lines[0]) {
        let s = lines[0].trim().to_string();
        if !s.is_empty() && s.chars().count() <= 60 && !s.ends_with(['.', ',', ';', ':']) {
            lines[0] = format!("# {s}");
        }
    }

    // Pass 2: without blank lines, every line is its own paragraph (lists stay together).
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        out.push_str(line);
        out.push('\n');
        if !has_blank_lines {
            let next = lines.get(i + 1).map(String::as_str).unwrap_or("");
            if !(is_item(line) && is_item(next)) && !next.trim().is_empty() {
                out.push('\n');
            }
        }
    }
    out
}

/// Convert a plain note (lightly Markdown-aware).
pub fn from_plain(text: &str, as_document: bool) -> Converted {
    from_markdown(&normalise_plain(text), as_document)
}

/// Convert Markdown (CommonMark + tables, strikethrough, task lists, $math$).
pub fn from_markdown(md: &str, as_document: bool) -> Converted {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH;
    let events: Vec<Event> = Parser::new_ext(md, opts).collect();

    let mut out = Converted::default();
    // Shift heading levels so the smallest one used becomes \section.
    let levels: Vec<u8> = events
        .iter()
        .filter_map(|e| match e {
            Event::Start(Tag::Heading { level, .. }) => Some(*level as u8),
            _ => None,
        })
        .collect();
    let mut top = levels.iter().copied().min().unwrap_or(1);
    // A lone top-level heading at the very start is the title.
    let mut title_pending = false;
    if as_document && levels.iter().filter(|l| **l == top).count() == 1 {
        if let Some(Event::Start(Tag::Heading { level, .. })) = events.first() {
            if *level as u8 == top {
                title_pending = true;
                top = levels.iter().copied().filter(|l| *l > top).min().unwrap_or(top + 1);
            }
        }
    }

    let mut s = String::new();
    let mut in_code = false;
    let mut in_title = false;
    let mut title = String::new();
    let mut lists: Vec<bool> = Vec::new();
    let mut table_align: Vec<char> = Vec::new();
    let mut table: Vec<Vec<String>> = Vec::new();
    let mut cell: Option<String> = None;
    let mut link_stack: Vec<(Option<String>, LinkType, usize)> = Vec::new();

    // Write to the current cell, the title, or the body.
    macro_rules! emit {
        ($($t:tt)*) => {{
            let piece = format!($($t)*);
            if let Some(c) = cell.as_mut() { c.push_str(&piece); }
            else if in_title { title.push_str(&piece); }
            else { s.push_str(&piece); }
        }};
    }

    for ev in events {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Heading { level, .. } => {
                    if title_pending {
                        in_title = true;
                        title_pending = false;
                    } else {
                        let l = (level as u8).saturating_sub(top) + 1;
                        emit!("\\{}{{", heading_cmd(l));
                    }
                }
                Tag::Paragraph => {}
                Tag::BlockQuote(_) => emit!("\\begin{{quote}}\n"),
                Tag::CodeBlock(_) => {
                    in_code = true;
                    emit!("\\begin{{verbatim}}\n");
                }
                Tag::List(start) => {
                    let ordered = start.is_some();
                    emit!("{}\\begin{{{}}}\n", "  ".repeat(lists.len()), env(ordered));
                    if let Some(n) = start.filter(|n| *n != 1) {
                        emit!("\\setcounter{{enumi}}{{{}}}\n", n.saturating_sub(1));
                    }
                    lists.push(ordered);
                }
                Tag::Item => emit!("{}\\item ", "  ".repeat(lists.len())),
                Tag::Emphasis => emit!("\\emph{{"),
                Tag::Strong => emit!("\\textbf{{"),
                Tag::Strikethrough => {
                    out.packages.insert("ulem");
                    emit!("\\sout{{");
                }
                Tag::Link { link_type, dest_url, .. } => {
                    let start = if let Some(c) = cell.as_ref() { c.len() } else if in_title { title.len() } else { s.len() };
                    let url = if matches!(link_type, LinkType::Email) && !dest_url.contains(':') {
                        format!("mailto:{dest_url}")
                    } else {
                        dest_url.to_string()
                    };
                    match safe_url(&url) {
                        Some(url) => {
                            out.packages.insert("hyperref");
                            if matches!(link_type, LinkType::Autolink | LinkType::Email) {
                                emit!("\\url{{{}", escape_url(url.trim_start_matches("mailto:")));
                            } else {
                                emit!("\\href{{{}}}{{", escape_url(&url));
                            }
                            link_stack.push((Some(url), link_type, start));
                        }
                        // Unsafe or relative link: keep the text, drop the link.
                        None => link_stack.push((None, LinkType::Inline, start)),
                    }
                }
                Tag::Image { .. } => emit!("\\textit{{[image: "),
                Tag::Table(align) => {
                    table_align = align
                        .iter()
                        .map(|a| match a {
                            Alignment::Center => 'c',
                            Alignment::Right => 'r',
                            _ => 'l',
                        })
                        .collect();
                    table.clear();
                }
                Tag::TableHead | Tag::TableRow => table.push(Vec::new()),
                Tag::TableCell => cell = Some(String::new()),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Heading(_) => {
                    if in_title {
                        in_title = false;
                    } else {
                        emit!("}}\n\n");
                    }
                }
                TagEnd::Paragraph => emit!("\n\n"),
                TagEnd::BlockQuote(_) => emit!("\\end{{quote}}\n\n"),
                TagEnd::CodeBlock => {
                    in_code = false;
                    if !s.ends_with('\n') {
                        s.push('\n');
                    }
                    emit!("\\end{{verbatim}}\n\n");
                }
                TagEnd::List(_) => {
                    let ordered = lists.pop().unwrap_or(false);
                    if !s.ends_with('\n') {
                        s.push('\n');
                    }
                    emit!("{}\\end{{{}}}\n", "  ".repeat(lists.len()), env(ordered));
                    if lists.is_empty() {
                        emit!("\n");
                    }
                }
                TagEnd::Item => {
                    if !s.ends_with('\n') {
                        s.push('\n');
                    }
                }
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => emit!("}}"),
                TagEnd::Link => {
                    if let Some((Some(_), _, _)) = link_stack.pop() {
                        emit!("}}");
                    }
                }
                TagEnd::Image => emit!("]}}"),
                TagEnd::TableCell => {
                    if let (Some(c), Some(row)) = (cell.take(), table.last_mut()) {
                        row.push(c);
                    }
                }
                TagEnd::TableHead => {
                    if let Some(row) = table.last_mut() {
                        for c in row.iter_mut() {
                            *c = format!("\\textbf{{{}}}", c.trim());
                        }
                    }
                }
                TagEnd::Table => {
                    let mut body = String::new();
                    write_table(&mut body, &table, &table_align);
                    s.push_str(&body);
                    table.clear();
                }
                _ => {}
            },
            Event::Text(t) => {
                if in_code {
                    s.push_str(&t);
                } else if link_stack.last().is_some_and(|(u, k, _)| u.is_some() && matches!(k, LinkType::Autolink | LinkType::Email)) {
                    // \url{…} already has the address; skip the visible text.
                } else {
                    emit!("{}", escape(&t));
                }
            }
            Event::Code(t) => emit!("\\texttt{{{}}}", escape(&t)),
            Event::InlineMath(m) => {
                if math_is_safe(&m) {
                    out.packages.insert("amsmath");
                    out.packages.insert("amssymb");
                    emit!("${}$", m)
                } else {
                    emit!("\\${}\\$", escape(&m))
                }
            }
            Event::DisplayMath(m) => {
                if math_is_safe(&m) {
                    out.packages.insert("amsmath");
                    out.packages.insert("amssymb");
                    emit!("\n\\[\n{}\n\\]\n", m.trim())
                } else {
                    emit!("\n\\$\\${}\\$\\$\n", escape(&m))
                }
            }
            Event::SoftBreak => emit!("\n"),
            Event::HardBreak => emit!("\\\\\n"),
            Event::Rule => emit!("\\noindent\\rule{{\\linewidth}}{{0.4pt}}\n\n"),
            Event::TaskListMarker(done) => {
                out.packages.insert("amssymb");
                emit!("{} ", if done { "$\\boxtimes$" } else { "$\\square$" });
            }
            Event::Html(h) | Event::InlineHtml(h)
                if h.trim().eq_ignore_ascii_case("<br>") || h.trim().eq_ignore_ascii_case("<br/>") =>
            {
                emit!("\\\\\n");
            }
            _ => {}
        }
    }
    if !title.trim().is_empty() {
        out.title = Some(title.trim().to_string());
    }
    out.body = tidy(&s);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, size: f64) -> RichRun {
        RichRun { text: text.into(), style: Style::default(), size }
    }
    fn styled(text: &str, size: f64, f: impl FnOnce(&mut Style)) -> RichRun {
        let mut r = run(text, size);
        f(&mut r.style);
        r
    }
    fn para(runs: Vec<RichRun>) -> RichPara {
        RichPara { runs, ..Default::default() }
    }

    #[test]
    fn escapes_special_characters() {
        assert_eq!(escape("50% off & $5 #1 a_b {x} ~ ^"), "50\\% off \\& \\$5 \\#1 a\\_b \\{x\\} \\textasciitilde{} \\^{}");
        assert_eq!(escape("“Hi” — it’s…"), "``Hi'' --- it's\\ldots{}");
        assert_eq!(escape("C:\\path"), "C:\\textbackslash{}path");
    }

    #[test]
    fn detects_latex() {
        assert!(looks_like_latex("\\begin{equation} x \\end{equation}"));
        assert!(looks_like_latex("Use \\textbf{x} and \\emph{y}"));
        assert!(!looks_like_latex("Costs $5 and 10%"));
    }

    #[test]
    fn rich_headings_lists_styles() {
        let paras = vec![
            para(vec![run("Quarterly Report", 24.0)]),
            para(vec![run("Introduction", 18.0)]),
            para(vec![run("This is ", 12.0), styled("bold", 12.0, |s| s.bold = true), run(" and ", 12.0), styled("italic", 12.0, |s| s.italic = true), run(" with 100% & more.", 12.0)]),
            para(vec![styled("Details", 12.0, |s| s.bold = true)]),
            RichPara { runs: vec![run("first", 12.0)], list: Some((false, 1)), cell: None },
            RichPara { runs: vec![run("nested", 12.0)], list: Some((true, 2)), cell: None },
            RichPara { runs: vec![run("second", 12.0)], list: Some((false, 1)), cell: None },
            para(vec![run("See ", 12.0), styled("our site", 12.0, |s| s.link = Some("https://example.com".into())), run(".", 12.0)]),
        ];
        let c = from_rich(&paras, true);
        assert_eq!(c.title.as_deref(), Some("Quarterly Report"));
        assert!(c.body.contains("\\section{Introduction}"), "{}", c.body);
        assert!(c.body.contains("This is \\textbf{bold} and \\emph{italic} with 100\\% \\& more."), "{}", c.body);
        assert!(c.body.contains("\\subsection{Details}"), "{}", c.body);
        assert!(c.body.contains("\\begin{itemize}\n  \\item first\n  \\begin{enumerate}\n    \\item nested\n  \\end{enumerate}\n  \\item second\n\\end{itemize}"), "{}", c.body);
        assert!(c.body.contains("\\href{https://example.com}{our site}"), "{}", c.body);
        assert!(c.packages.contains("hyperref"));
    }

    #[test]
    fn rich_tables_and_code() {
        let cell = |row, col, t: &str| RichPara {
            runs: vec![run(t, 12.0)],
            list: None,
            cell: Some(TableCell { table: 7, row, col, columns: 2 }),
        };
        let mono = |t: &str| para(vec![styled(t, 11.0, |s| s.mono = true)]);
        let paras = vec![
            para(vec![run("Text before.", 12.0)]),
            cell(0, 0, "Name"), cell(0, 1, "Price"), cell(1, 0, "Tea"), cell(1, 1, "$3"),
            mono("fn main() {"), mono("    println!(\"50%\");"), mono("}"),
            para(vec![run("Text after.", 12.0)]),
        ];
        let c = from_rich(&paras, false);
        assert!(c.body.contains("\\begin{tabular}{ll}\n\\hline\nName & Price \\\\\n\\hline\nTea & \\$3 \\\\\n\\hline\n\\end{tabular}"), "{}", c.body);
        assert!(c.body.contains("\\begin{verbatim}\nfn main() {\n    println!(\"50%\");\n}\n\\end{verbatim}"), "{}", c.body);
    }

    #[test]
    fn plain_rich_text_falls_back_to_markdown() {
        let paras = vec![para(vec![run("# Hi", 13.0)]), para(vec![run("- a", 13.0)])];
        assert!(rich_is_plain(&paras));
        let styled_paras = vec![para(vec![styled("Hi", 13.0, |s| s.bold = true)])];
        assert!(!rich_is_plain(&styled_paras));
    }

    #[test]
    fn markdown_basics() {
        let md = "# Trip notes\n\n## Packing\n\n- [x] passport\n- [ ] charger\n\nCost: **$40** for _two_ nights, 10% off. See <https://example.com> or [the map](https://maps.example.com/a#b).\n\n1. Fly\n2. Drive\n\n| Day | Plan |\n|:---|:---:|\n| Mon | Hike |\n\n```\nlet x = 5 % 2;\n```\n\nInline math $e^{i\\pi}$ and ~~old~~.\n";
        let c = from_markdown(md, true);
        assert_eq!(c.title.as_deref(), Some("Trip notes"));
        let b = &c.body;
        assert!(b.contains("\\section{Packing}"), "{b}");
        assert!(b.contains("\\item $\\boxtimes$ passport"), "{b}");
        assert!(b.contains("\\textbf{\\$40} for \\emph{two} nights, 10\\% off"), "{b}");
        assert!(b.contains("\\url{https://example.com}"), "{b}");
        assert!(b.contains("\\href{https://maps.example.com/a\\#b}{the map}"), "{b}");
        assert!(b.contains("\\begin{enumerate}\n  \\item Fly\n  \\item Drive\n\\end{enumerate}"), "{b}");
        assert!(b.contains("\\begin{tabular}{lc}"), "{b}");
        assert!(b.contains("\\textbf{Day} & \\textbf{Plan}"), "{b}");
        assert!(b.contains("\\begin{verbatim}\nlet x = 5 % 2;\n\\end{verbatim}"), "{b}");
        assert!(b.contains("$e^{i\\pi}$"), "{b}");
        assert!(b.contains("\\sout{old}"), "{b}");
    }

    #[test]
    fn plain_notes() {
        let note = "Groceries\nmilk & eggs\n• bread\n• 2% milk\nCall Sam at 5pm";
        let c = from_plain(note, true);
        assert_eq!(c.title.as_deref(), Some("Groceries"));
        assert!(c.body.contains("milk \\& eggs\n\n"), "{}", c.body);
        assert!(c.body.contains("\\begin{itemize}\n  \\item bread\n  \\item 2\\% milk\n\\end{itemize}"), "{}", c.body);
        assert!(c.body.contains("Call Sam at 5pm"), "{}", c.body);
        // Inserting (not a new document): the title becomes a section instead.
        let c = from_plain(note, false);
        assert!(c.title.is_none() && c.body.starts_with("\\section{Groceries}"), "{}", c.body);
    }

    #[test]
    fn only_safe_links_survive() {
        for bad in ["javascript:alert(1)", "JaVaScRiPt:x", "file:///etc/passwd", "vbscript:x", "data:text/html,x", "x-man-page://ls", "#top", "page.html", "https://a b", "http://"] {
            assert!(safe_url(bad).is_none(), "{bad}");
        }
        assert!(safe_url("https://example.com/a?b=c#d").is_some());
        assert!(safe_url("mailto:me@example.com").is_some());
        let c = from_markdown("[click](javascript:alert(1)) and [ok](https://ok.com) <file:///etc/passwd>", false);
        assert!(!c.body.contains("javascript") && c.body.contains("click"), "{}", c.body);
        assert!(c.body.contains("\\href{https://ok.com}{ok}"), "{}", c.body);
        assert!(!c.body.contains("\\url{file"), "{}", c.body);
        let r = vec![RichPara { runs: vec![styled("x", 12.0, |s| s.link = Some("javascript:evil()".into()))], ..Default::default() }];
        assert!(!from_rich(&r, false).body.contains("href"));
    }

    #[test]
    fn hostile_math_is_shown_as_text() {
        for bad in [
            "\\input{/etc/passwd}", "\\immediate\\write18{rm -rf ~}", "x \\def\\a{b}", "^^5cinput{x}",
            "a } \\input{x} {", "a % hidden", "\\csname input\\endcsname", "\\begin{verbatim}", "\\catcode`\\@=11",
            "\\openout1=x", "\\@input", "\\text{\\input{x}}",
        ] {
            assert!(!math_is_safe(bad), "{bad}");
            let c = from_markdown(&format!("Try ${bad}$ now"), false);
            assert!(!c.body.contains("$\\input") && !c.body.contains("\\write18{") || c.body.contains("\\$"), "{}", c.body);
        }
        for good in ["e^{i\\pi} + 1 = 0", "\\frac{a}{b}", "\\sum_{k=1}^{n} k", "\\begin{pmatrix} 1 & 2 \\\\ 3 & 4 \\end{pmatrix}", "\\mathbb{R}^n", "a \\, b \\; c"] {
            assert!(math_is_safe(good), "{good}");
        }
        let c = from_markdown("Euler: $e^{i\\pi}+1=0$", false);
        assert!(c.body.contains("$e^{i\\pi}+1=0$"));
    }

    #[test]
    fn strips_hidden_characters() {
        let t = "admin\u{202e}gnp.exe\u{2066}x\u{0007}y\u{0000}z";
        assert!(has_bidi_overrides(t));
        assert_eq!(escape(t), "admingnp.exexyz");
    }

    #[test]
    fn flags_risky_pasted_latex() {
        assert_eq!(risky_latex("\\section{x} \\textbf{y}"), Vec::<&str>::new());
        let r = risky_latex("\\input{/etc/passwd} \\immediate\\write18{ls}");
        assert!(r.contains(&"\\input") && r.contains(&"\\write") && r.contains(&"\\immediate"));
    }

    #[test]
    fn html_sanitizer_blocks_remote_content_and_scripts() {
        let evil = r#"<html><head><title>t</title><link rel=stylesheet href="https://evil/x.css"><meta http-equiv="refresh" content="0;url=https://evil"><style>body{background:url(https://evil/p.png)}</style></head>
<body onload="steal()"><script>alert(1)</script><SCRIPT SRC=https://evil/x.js></SCRIPT>
<p style="color:red">Hello <b>bold</b> <img src="https://evil/track.png" onerror=alert(1)> <a href="javascript:alert(1)">bad</a> <a href="&#106;avascript:alert(1)">bad2</a> <a href="java&#x09;script:x">bad3</a> <a href="https://good.com/" onclick="x()">good</a></p>
<p style="background:url(https://evil/bg)">x</p><iframe src="https://evil"></iframe><svg><image href="https://evil"/></svg><object data=x></object>
<table><tr><td colspan="2">cell</td></tr></table><!-- <img src=x> --> 3 < 5 &amp; done</body></html>"#;
        let clean = sanitize_html(evil);
        let l = clean.to_ascii_lowercase();
        for bad in ["evil", "script", "onload", "onerror", "onclick", "javascript", "<img", "<link", "<meta", "<style", "<iframe", "<svg", "<object", "url(", "alert"] {
            assert!(!l.contains(bad), "{bad} survived:\n{clean}");
        }
        for good in ["Hello", "<b>bold</b>", "href=\"https://good.com/\"", "style=\"color:red\"", "colspan=\"2\"", "cell", "&amp; done", "good"] {
            assert!(clean.contains(good), "{good} lost:\n{clean}");
        }
    }

    #[test]
    fn html_sanitizer_handles_tricky_markup() {
        for (input, banned) in [
            ("<scr<script>ipt>alert(1)</script>", "<script"),
            ("<img/src=x/onerror=alert(1)>", "onerror"),
            ("<a href='https://ok' onmouseover='x'>t</a>", "onmouseover"),
            ("<div style=\"width:expression(alert(1))\">t</div>", "expression"),
            ("<p style='background-image:\\75 rl(x)'>t</p>", "rl("),
            ("<A HREF=\"JAVASCRIPT:x\">t</A>", "javascript"),
            ("<style>@import 'https://evil'</style>t", "evil"),
            ("<unclosed <b>text", "<unclosed"),
        ] {
            let out = sanitize_html(input).to_ascii_lowercase();
            assert!(!out.contains(banned), "{input:?} -> {out:?}");
        }
    }

    /// Random and adversarial inputs: nothing may panic, and sanitised HTML
    /// must never contain anything that runs or loads remote content.
    #[test]
    fn fuzz_parsers() {
        const FRAGS: &[&str] = &[
            "<", ">", "</", "<script>", "</script>", "<SCRIPT", "<img src=x>", "<a href=\"", "javascript:", "&#106;",
            "&#x6a;", "&", ";", "\"", "'", "=", " ", "\n", "\t", "<b>", "</b>", "<style>", "url(", "onload=",
            "<!--", "-->", "<!", "<?", "\\", "{", "}", "$", "$$", "%", "#", "^^", "\\input", "\\def", "\\frac",
            "é", "中", "\u{202e}", "\u{0}", "\u{7}", "**", "_", "- ", "1. ", "# ", "|", "---", "```", "[x](",
            ")", "<https://a>", "\\begin{", "pmatrix", "\\end{", "<svg>", "<iframe", "style=\"", "expression(",
            "<td colspan=", "<p", "x", "y", "\u{2028}", "•", "\r\n", "\\documentclass{article}", "\\begin{document}", "\\end{document}", "\\begin{itemize}", "\\item", "x_1", "a^2", "\\secton", "\\usepackage{x}", "&",
        ];
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..200_000 {
            let n = (next() % 24) as usize;
            let mut input = String::new();
            for _ in 0..n {
                input.push_str(FRAGS[(next() % FRAGS.len() as u64) as usize]);
            }
            let clean = sanitize_html(&input).to_ascii_lowercase();
            // Text can mention anything; what matters is every tag we emit:
            // only allowed tags, only allowed attributes, only safe values.
            let mut rest = clean.as_str();
            while let Some(p) = rest.find('<') {
                let tag_end = rest[p..].find('>').map(|e| p + e + 1).unwrap_or(rest.len());
                let tag = &rest[p + 1..tag_end.saturating_sub(1).max(p + 1)];
                let body = tag.trim_start_matches('/');
                let name_len = body.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(body.len());
                assert!(HTML_KEEP.contains(&&body[..name_len]), "tag {tag:?} from {input:?}");
                for (k, v) in parse_attrs(&body[name_len..]) {
                    let v = v.replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">");
                    match k.as_str() {
                        "href" => assert!(safe_url(&decode_entities(&v)).is_some(), "href {v:?} from {input:?}"),
                        "style" => assert!(safe_css(&v), "style {v:?} from {input:?}"),
                        "colspan" | "rowspan" | "align" | "start" => {}
                        other => panic!("attribute {other:?} in {tag:?} from {input:?}"),
                    }
                }
                rest = &rest[tag_end..];
            }
            let _ = math_is_safe(&input);
            let _ = risky_latex(&input);
            let _ = from_markdown(&input, true);
            let _ = from_plain(&input, false);
            let _ = escape(&input);
            let _ = safe_url(&input);
            let _ = crate::complete::context(&input);
            let _ = crate::hints::explain(&input, Some(&input), &input);
            let _ = crate::sync::SyncIndex::parse(&input);
            let errs = [
                crate::compile::TexError { message: "LaTeX Error: Missing \\begin{document}.".into(), line: Some((next() % 6) as usize) },
                crate::compile::TexError { message: "Missing $ inserted.".into(), line: Some((next() % 6) as usize) },
                crate::compile::TexError { message: "Undefined control sequence.".into(), line: Some((next() % 6) as usize) },
                crate::compile::TexError { message: "Misplaced alignment tab character &.".into(), line: Some((next() % 6) as usize) },
                crate::compile::TexError { message: "LaTeX Error: File `x.sty' not found.".into(), line: None },
            ];
            let fx = crate::fixes::propose(&input, &errs);
            let all: Vec<&crate::fixes::Fix> = fx.iter().collect();
            let _ = crate::fixes::apply(&input, &all);
        }
    }

    #[test]
    fn strips_manual_heading_numbers() {
        assert_eq!(strip_numbering("1.2 Methods"), "Methods");
        assert_eq!(strip_numbering("2024 plans"), "plans");
        assert_eq!(strip_numbering("Results"), "Results");
    }

    #[test]
    fn wraps_complete_document() {
        let c = from_markdown("# Title\n\nSee [x](https://a.b) and ~~y~~.\n", true);
        let doc = wrap_document(&c);
        assert!(doc.starts_with("\\documentclass{article}\n\\usepackage[normalem]{ulem}\n\\usepackage{hyperref}\n"), "{doc}");
        assert!(doc.contains("\\title{Title}") && doc.contains("\\maketitle"));
        assert!(doc.trim_end().ends_with("\\end{document}"));
    }

    /// Writes sample conversions to $TYPESET_DUMP so they can be compiled with
    /// a real TeX engine:  TYPESET_DUMP=/tmp/x cargo test dump -- --ignored
    #[test]
    #[ignore]
    fn dump_samples_for_tex() {
        let Some(dir) = std::env::var_os("TYPESET_DUMP") else { return };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let nasty = "50% & $5 #tag a_b {x} ~home ^up \\back <tag> | pipe “quotes” — dash … é ü 中";
        let md = format!(
            "# {nasty}\n\n## Sub {nasty}\n\n{nasty}\n\n- [x] {nasty}\n- [ ] item\n  1. nested {nasty}\n\n> quote {nasty}\n\n\
             | {nasty} | b |\n|---|--:|\n| x & y | 100% |\n\n```\n{nasty}\n```\n\n[link {nasty}](https://ex.com/a%20b#c?d=1&e=2) <https://ex.com/x_y> \
             `code {nasty}` **bold _both_** ~~gone~~ $x^2_i$\n\n$$\\int_0^1 f$$\n\n---\n\n![alt](pic.png)\n"
        );
        std::fs::write(dir.join("markdown.tex"), wrap_document(&from_markdown(&md, true))).unwrap();
        std::fs::write(dir.join("plain.tex"), wrap_document(&from_plain(&format!("Note title\n{nasty}\n• {nasty}\n• two\n1) {nasty}\n2) b\nend"), true))).unwrap();
        let r = |t: &str, size: f64| RichRun { text: t.into(), style: Style::default(), size };
        let st = |t: &str, f: &dyn Fn(&mut Style)| { let mut x = r(t, 12.0); f(&mut x.style); x };
        let cell = |row, col, t: &str| RichPara { runs: vec![r(t, 12.0)], list: None, cell: Some(TableCell { table: 1, row, col, columns: 2 }) };
        let paras = vec![
            RichPara { runs: vec![r(nasty, 26.0)], ..Default::default() },
            RichPara { runs: vec![r(&format!("2.1 {nasty}"), 18.0)], ..Default::default() },
            RichPara { runs: vec![r(nasty, 12.0), st(" bold ", &|s| s.bold = true), st("ital", &|s| { s.italic = true; s.underline = true }), st("x", &|s| s.script = 1), st("2", &|s| s.script = -1), st(nasty, &|s| s.strike = true), st("link %", &|s| s.link = Some("https://ex.com/%20#x".into())), st("mono {}", &|s| s.mono = true), r("line\u{2028}break", 12.0)], ..Default::default() },
            RichPara { runs: vec![st(nasty, &|s| s.bold = true)], ..Default::default() },
            RichPara { runs: vec![r(nasty, 12.0)], list: Some((true, 1)), cell: None },
            RichPara { runs: vec![r(nasty, 12.0)], list: Some((false, 2)), cell: None },
            RichPara { runs: vec![r(nasty, 12.0)], list: Some((false, 3)), cell: None },
            cell(0, 0, nasty), cell(0, 1, "h"), cell(1, 0, "a & b"), cell(1, 1, "50%"),
            RichPara { runs: vec![st(nasty, &|s| s.mono = true)], ..Default::default() },
            RichPara { runs: vec![r("end", 12.0)], ..Default::default() },
        ];
        std::fs::write(dir.join("rich.tex"), wrap_document(&from_rich(&paras, true))).unwrap();
    }
}
