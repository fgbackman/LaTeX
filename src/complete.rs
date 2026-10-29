//! Overleaf-style autocomplete for LaTeX. Pure Rust (no AppKit), unit-tested.
//!
//! The editor asks two questions — `context(line_before_cursor)`: is the
//! cursor somewhere we can help? and `candidates(..)`: what should the list
//! show? When the user picks one, `expansion(..)` says what text to insert
//! and where the cursor goes.

use std::path::Path;

/// Where the cursor is, in terms of what can be completed there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ctx {
    /// `\sec|` — `prefix` includes the backslash.
    Command { prefix: String },
    /// `\begin{ali|`
    BeginEnv { prefix: String },
    /// `\end{ali|`
    EndEnv { prefix: String },
    /// `\ref{fig:|`, `\eqref{`, `\autoref{`, `\cref{`, `\pageref{`
    Ref { prefix: String },
    /// `\cite{knu|` (also after a comma: `\cite{a,b|`)
    Cite { prefix: String },
    /// `\usepackage{ams|`
    Package { prefix: String },
    /// `\documentclass{art|`
    Class { prefix: String },
    /// `\includegraphics{fig|`
    Graphic { prefix: String },
    /// `\input{ch|` / `\include{ch|`
    TexFile { prefix: String },
}

impl Ctx {
    /// The text being completed (what the suggestion list filters on).
    pub fn prefix(&self) -> &str {
        match self {
            Ctx::Command { prefix }
            | Ctx::BeginEnv { prefix }
            | Ctx::EndEnv { prefix }
            | Ctx::Ref { prefix }
            | Ctx::Cite { prefix }
            | Ctx::Package { prefix }
            | Ctx::Class { prefix }
            | Ctx::Graphic { prefix }
            | Ctx::TexFile { prefix } => prefix,
        }
    }
}

/// Look at the current line up to the cursor and decide what can be completed.
pub fn context(before: &str) -> Option<Ctx> {
    // Never complete inside a comment.
    if in_comment(before) {
        return None;
    }

    // 1. Inside a braced argument: `\name[opt]{partial`
    if let Some(open) = before.rfind('{') {
        let partial = &before[open + 1..];
        if !partial.contains('}') && !partial.contains('\\') && !partial.contains('{') {
            let head = strip_optional_arg(&before[..open]);
            if let Some(name) = trailing_command(head) {
                let last = |p: &str| p.rsplit(',').next().unwrap_or("").trim_start().to_string();
                let ctx = match name {
                    "begin" => Some(Ctx::BeginEnv { prefix: partial.into() }),
                    "end" => Some(Ctx::EndEnv { prefix: partial.into() }),
                    "ref" | "eqref" | "autoref" | "cref" | "Cref" | "pageref" | "nameref" | "vref" => {
                        Some(Ctx::Ref { prefix: partial.into() })
                    }
                    "cite" | "citep" | "citet" | "citeauthor" | "citeyear" | "nocite" | "parencite"
                    | "textcite" | "autocite" | "footcite" => Some(Ctx::Cite { prefix: last(partial) }),
                    "usepackage" | "RequirePackage" => Some(Ctx::Package { prefix: last(partial) }),
                    "documentclass" => Some(Ctx::Class { prefix: partial.into() }),
                    "includegraphics" => Some(Ctx::Graphic { prefix: partial.into() }),
                    "input" | "include" | "subfile" => Some(Ctx::TexFile { prefix: partial.into() }),
                    _ => None,
                };
                if ctx.is_some() {
                    return ctx;
                }
            }
        }
    }

    // 2. A command name being typed: `\sec`
    let letters = before.len() - before.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '@').len();
    let start = before.len() - letters;
    if letters >= 1 && start >= 1 && before[..start].ends_with('\\') && !escaped_backslash(&before[..start - 1]) {
        return Some(Ctx::Command { prefix: before[start - 1..].to_string() });
    }
    None
}

/// `\name` immediately at the end of `s` (after optional whitespace) → `name`.
fn trailing_command(s: &str) -> Option<&str> {
    let s = s.trim_end();
    let name_len = s.len() - s.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == '*').len();
    let start = s.len() - name_len;
    if name_len == 0 || start == 0 || !s[..start].ends_with('\\') {
        return None;
    }
    Some(s[start..].trim_end_matches('*'))
}

/// Drop one trailing `[...]` optional argument: `\includegraphics[width=3cm]`.
fn strip_optional_arg(s: &str) -> &str {
    if s.ends_with(']') {
        if let Some(i) = s.rfind('[') {
            return &s[..i];
        }
    }
    s
}

