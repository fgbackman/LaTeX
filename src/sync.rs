//! SyncTeX: map between a spot in the PDF and a line in the source.
//!
//! Tectonic (with `--synctex`) writes `texput.synctex.gz`: a record for every
//! box, glue, kern and math item TeX placed, tagged with the source file and
//! line it came from and its position on the page. Positions are in TeX
//! scaled points from the page's top-left; we convert them to PDF points
//! (1/72 in) measured from the top.

use std::io::Read;

/// Scaled points per PDF point (65536 sp/pt × 72.27 pt/in ÷ 72 bp/in).
const SP_PER_BP: f64 = 65781.76;

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rec {
    page: u32,
    kind: u8,
    tag: u32,
    line: u32,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    d: f64,
}

/// A rectangle on a PDF page, in points from the page's top-left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spot {
    /// 1-based page number.
    pub page: u32,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Default)]
pub struct SyncIndex {
    main_tag: u32,
    recs: Vec<Rec>,
}

impl SyncIndex {
    /// Decompress and parse. The engine wrote this file, so it is untrusted:
    /// decompression is capped to defuse "zip bomb" files.
    pub fn from_gz(bytes: &[u8]) -> Option<SyncIndex> {
        const MAX_TEXT: u64 = 256 * 1024 * 1024;
        let mut raw = Vec::new();
        flate2::read::GzDecoder::new(bytes).take(MAX_TEXT + 1).read_to_end(&mut raw).ok()?;
        if raw.len() as u64 > MAX_TEXT {
            return None;
        }
        Some(Self::parse(&String::from_utf8_lossy(&raw)))
    }

    pub fn parse(text: &str) -> SyncIndex {
        let mut idx = SyncIndex { main_tag: 0, recs: Vec::new() };
        let mut unit = 1.0_f64;
        let mut mag = 1.0_f64;
        let (mut xoff, mut yoff) = (0.0_f64, 0.0_f64);
        let mut page = 0u32;
        let mut in_content = false;

        for line in text.lines() {
            if !in_content {
                if let Some(rest) = line.strip_prefix("Input:") {
                    let mut p = rest.splitn(2, ':');
                    if let (Some(tag), Some(path)) = (p.next(), p.next()) {
                        if idx.main_tag == 0 && (path == "texput.tex" || path.ends_with("/texput.tex")) {
                            idx.main_tag = tag.parse().unwrap_or(0);
                        }
                    }
                } else if let Some(v) = line.strip_prefix("Unit:") {
                    unit = v.trim().parse().unwrap_or(1.0);
                } else if let Some(v) = line.strip_prefix("Magnification:") {
                    mag = v.trim().parse::<f64>().map(|m| m / 1000.0).unwrap_or(1.0);
                } else if let Some(v) = line.strip_prefix("X Offset:") {
                    xoff = v.trim().parse().unwrap_or(0.0);
                } else if let Some(v) = line.strip_prefix("Y Offset:") {
                    yoff = v.trim().parse().unwrap_or(0.0);
                } else if line.starts_with("Content:") {
                    in_content = true;
                }
                continue;
            }
            let Some(&kind) = line.as_bytes().first() else { continue };
            if kind == b'{' {
                page = line[1..].trim().parse().unwrap_or(page + 1);
                continue;
            }
            if !b"[(hvgk$x".contains(&kind) {
                continue;
            }
            // <kind><tag>,<line>[,<col>]:<x>,<y>[:<W>[,<H>,<D>]]
            let body = &line[1..];
            let mut parts = body.split(':');
            let (Some(tl), Some(xy)) = (parts.next(), parts.next()) else { continue };
            let mut tl = tl.split(',');
            let (Some(tag), Some(ln)) = (tl.next(), tl.next()) else { continue };
            let (Ok(tag), Ok(ln)) = (tag.parse::<u32>(), ln.parse::<i64>()) else { continue };
            let mut xy = xy.split(',');
            let (Some(x), Some(y)) = (xy.next(), xy.next()) else { continue };
            let (Ok(x), Ok(y)) = (x.parse::<f64>(), y.parse::<f64>()) else { continue };
            let mut dims = [0.0_f64; 3];
            if let Some(whd) = parts.next() {
                for (i, v) in whd.split(',').take(3).enumerate() {
                    dims[i] = v.parse().unwrap_or(0.0);
                }
            }
            let k = unit * mag / SP_PER_BP;
            idx.recs.push(Rec {
                page,
                kind,
                tag,
                line: ln.max(0) as u32,
                x: (x + xoff) * k,
                y: (y + yoff) * k,
                w: dims[0] * k,
                h: dims[1] * k,
                d: dims[2] * k,
            });
        }
        if idx.main_tag == 0 {
            idx.main_tag = 1;
        }
        idx
    }

