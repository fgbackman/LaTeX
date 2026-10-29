//! LaTeX syntax scanner.
//!
//! Pure Rust with no platform dependencies, so it is unit-tested on any OS.
//! Offsets are in UTF-16 code units because that is what `NSString` ranges use.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `\section`, `\alpha`, `\\`, `\%`
    Command,
    /// `% to end of line`
    Comment,
    /// `$...$`, `$$...$$`, `\(...\)`, `\[...\]`
    Math,
    /// The name inside `\begin{...}` / `\end{...}`
    Env,
    /// `{` and `}`
    Brace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub len: usize,
    pub kind: Kind,
}

/// Scan `text` and return coloured spans. Math spans come first so that
/// command spans inside math are painted on top of them.
pub fn scan(text: &str) -> Vec<Span> {
    // (utf16 offset, char) for every char, plus a sentinel end offset.
    let chars: Vec<(usize, char)> = {
        let mut v = Vec::with_capacity(text.len());
        let mut pos = 0usize;
        for c in text.chars() {
            v.push((pos, c));
            pos += c.len_utf16();
        }
        v.push((pos, '\0'));
        v
    };
    let n = chars.len() - 1; // index of sentinel
    let at = |i: usize| chars[i.min(n)].1;
    let off = |i: usize| chars[i.min(n)].0;

    let mut math: Vec<Span> = Vec::new();
    let mut rest: Vec<Span> = Vec::new();

    // Open math region: (start char index, closing delimiter)
    let mut open: Option<(usize, &'static str)> = None;

    let close_math = |math: &mut Vec<Span>, start: usize, end_excl: usize| {
        let (s, e) = (off(start), off(end_excl));
        if e > s {
            math.push(Span { start: s, len: e - s, kind: Kind::Math });
        }
    };

    let mut i = 0;
    while i < n {
        let c = at(i);
        match c {
            '\n' => {
                // Inline math never survives a line break in our line-local model.
                if let Some((s, _)) = open.take() {
                    close_math(&mut math, s, i);
                }
                i += 1;
            }
            '%' => {
                let start = i;
                while i < n && at(i) != '\n' {
                    i += 1;
                }
                rest.push(Span { start: off(start), len: off(i) - off(start), kind: Kind::Comment });
            }
            '\\' => {
                let start = i;
                let next = at(i + 1);
                if i + 1 >= n {
                    i += 1;
                    continue;
                }
                if next.is_ascii_alphabetic() || next == '@' {
                    let mut j = i + 1;
                    while j < n && (at(j).is_ascii_alphabetic() || at(j) == '@') {
                        j += 1;
                    }
                    let name: String = chars[i + 1..j].iter().map(|&(_, c)| c).collect();
                    rest.push(Span { start: off(start), len: off(j) - off(start), kind: Kind::Command });
                    i = j;
                    if (name == "begin" || name == "end") && at(i) == '{' {
                        let mut k = i + 1;
                        while k < n && at(k) != '}' && at(k) != '\n' {
                            k += 1;
                        }
                        if at(k) == '}' {
                            rest.push(Span { start: off(i), len: 1, kind: Kind::Brace });
                            if k > i + 1 {
                                rest.push(Span { start: off(i + 1), len: off(k) - off(i + 1), kind: Kind::Env });
                            }
                            rest.push(Span { start: off(k), len: 1, kind: Kind::Brace });
                            i = k + 1;
                        }
                    }
                } else {
                    // Control symbol: \\ \% \$ \{ \( \[ \) \] ...
                    match (next, open) {
                        ('(', None) => open = Some((i, "\\)")),
                        ('[', None) => open = Some((i, "\\]")),
                        (')', Some((s, "\\)"))) | (']', Some((s, "\\]"))) => {
                            close_math(&mut math, s, i + 2);
                            open = None;
                        }
                        _ => {}
                    }
                    rest.push(Span { start: off(start), len: off(i + 2) - off(start), kind: Kind::Command });
                    i += 2;
                }
            }
            '$' => {
                let double = at(i + 1) == '$';
                let w = if double { 2 } else { 1 };
                match open {
                    None => open = Some((i, if double { "$$" } else { "$" })),
                    Some((s, d)) if (d == "$$") == double => {
                        close_math(&mut math, s, i + w);
                        open = None;
                    }
                    Some((s, "$$")) => {
                        // Single $ inside $$...$$ (malformed) – close anyway.
                        close_math(&mut math, s, i + 1);
                        open = None;
                    }
                    _ => {}
                }
                i += w;
            }
            '{' | '}' => {
                rest.push(Span { start: off(i), len: 1, kind: Kind::Brace });
                i += 1;
            }
            _ => i += 1,
        }
    }
    if let Some((s, _)) = open {
        close_math(&mut math, s, n);
    }

    math.extend(rest);
    math
}

/// UTF-16 (start, length) of 1-based `line` in `text`, excluding the newline.
/// Lines past the end clamp to the last line.
pub fn line_range_utf16(text: &str, line: usize) -> (usize, usize) {
    let mut start = 0usize;
    let mut current = 1usize;
    let mut pos = 0usize;
    for c in text.chars() {
        if c == '\n' {
            if current == line {
                return (start, pos - start);
            }
            current += 1;
            start = pos + 1;
        }
        pos += c.len_utf16();
    }
    (start, pos - start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_ranges() {
        let t = "ab\ncdé\n\nlast";
        assert_eq!(line_range_utf16(t, 1), (0, 2));
        assert_eq!(line_range_utf16(t, 2), (3, 3));
        assert_eq!(line_range_utf16(t, 3), (7, 0));
        assert_eq!(line_range_utf16(t, 4), (8, 4));
        assert_eq!(line_range_utf16(t, 99), (8, 4));
    }

    fn kinds(t: &str) -> Vec<(String, Kind)> {
        let u: Vec<u16> = t.encode_utf16().collect();
        scan(t)
            .into_iter()
            .map(|s| (String::from_utf16_lossy(&u[s.start..s.start + s.len]), s.kind))
            .collect()
    }

    #[test]
    fn commands_and_comments() {
        let k = kinds("\\section{Hi} % note \\foo");
        assert!(k.contains(&("\\section".into(), Kind::Command)));
        assert!(k.contains(&("% note \\foo".into(), Kind::Comment)));
        assert!(!k.contains(&("\\foo".into(), Kind::Command)));
    }

    #[test]
    fn escaped_percent_is_not_comment() {
        let k = kinds("50\\% off");
        assert_eq!(k, vec![("\\%".into(), Kind::Command)]);
    }

    #[test]
    fn inline_and_display_math() {
        let k = kinds("a $x^2$ b $$\\alpha$$ c \\(y\\) d \\[z\\]");
        assert!(k.contains(&("$x^2$".into(), Kind::Math)));
        assert!(k.contains(&("$$\\alpha$$".into(), Kind::Math)));
        assert!(k.contains(&("\\(y\\)".into(), Kind::Math)));
        assert!(k.contains(&("\\[z\\]".into(), Kind::Math)));
        assert!(k.contains(&("\\alpha".into(), Kind::Command)));
        // math spans must precede command spans
        let first_cmd = k.iter().position(|x| x.1 != Kind::Math).unwrap();
        assert!(k[first_cmd..].iter().all(|x| x.1 != Kind::Math));
    }

    #[test]
    fn environments() {
        let k = kinds("\\begin{equation*}");
        assert!(k.contains(&("equation*".into(), Kind::Env)));
        assert!(k.contains(&("\\begin".into(), Kind::Command)));
    }

    #[test]
    fn utf16_offsets() {
        // "é" is 1 UTF-16 unit, "𝔸" is 2.
        let t = "é𝔸 \\x";
        let s = scan(t);
        assert_eq!(s, vec![Span { start: 4, len: 2, kind: Kind::Command }]);
    }

    #[test]
    fn unclosed_math_stops_at_newline() {
        let k = kinds("$a\nb");
        assert_eq!(k, vec![("$a".into(), Kind::Math)]);
    }
}