/// True when `s` ends in an odd number of backslashes, i.e. the next
/// backslash is escaped (`\\` line break, not a command).
fn escaped_backslash(s: &str) -> bool {
    let n = s.len() - s.trim_end_matches('\\').len();
    n % 2 == 1
}

fn in_comment(line: &str) -> bool {
    let b = line.as_bytes();
    for (i, &c) in b.iter().enumerate() {
        if c == b'%' {
            let slashes = b[..i].iter().rev().take_while(|&&x| x == b'\\').count();
            if slashes % 2 == 0 {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Built-in knowledge
// ---------------------------------------------------------------------------

/// (command, snippet, hint). In snippets `¦` marks where the cursor lands.
pub const COMMANDS: &[(&str, &str, &str)] = &[
    // structure
    ("\\section", "\\section{¦}", "Numbered section heading"),
    ("\\section*", "\\section*{¦}", "Unnumbered section heading"),
    ("\\subsection", "\\subsection{¦}", "Numbered subsection"),
    ("\\subsection*", "\\subsection*{¦}", "Unnumbered subsection"),
    ("\\subsubsection", "\\subsubsection{¦}", "Third-level heading"),
    ("\\paragraph", "\\paragraph{¦}", "Run-in paragraph heading"),
    ("\\chapter", "\\chapter{¦}", "Chapter (report/book classes)"),
    ("\\part", "\\part{¦}", "Part heading"),
    ("\\title", "\\title{¦}", "Document title (shown by \\maketitle)"),
    ("\\author", "\\author{¦}", "Author name(s); separate with \\and"),
    ("\\date", "\\date{¦}", "Date; \\today for today's date"),
    ("\\maketitle", "\\maketitle", "Print the title block"),
    ("\\tableofcontents", "\\tableofcontents", "Table of contents (compiles twice)"),
    ("\\begin", "\\begin{¦}", "Start an environment"),
    ("\\end", "\\end{¦}", "End an environment"),
    ("\\documentclass", "\\documentclass{¦}", "Document type: article, report, book, beamer…"),
    ("\\usepackage", "\\usepackage{¦}", "Load a package"),
    ("\\item", "\\item ¦", "List item"),
    ("\\label", "\\label{¦}", "Name this spot so \\ref can point to it"),
    ("\\ref", "\\ref{¦}", "Number of a \\label"),
    ("\\eqref", "\\eqref{¦}", "Equation number in parentheses (amsmath)"),
    ("\\pageref", "\\pageref{¦}", "Page number of a \\label"),
    ("\\autoref", "\\autoref{¦}", "\"Figure 3\"-style reference (hyperref)"),
    ("\\cite", "\\cite{¦}", "Citation"),
    ("\\footnote", "\\footnote{¦}", "Footnote"),
    ("\\caption", "\\caption{¦}", "Caption for a figure or table"),
    ("\\includegraphics", "\\includegraphics[width=\\linewidth]{¦}", "Insert an image (graphicx)"),
    ("\\input", "\\input{¦}", "Insert another .tex file here"),
    ("\\include", "\\include{¦}", "Include a chapter file on a new page"),
    ("\\bibliography", "\\bibliography{¦}", "Use references from a .bib file"),
    ("\\bibliographystyle", "\\bibliographystyle{¦}", "Citation style: plain, alpha, abbrv…"),
    ("\\appendix", "\\appendix", "Start appendices"),
    ("\\newpage", "\\newpage", "Start a new page"),
    ("\\clearpage", "\\clearpage", "New page, flushing figures"),
    ("\\newline", "\\newline", "Line break"),
    ("\\noindent", "\\noindent", "No indent for this paragraph"),
    ("\\centering", "\\centering", "Centre the rest of this group"),
    ("\\hspace", "\\hspace{¦}", "Horizontal space, e.g. 1em"),
    ("\\vspace", "\\vspace{¦}", "Vertical space, e.g. 1em"),
    ("\\hfill", "\\hfill", "Stretchy horizontal space"),
    ("\\vfill", "\\vfill", "Stretchy vertical space"),
    ("\\newcommand", "\\newcommand{\\¦}{}", "Define your own command"),
    ("\\renewcommand", "\\renewcommand{\\¦}{}", "Redefine a command"),
    ("\\url", "\\url{¦}", "Web address (url or hyperref)"),
    ("\\href", "\\href{¦}{}", "Link with text (hyperref)"),
    // text style
    ("\\textbf", "\\textbf{¦}", "Bold"),
    ("\\textit", "\\textit{¦}", "Italic"),
    ("\\emph", "\\emph{¦}", "Emphasis (italic)"),
    ("\\underline", "\\underline{¦}", "Underline"),
    ("\\texttt", "\\texttt{¦}", "Typewriter / code font"),
    ("\\textsc", "\\textsc{¦}", "Small capitals"),
    ("\\textrm", "\\textrm{¦}", "Roman (serif) font"),
    ("\\textsf", "\\textsf{¦}", "Sans-serif font"),
    ("\\textcolor", "\\textcolor{¦}{}", "Coloured text (xcolor)"),
    ("\\tiny", "\\tiny", "Smallest text size"),
    ("\\small", "\\small", "Small text"),
    ("\\large", "\\large", "Larger text"),
    ("\\Large", "\\Large", "Even larger text"),
    ("\\huge", "\\huge", "Huge text"),
    ("\\today", "\\today", "Today's date"),
    ("\\ldots", "\\ldots", "Ellipsis …"),
    ("\\LaTeX", "\\LaTeX", "The LaTeX logo"),
    ("\\TeX", "\\TeX", "The TeX logo"),
    // math
    ("\\frac", "\\frac{¦}{}", "Fraction: \\frac{top}{bottom}"),
    ("\\dfrac", "\\dfrac{¦}{}", "Display-size fraction (amsmath)"),
    ("\\sqrt", "\\sqrt{¦}", "Square root; \\sqrt[n]{x} for nth root"),
    ("\\sum", "\\sum_{¦}^{}", "Sum ∑ with limits"),
    ("\\prod", "\\prod_{¦}^{}", "Product ∏ with limits"),
    ("\\int", "\\int_{¦}^{}", "Integral ∫ with limits"),
    ("\\iint", "\\iint", "Double integral (amsmath)"),
    ("\\oint", "\\oint", "Contour integral"),
    ("\\lim", "\\lim_{¦}", "Limit"),
    ("\\infty", "\\infty", "Infinity ∞"),
    ("\\partial", "\\partial", "Partial derivative ∂"),
    ("\\nabla", "\\nabla", "Nabla ∇"),
    ("\\cdot", "\\cdot", "Centred dot ⋅"),
    ("\\cdots", "\\cdots", "Centred dots ⋯"),
    ("\\times", "\\times", "Times ×"),
    ("\\div", "\\div", "Division ÷"),
    ("\\pm", "\\pm", "Plus-minus ±"),
    ("\\leq", "\\leq", "Less or equal ≤"),
    ("\\geq", "\\geq", "Greater or equal ≥"),
    ("\\neq", "\\neq", "Not equal ≠"),
    ("\\approx", "\\approx", "Approximately ≈"),
    ("\\equiv", "\\equiv", "Equivalent ≡"),
    ("\\sim", "\\sim", "Similar ∼"),
    ("\\propto", "\\propto", "Proportional ∝"),
    ("\\in", "\\in", "Element of ∈"),
    ("\\notin", "\\notin", "Not an element ∉"),
    ("\\subset", "\\subset", "Subset ⊂"),
    ("\\subseteq", "\\subseteq", "Subset or equal ⊆"),
    ("\\cup", "\\cup", "Union ∪"),
    ("\\cap", "\\cap", "Intersection ∩"),
    ("\\emptyset", "\\emptyset", "Empty set ∅"),
    ("\\forall", "\\forall", "For all ∀"),
    ("\\exists", "\\exists", "There exists ∃"),
    ("\\to", "\\to", "Arrow →"),
    ("\\rightarrow", "\\rightarrow", "Right arrow →"),
    ("\\leftarrow", "\\leftarrow", "Left arrow ←"),
    ("\\Rightarrow", "\\Rightarrow", "Implies ⇒"),
    ("\\Leftrightarrow", "\\Leftrightarrow", "If and only if ⇔"),
    ("\\mapsto", "\\mapsto", "Maps to ↦"),
    ("\\left", "\\left(¦\\right)", "Auto-sized brackets"),
    ("\\mathbb", "\\mathbb{¦}", "Blackboard bold ℝ ℕ ℤ (amssymb)"),
    ("\\mathcal", "\\mathcal{¦}", "Calligraphic letters"),
    ("\\mathbf", "\\mathbf{¦}", "Bold math"),
    ("\\mathrm", "\\mathrm{¦}", "Upright text in math"),
    ("\\text", "\\text{¦}", "Normal text inside math (amsmath)"),
    ("\\operatorname", "\\operatorname{¦}", "Custom function name (amsmath)"),
    ("\\hat", "\\hat{¦}", "Hat accent x̂"),
    ("\\bar", "\\bar{¦}", "Bar accent x̄"),
    ("\\vec", "\\vec{¦}", "Vector arrow"),
    ("\\dot", "\\dot{¦}", "Dot accent (time derivative)"),
    ("\\overline", "\\overline{¦}", "Line over an expression"),
    ("\\binom", "\\binom{¦}{}", "Binomial coefficient (amsmath)"),
    ("\\sin", "\\sin", "sine"),
    ("\\cos", "\\cos", "cosine"),
    ("\\tan", "\\tan", "tangent"),
    ("\\log", "\\log", "logarithm"),
    ("\\ln", "\\ln", "natural log"),
    ("\\exp", "\\exp", "exponential"),
    ("\\max", "\\max", "maximum"),
    ("\\min", "\\min", "minimum"),
    ("\\quad", "\\quad", "1em space in math"),
    ("\\qquad", "\\qquad", "2em space in math"),
    ("\\alpha", "\\alpha", "α"), ("\\beta", "\\beta", "β"), ("\\gamma", "\\gamma", "γ"),
    ("\\delta", "\\delta", "δ"), ("\\epsilon", "\\epsilon", "ϵ"), ("\\varepsilon", "\\varepsilon", "ε"),
    ("\\zeta", "\\zeta", "ζ"), ("\\eta", "\\eta", "η"), ("\\theta", "\\theta", "θ"),
    ("\\iota", "\\iota", "ι"), ("\\kappa", "\\kappa", "κ"), ("\\lambda", "\\lambda", "λ"),
    ("\\mu", "\\mu", "μ"), ("\\nu", "\\nu", "ν"), ("\\xi", "\\xi", "ξ"),
    ("\\pi", "\\pi", "π"), ("\\rho", "\\rho", "ρ"), ("\\sigma", "\\sigma", "σ"),
    ("\\tau", "\\tau", "τ"), ("\\phi", "\\phi", "ϕ"), ("\\varphi", "\\varphi", "φ"),
    ("\\chi", "\\chi", "χ"), ("\\psi", "\\psi", "ψ"), ("\\omega", "\\omega", "ω"),
    ("\\Gamma", "\\Gamma", "Γ"), ("\\Delta", "\\Delta", "Δ"), ("\\Theta", "\\Theta", "Θ"),
    ("\\Lambda", "\\Lambda", "Λ"), ("\\Pi", "\\Pi", "Π"), ("\\Sigma", "\\Sigma", "Σ"),
    ("\\Phi", "\\Phi", "Φ"), ("\\Psi", "\\Psi", "Ψ"), ("\\Omega", "\\Omega", "Ω"),
    // tables & floats
    ("\\hline", "\\hline", "Horizontal rule in a table"),
    ("\\toprule", "\\toprule", "Top rule (booktabs)"),
    ("\\midrule", "\\midrule", "Middle rule (booktabs)"),
    ("\\bottomrule", "\\bottomrule", "Bottom rule (booktabs)"),
    ("\\multicolumn", "\\multicolumn{¦}{c}{}", "Cell spanning columns"),
];

/// (environment, body inserted between \begin and \end, hint). `¦` = cursor.
pub const ENVIRONMENTS: &[(&str, &str, &str)] = &[
    ("equation", "¦", "Numbered equation"),
    ("equation*", "¦", "Unnumbered equation (amsmath)"),
    ("align", "¦ &= \\\\", "Aligned equations — & marks the alignment (amsmath)"),
    ("align*", "¦ &= \\\\", "Unnumbered aligned equations (amsmath)"),
    ("gather", "¦", "Centred equations (amsmath)"),
    ("itemize", "\\item ¦", "Bullet list"),
    ("enumerate", "\\item ¦", "Numbered list"),
    ("description", "\\item[¦] ", "Labelled list"),
    ("figure", "\\centering\n\\includegraphics[width=0.8\\linewidth]{¦}\n\\caption{}\n\\label{fig:}", "Figure with image, caption and label"),
    ("table", "\\centering\n\\begin{tabular}{ll}\n¦ & \\\\\n\\end{tabular}\n\\caption{}\n\\label{tab:}", "Table with caption and label"),
    ("tabular", "¦ & \\\\", "Grid of cells — add column spec like {lcr}"),
    ("center", "¦", "Centred block"),
    ("abstract", "¦", "Abstract"),
    ("quote", "¦", "Indented quotation"),
    ("verbatim", "¦", "Text exactly as typed"),
    ("minipage", "¦", "Box of fixed width: {0.45\\linewidth}"),
    ("matrix", "¦ & \\\\", "Matrix without brackets (amsmath)"),
    ("pmatrix", "¦ & \\\\", "Matrix in ( ) (amsmath)"),
    ("bmatrix", "¦ & \\\\", "Matrix in [ ] (amsmath)"),
    ("cases", "¦ & \\text{if } \\\\", "Piecewise definition (amsmath)"),
    ("theorem", "¦", "Theorem (define with \\newtheorem)"),
    ("lemma", "¦", "Lemma (define with \\newtheorem)"),
    ("proof", "¦", "Proof with ∎ (amsthm)"),
    ("frame", "\\frametitle{¦}\n", "Beamer slide"),
    ("document", "¦", "The document body"),
];

const PACKAGES: &[(&str, &str)] = &[
    ("amsmath", "Better equations: align, \\text, \\eqref"),
    ("amssymb", "Extra math symbols, \\mathbb"),
    ("amsthm", "Theorems and proofs"),
    ("graphicx", "\\includegraphics for images"),
    ("hyperref", "Clickable links and references"),
    ("xcolor", "Colours, \\textcolor"),
    ("geometry", "Page margins: \\usepackage[margin=1in]{geometry}"),
    ("booktabs", "Professional table rules"),
    ("siunitx", "Units and numbers: \\SI{3}{\\metre}"),
    ("tikz", "Drawing diagrams"),
    ("pgfplots", "Plots"),
    ("listings", "Source code listings"),
    ("caption", "Customise captions"),
    ("subcaption", "Sub-figures"),
    ("float", "[H] to place figures exactly here"),
    ("enumitem", "Customise lists"),
    ("fancyhdr", "Headers and footers"),
    ("natbib", "Author-year citations"),
    ("biblatex", "Modern bibliographies"),
    ("csquotes", "Proper quotation marks"),
    ("babel", "Language support"),
    ("fontspec", "Use any system font (XeTeX)"),
    ("microtype", "Subtle typographic polish"),
    ("mathtools", "amsmath extensions"),
    ("physics", "Physics notation shortcuts"),
    ("tabularx", "Tables that fit the page width"),
    ("multirow", "Cells spanning rows"),
    ("url", "Typeset web addresses"),
    ("cleveref", "\\cref: smart references"),
    ("algorithm2e", "Algorithms / pseudocode"),
];

const CLASSES: &[(&str, &str)] = &[
    ("article", "Papers and short documents"),
    ("report", "Longer documents with chapters"),
    ("book", "Books"),
    ("beamer", "Slides"),
    ("letter", "Letters"),
    ("amsart", "AMS journal article"),
    ("memoir", "Flexible book/report class"),
    ("scrartcl", "KOMA-Script article"),
];

/// One suggestion in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Shown in the list, and what the partial text is replaced with while browsing.
    pub label: String,
    pub hint: String,
}

/// Things found in the user's own document and project folder.
#[derive(Default, Debug)]
pub struct Found {
    pub labels: Vec<String>,
    pub cite_keys: Vec<String>,
    pub commands: Vec<String>,
    pub environments: Vec<String>,
}

/// Scan the document (and .bib files beside it) for labels, citation keys,
/// and commands/environments the user defined or already uses.
pub fn scan_document(doc: &str, project_dir: Option<&Path>) -> Found {
    let mut f = Found::default();
    for (cmd, arg) in braced_args(doc) {
        match cmd {
            "label" => push_unique(&mut f.labels, arg),
            "bibitem" => push_unique(&mut f.cite_keys, arg),
            "begin" => push_unique(&mut f.environments, arg),
            "newcommand" | "renewcommand" | "DeclareMathOperator" if arg.starts_with('\\') => {
                push_unique(&mut f.commands, arg)
            }
            "newenvironment" | "newtheorem" => push_unique(&mut f.environments, arg),
            _ => {}
        }
    }
    // Commands already used in this document (so your own macros come back).
    let b = doc.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            let s = i + 1;
            let mut j = s;
            while j < b.len() && (b[j].is_ascii_alphabetic() || b[j] == b'@') {
                j += 1;
            }
            if j - s >= 2 {
                push_unique(&mut f.commands, &doc[i..j]);
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    if let Some(dir) = project_dir {
        for bib in list_files(dir, &["bib"], 2) {
            let path = dir.join(&bib);
            // Skip anything huge (a real .bib is far smaller) and non-regular files.
            let ok = std::fs::symlink_metadata(&path).map(|m| m.is_file() && m.len() <= 5 * 1024 * 1024).unwrap_or(false);
            if !ok {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                for key in bib_keys(&text) {
                    push_unique(&mut f.cite_keys, &key);
                }
            }
        }
    }
    f
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    let s = s.trim();
    if !s.is_empty() && !v.iter().any(|x| x == s) {
        v.push(s.to_string());
    }
}

/// Every `\cmd{arg}` / `\cmd*{arg}` / `\cmd[opt]{arg}` (first argument only).
fn braced_args(doc: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut rest = doc;
    while let Some(p) = rest.find('\\') {
        let after = &rest[p + 1..];
        let n = after.len() - after.trim_start_matches(|c: char| c.is_ascii_alphabetic()).len();
        let name = &after[..n];
        let mut tail = after[n..].trim_start_matches('*');
        if tail.starts_with('[') {
            if let Some(e) = tail.find(']') {
                tail = &tail[e + 1..];
            }
        }
        if n > 0 && tail.starts_with('{') {
            if let Some(e) = tail.find('}') {
                if !tail[1..e].contains('\n') {
                    out.push((name, &tail[1..e]));
                }
            }
        }
        rest = &after[n..];
    }
    out
}

/// `@article{knuth84,` → `knuth84`
pub fn bib_keys(bib: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for line in bib.lines() {
        let l = line.trim_start();
        if let Some(rest) = l.strip_prefix('@') {
            if let Some(open) = rest.find(['{', '(']) {
                let kind = rest[..open].trim().to_ascii_lowercase();
                if kind == "comment" || kind == "string" || kind == "preamble" {
                    continue;
                }
                let key = rest[open + 1..].split(',').next().unwrap_or("").trim();
                if !key.is_empty() {
                    keys.push(key.to_string());
                }
            }
        }
    }
    keys
}

/// Files under `dir` (relative paths, depth-limited) with one of `exts`.
pub fn list_files(dir: &Path, exts: &[&str], depth: usize) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(base: &Path, rel: &Path, exts: &[&str], depth: usize, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(base.join(rel)) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let p = rel.join(&name);
            let ft = e.file_type();
            if ft.as_ref().map(|t| t.is_dir()).unwrap_or(false) {
                if depth > 0 {
                    walk(base, &p, exts, depth - 1, out);
                }
            } else if let Some(ext) = Path::new(&name).extension().and_then(|x| x.to_str()) {
                if exts.iter().any(|x| x.eq_ignore_ascii_case(ext)) {
                    out.push(p.to_string_lossy().into_owned());
                }
            }
            if out.len() > 500 {
                return;
            }
        }
    }
    walk(dir, Path::new(""), exts, depth, &mut out);
    out.sort();
    out
}

/// Environments opened but not yet closed before the cursor, innermost last.
pub fn open_environments(before_cursor: &str) -> Vec<String> {
    let mut stack: Vec<String> = Vec::new();
    for (cmd, arg) in braced_args(before_cursor) {
        match cmd {
            "begin" => stack.push(arg.to_string()),
            "end" => {
                if let Some(i) = stack.iter().rposition(|e| e == arg) {
                    stack.truncate(i);
                }
            }
            _ => {}
        }
    }
    stack
}

/// The suggestion list for a context. Prefix matches first, then
/// substring matches; built-ins first, then things from your document.
pub fn candidates(ctx: &Ctx, found: &Found, open_envs: &[String], project_dir: Option<&Path>) -> Vec<Candidate> {
    let prefix = ctx.prefix();
    let mut list: Vec<Candidate> = Vec::new();
    let mut add = |label: &str, hint: &str| {
        if !list.iter().any(|c| c.label == label) {
            list.push(Candidate { label: label.to_string(), hint: hint.to_string() });
        }
    };
    match ctx {
        Ctx::Command { .. } => {
            for (c, _, h) in COMMANDS {
                add(c, h);
            }
            for c in &found.commands {
                add(c, "Used in this document");
            }
        }
        Ctx::BeginEnv { .. } => {
            for (e, _, h) in ENVIRONMENTS {
                add(e, h);
            }
            for e in &found.environments {
                add(e, "Used in this document");
            }
        }
        Ctx::EndEnv { .. } => {
            for e in open_envs.iter().rev() {
                add(e, "Close the open environment");
            }
        }
        Ctx::Ref { .. } => {
            for l in &found.labels {
                add(l, "Label in this document");
            }
        }
        Ctx::Cite { .. } => {
            for k in &found.cite_keys {
                add(k, "Bibliography entry");
            }
        }
        Ctx::Package { .. } => {
            for (p, h) in PACKAGES {
                add(p, h);
            }
        }
        Ctx::Class { .. } => {
            for (c, h) in CLASSES {
                add(c, h);
            }
        }
        Ctx::Graphic { .. } => {
            if let Some(dir) = project_dir {
                for f in list_files(dir, &["png", "jpg", "jpeg", "pdf", "eps"], 2) {
                    add(&f, "Image in the document's folder");
                }
            }
        }
        Ctx::TexFile { .. } => {
            if let Some(dir) = project_dir {
                for f in list_files(dir, &["tex"], 2) {
                    add(f.trim_end_matches(".tex"), "TeX file in the document's folder");
                }
            }
        }
    }
    let lower = prefix.to_lowercase();
    let mut starts: Vec<Candidate> = Vec::new();
    let mut contains: Vec<Candidate> = Vec::new();
    for c in list {
        if c.label == prefix {
            continue; // already fully typed
        }
        if c.label.starts_with(prefix) {
            starts.push(c);
        } else if prefix.len() >= 3 && c.label.to_lowercase().contains(lower.trim_start_matches('\\')) {
            contains.push(c);
        }
    }
    starts.extend(contains);
    starts.truncate(80);
    starts
}

/// What to insert when a suggestion is accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    /// Replaces the partial text (and `consume_after` chars after the cursor).
    pub text: String,
    /// Cursor position within `text` (UTF-16 units).
    pub cursor: usize,
    /// Characters after the cursor to replace too (an auto-inserted `}`).
    pub consume_after: usize,
}

/// `after` is the rest of the current line after the cursor; `indent` is the
/// current line's leading whitespace.
pub fn expansion(ctx: &Ctx, chosen: &str, after: &str, indent: &str) -> Expansion {
    let closes = after.starts_with('}');
    let finish = |body: String| -> Expansion {
        let (text, cursor) = place_cursor(&body);
        Expansion { text, cursor, consume_after: 0 }
    };
    match ctx {
        Ctx::Command { .. } => {
            let snippet = COMMANDS.iter().find(|(c, _, _)| *c == chosen).map(|(_, s, _)| *s);
            match snippet {
                Some(s) => finish(s.to_string()),
                None => finish(format!("{chosen}¦")),
            }
        }
        Ctx::BeginEnv { .. } => {
            let body = ENVIRONMENTS.iter().find(|(e, _, _)| *e == chosen).map(|(_, b, _)| *b).unwrap_or("¦");
            let inner = format!("{indent}  ");
            let body_lines: Vec<String> = body.lines().map(|l| format!("{inner}{l}")).collect();
            let text = format!("{chosen}}}\n{}\n{indent}\\end{{{chosen}}}", body_lines.join("\n"));
            let (text, cursor) = place_cursor(&text);
            Expansion { text, cursor, consume_after: usize::from(closes) }
        }
        _ => {
            // Plain argument: insert it and step past the closing brace.
            let text = format!("{chosen}}}");
            let cursor = utf16_len(&text);
            Expansion { text, cursor, consume_after: usize::from(closes) }
        }
    }
}

fn place_cursor(s: &str) -> (String, usize) {
    match s.find('¦') {
        Some(i) => {
            let text = s.replacen('¦', "", 1);
            (text, utf16_len(&s[..i]))
        }
        None => (s.to_string(), utf16_len(s)),
    }
}

pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_commands() {
        assert_eq!(context("Some text \\sec"), Some(Ctx::Command { prefix: "\\sec".into() }));
        assert_eq!(context("\\"), None); // nothing typed yet
        assert_eq!(context("a\\\\sec"), None); // \\ is a line break, "sec" is text
        assert_eq!(context("50\\% \\al"), Some(Ctx::Command { prefix: "\\al".into() }));
        assert_eq!(context("% comment \\sec"), None);
        assert_eq!(context("plain words"), None);
    }

    #[test]
    fn detects_arguments() {
        assert_eq!(context("\\begin{ali"), Some(Ctx::BeginEnv { prefix: "ali".into() }));
        assert_eq!(context("  \\end{"), Some(Ctx::EndEnv { prefix: "".into() }));
        assert_eq!(context("see \\eqref{eq:"), Some(Ctx::Ref { prefix: "eq:".into() }));
        assert_eq!(context("\\cite{knuth84, lam"), Some(Ctx::Cite { prefix: "lam".into() }));
        assert_eq!(context("\\usepackage{amsmath,gra"), Some(Ctx::Package { prefix: "gra".into() }));
        assert_eq!(context("\\includegraphics[width=3cm]{fig"), Some(Ctx::Graphic { prefix: "fig".into() }));
        assert_eq!(context("\\textbf{bo"), None);
    }

    #[test]
    fn candidate_ranking() {
        let f = Found::default();
        let c = candidates(&Ctx::Command { prefix: "\\sub".into() }, &f, &[], None);
        assert_eq!(c[0].label, "\\subsection");
        assert!(c.iter().all(|x| x.label.starts_with("\\sub")));
        let c = candidates(&Ctx::BeginEnv { prefix: "".into() }, &f, &[], None);
        assert!(c.iter().any(|x| x.label == "itemize"));
    }

    #[test]
    fn finds_labels_keys_and_macros() {
        let doc = "\\newcommand{\\R}{\\mathbb{R}}\n\\section{A}\\label{sec:a}\n\\begin{equation}\\label{eq:1}\\end{equation}\n\\bibitem{knuth} \\myMacro";
        let f = scan_document(doc, None);
        assert_eq!(f.labels, vec!["sec:a", "eq:1"]);
        assert_eq!(f.cite_keys, vec!["knuth"]);
        assert!(f.commands.contains(&"\\R".to_string()));
        assert!(f.commands.contains(&"\\myMacro".to_string()));
        let c = candidates(&Ctx::Ref { prefix: "eq".into() }, &f, &[], None);
        assert_eq!(c[0].label, "eq:1");
    }

    #[test]
    fn bib_files() {
        let bib = "@article{knuth84,\n title={x}}\n@Book{lamport94, }\n@comment{nope}\n";
        assert_eq!(bib_keys(bib), vec!["knuth84", "lamport94"]);
    }

    #[test]
    fn end_suggests_innermost_open_env() {
        let before = "\\begin{document}\n\\begin{itemize}\n\\item a\n\\begin{enumerate}\n\\end{enumerate}\n";
        let open = open_environments(before);
        assert_eq!(open, vec!["document", "itemize"]);
        let c = candidates(&Ctx::EndEnv { prefix: "".into() }, &Found::default(), &open, None);
        assert_eq!(c[0].label, "itemize");
    }

    #[test]
    fn expansions() {
        let e = expansion(&Ctx::Command { prefix: "\\fr".into() }, "\\frac", "", "");
        assert_eq!(e, Expansion { text: "\\frac{}{}".into(), cursor: 6, consume_after: 0 });

        let e = expansion(&Ctx::BeginEnv { prefix: "it".into() }, "itemize", "}", "  ");
        assert_eq!(e.text, "itemize}\n    \\item \n  \\end{itemize}");
        assert_eq!(e.consume_after, 1);
        assert_eq!(&e.text.encode_utf16().collect::<Vec<_>>()[..e.cursor], &"itemize}\n    \\item ".encode_utf16().collect::<Vec<_>>()[..]);

        let e = expansion(&Ctx::Ref { prefix: "eq".into() }, "eq:1", "} more", "");
        assert_eq!(e, Expansion { text: "eq:1}".into(), cursor: 5, consume_after: 1 });
    }

    #[test]
    fn lists_project_files() {
        let d = std::env::temp_dir().join(format!("typeset-complete-{}", std::process::id()));
        std::fs::create_dir_all(d.join("figs")).unwrap();
        std::fs::write(d.join("figs/plot.png"), b"x").unwrap();
        std::fs::write(d.join("ch1.tex"), b"x").unwrap();
        std::fs::write(d.join("refs.bib"), "@misc{web1, }").unwrap();
        let f = scan_document("", Some(&d));
        assert_eq!(f.cite_keys, vec!["web1"]);
        let g = candidates(&Ctx::Graphic { prefix: "".into() }, &f, &[], Some(&d));
        assert_eq!(g[0].label, "figs/plot.png");
        let t = candidates(&Ctx::TexFile { prefix: "c".into() }, &f, &[], Some(&d));
        assert_eq!(t[0].label, "ch1");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