    fn main(&self) -> impl Iterator<Item = &Rec> {
        let tag = self.main_tag;
        self.recs.iter().filter(move |r| r.tag == tag && r.line > 0)
    }

    /// PDF → source: the source line for a point (page is 1-based; x, y in
    /// points from the top-left of the page).
    pub fn line_at(&self, page: u32, x: f64, y: f64) -> Option<u32> {
        let on_page = || self.main().filter(move |r| r.page == page);
        let is_leaf = |r: &Rec| matches!(r.kind, b'g' | b'k' | b'$' | b'x' | b'h');

        // 1. The innermost line box (hbox) containing the point…
        let boxed = on_page()
            .filter(|r| r.kind == b'(' && r.w > 0.0)
            .filter(|r| x >= r.x - 1.0 && x <= r.x + r.w + 1.0 && y >= r.y - r.h - 2.0 && y <= r.y + r.d + 2.0)
            .min_by(|a, b| (a.w * (a.h + a.d)).total_cmp(&(b.w * (b.h + b.d))));
        if let Some(b) = boxed {
            // …then the nearest item on that line, which knows its own line number.
            let near = on_page()
                .filter(|r| is_leaf(r) && (r.y - b.y).abs() < 0.5 && r.x >= b.x - 1.0 && r.x <= b.x + b.w + 1.0)
                .min_by(|p, q| (p.x - x).abs().total_cmp(&(q.x - x).abs()));
            return Some(near.map(|r| r.line).unwrap_or(b.line));
        }
        // 2. Otherwise whatever is closest (vertical distance counts double).
        on_page()
            .filter(|r| is_leaf(r) || r.kind == b'(')
            .min_by(|p, q| {
                let dp = (p.x - x).powi(2) + (2.0 * (p.y - y)).powi(2);
                let dq = (q.x - x).powi(2) + (2.0 * (q.y - y)).powi(2);
                dp.total_cmp(&dq)
            })
            .map(|r| r.line)
    }

    /// Source → PDF: where `line` ended up (the first place, if several).
    /// Lines with no output (blank lines, comments) use the next line that has some.
    pub fn spot_for_line(&self, line: u32) -> Option<Spot> {
        let target = self
            .main()
            .map(|r| r.line)
            .filter(|&l| l >= line)
            .min()
            .or_else(|| self.main().map(|r| r.line).max())?;
        let first = self.main().find(|r| r.line == target)?;

        // Highlight the whole printed line: the hbox whose baseline this item sits on.
        let line_box = self
            .main()
            .filter(|r| r.page == first.page && r.kind == b'(' && r.w > 0.0)
            .filter(|r| (r.y - first.y).abs() < 0.5 && first.x >= r.x - 1.0 && first.x <= r.x + r.w + 1.0)
            .max_by(|a, b| a.w.total_cmp(&b.w))
            .or_else(|| self.recs.iter().find(|r| {
                // fall back to any box on that baseline (e.g. from the class file)
                r.page == first.page && r.kind == b'(' && r.w > 0.0 && (r.y - first.y).abs() < 0.5
                    && first.x >= r.x - 1.0 && first.x <= r.x + r.w + 1.0
            }));
        Some(match line_box {
            Some(b) => Spot { page: b.page, x: b.x, y: b.y - b.h.max(6.0), w: b.w, h: b.h.max(6.0) + b.d },
            None if first.kind == b'(' || first.kind == b'[' => {
                Spot { page: first.page, x: first.x, y: first.y - first.h, w: first.w.max(20.0), h: (first.h + first.d).max(10.0) }
            }
            None => Spot { page: first.page, x: first.x - 2.0, y: first.y - 10.0, w: 60.0, h: 13.0 },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Generated by `xelatex -synctex=1` (the engine Tectonic is built on) from:
    //  6 \maketitle
    //  7 Type on the left. The preview on the right updates…
    //  9 Euler's identity: $e^{i\pi} + 1 = 0$.
    // 11   \int_{-\infty}^{\infty} e^{-x^2}\,dx = \sqrt{\pi}
    // 13 Second paragraph with more words…
    // 16 Page two text here.
    // Expected answers come from the official `synctex` command-line tool.
    const FIXTURE: &[u8] = include_bytes!("testdata/texput.synctex.gz");

    fn idx() -> SyncIndex {
        let mut i = SyncIndex::from_gz(FIXTURE).expect("parse");
        i.main_tag = 1; // fixture was made from a file named texput.tex in another folder
        i
    }

    #[test]
    fn rejects_decompression_bombs_and_garbage() {
        use std::io::Write;
        // 300 MB of zeros compresses to ~300 KB.
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        let chunk = vec![b'0'; 1 << 20];
        for _ in 0..300 {
            enc.write_all(&chunk).unwrap();
        }
        let bomb = enc.finish().unwrap();
        assert!(bomb.len() < 2_000_000);
        assert!(SyncIndex::from_gz(&bomb).is_none());
        assert!(SyncIndex::from_gz(b"not gzip at all").is_none());
        // Hostile-but-valid text must not panic.
        let junk = "Content:\n{\n{99999999999999999999\n(1,2:x,y\n[\n$,:,\nh1,-5:1e308,NaN:inf,-inf,1\n\u{1F600}\n";
        let idx = SyncIndex::parse(junk);
        let _ = idx.line_at(1, 0.0, 0.0);
        let _ = idx.spot_for_line(1);
    }

    #[test]
    fn units_are_pdf_points() {
        let i = idx();
        let first = i.recs.first().unwrap();
        assert!((first.x - 72.0).abs() < 0.01, "1in left offset → 72pt, got {}", first.x);
    }

    #[test]
    fn click_to_source_matches_synctex_tool() {
        let i = idx();
        // (page, x, y) → line, each checked with `synctex edit`
        for (p, x, y, want) in [
            (1, 300.0, 265.0, 7),
            (1, 150.0, 308.0, 8),
            (1, 230.0, 331.0, 9),
            (1, 200.0, 345.0, 9),
            (1, 310.0, 360.0, 11),
            (1, 400.0, 360.0, 12),
            (1, 250.0, 395.0, 13),
            (2, 150.0, 130.0, 16),
        ] {
            assert_eq!(i.line_at(p, x, y), Some(want), "click at page {p} ({x}, {y})");
        }
    }

    #[test]
    fn source_to_pdf_matches_synctex_tool() {
        let i = idx();
        // synctex view: line 7 → page 1, h 133.77, v 267.97, W 343.71
        let s = i.spot_for_line(7).unwrap();
        assert_eq!(s.page, 1);
        assert!((s.x - 133.77).abs() < 0.5, "{s:?}");
        assert!(s.y < 267.97 && s.y + s.h > 267.97, "baseline inside highlight: {s:?}");
        assert!((s.w - 343.71).abs() < 0.5, "{s:?}");
        // line 9 → page 1, baseline ≈ 334.7
        let s = i.spot_for_line(9).unwrap();
        assert!(s.y < 334.71 && s.y + s.h > 334.71, "{s:?}");
        // line 16 → page 2
        assert_eq!(i.spot_for_line(16).unwrap().page, 2);
        // blank line 14 → next line with output (15/16, on page 1 or 2)
        assert!(i.spot_for_line(14).is_some());
    }
}
