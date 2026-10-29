//! Getting documents and notes into LaTeX (macOS side).
//!
//! AppKit already knows how to read Word (.docx/.doc), Pages/TextEdit RTF,
//! OpenDocument, web pages and Google Docs (HTML) into an NSAttributedString.
//! We walk that attributed string paragraph by paragraph, note the styling
//! (font size, bold/italic, links, lists, table cells, monospace) and hand it
//! to `convert` — which does the actual LaTeX work and is unit-tested.

use std::path::Path;
use std::ptr;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, AllocAnyThread, MainThreadOnly};
use objc2_app_kit::{
    NSAttributedStringAppKitDocumentFormats, NSAttributedStringDocumentFormats,
    NSCharacterEncodingDocumentOption, NSDocumentTypeDocumentOption, NSFont,
    NSFontDescriptorSymbolicTraits, NSHTMLTextDocumentType, NSLinkAttributeName,
    NSParagraphStyle, NSParagraphStyleAttributeName, NSPasteboard, NSPasteboardTypeHTML,
    NSPasteboardTypeRTF, NSPasteboardTypeRTFD, NSPasteboardTypeString,
    NSStrikethroughStyleAttributeName, NSSuperscriptAttributeName, NSTextTableBlock,
    NSUnderlineStyleAttributeName, NSFontAttributeName, NSResponder, NSText, NSTextInputClient,
    NSTextView, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSAttributedString, NSAttributedStringKey, NSDictionary, NSNumber,
    NSObject, NSRange, NSRect, NSString, NSURL,
};

/// Largest file we'll import.
const MAX_IMPORT: u64 = 50 * 1024 * 1024;
/// File types Import accepts. (Old binary .doc and .webarchive are left out:
/// older, riskier parsers; .webarchive can carry scripts and remote content.)
pub const IMPORT_TYPES: &[&str] = &["docx", "rtf", "rtfd", "odt", "html", "htm", "md", "markdown", "txt"];

use crate::convert::{self, Converted, RichPara, RichRun, Style, TableCell};

/// What to put in the document.
pub enum Pasted {
    /// Formatted LaTeX (plus packages it needs).
    Converted(Converted),
    /// The clipboard already holds LaTeX: paste it unchanged.
    Raw(String),
    Nothing,
}

/// Convert whatever is on the clipboard.
pub fn from_clipboard(as_document: bool) -> Pasted {
    let pb = NSPasteboard::generalPasteboard();
    let plain = pb.stringForType(unsafe { NSPasteboardTypeString }).map(|s| s.to_string());
    if let Some(p) = &plain {
        if convert::looks_like_latex(p) {
            return Pasted::Raw(p.clone());
        }
    }
    match (clipboard_rich(), plain) {
        (Some(a), _) => Pasted::Converted(from_attributed(&a, as_document)),
        (None, Some(p)) if !p.trim().is_empty() => Pasted::Converted(convert::from_plain(&p, as_document)),
        _ => Pasted::Nothing,
    }
}

/// Rich text on the clipboard: RTF (Word, Pages, TextEdit, Safari), then
/// HTML (Google Docs, Chrome) — HTML only after sanitising, so nothing in it
/// can load remote content or run.
pub fn clipboard_rich() -> Option<Retained<NSAttributedString>> {
    let pb = NSPasteboard::generalPasteboard();
    unsafe {
        if let Some(d) = pb.dataForType(NSPasteboardTypeRTF) {
            NSAttributedString::initWithRTF_documentAttributes(NSAttributedString::alloc(), &d, None)
        } else if let Some(d) = pb.dataForType(NSPasteboardTypeRTFD) {
            NSAttributedString::initWithRTFD_documentAttributes(NSAttributedString::alloc(), &d, None)
        } else if let Some(d) = pb.dataForType(NSPasteboardTypeHTML) {
            html_to_attributed(&String::from_utf8_lossy(&d.to_vec()))
        } else {
            None
        }
    }
}

/// Sanitise HTML, then let AppKit read it (as UTF-8, explicitly).
unsafe fn html_to_attributed(html: &str) -> Option<Retained<NSAttributedString>> {
    let clean = convert::sanitize_html(html);
    let data = objc2_foundation::NSData::with_bytes(clean.as_bytes());
    let data = &*data;
    let keys = unsafe { [NSDocumentTypeDocumentOption, NSCharacterEncodingDocumentOption] };
    let utf8 = NSNumber::new_usize(4); // NSUTF8StringEncoding
    let html: &AnyObject = unsafe { NSHTMLTextDocumentType };
    let values: [&AnyObject; 2] = [html, &utf8];
    let opts = NSDictionary::from_slices(&keys, &values);
    unsafe {
        NSAttributedString::initWithData_options_documentAttributes_error(NSAttributedString::alloc(), data, &opts, None).ok()
    }
}

/// Import a file: Word, RTF, OpenDocument, HTML, Markdown or plain text.
pub fn from_file(path: &Path) -> Result<Converted, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if !IMPORT_TYPES.contains(&ext.as_str()) {
        return Err(format!(".{ext} files can't be imported"));
    }
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_IMPORT {
        return Err("That file is too large to import (over 50 MB)".into());
    }
    match ext.as_str() {
        "html" | "htm" => {
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            let a = unsafe { html_to_attributed(&String::from_utf8_lossy(&bytes)) }.ok_or("Couldn't read that web page")?;
            Ok(from_attributed(&a, true))
        }
        "md" | "markdown" | "mdown" | "txt" | "text" => {
            let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            Ok(if ext == "txt" || ext == "text" {
                convert::from_plain(&text, true)
            } else {
                convert::from_markdown(&text, true)
            })
        }
        _ => {
            let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
            let opts = NSDictionary::new();
            let a = unsafe {
                NSAttributedString::initWithURL_options_documentAttributes_error(NSAttributedString::alloc(), &url, &opts, None)
            }
            .map_err(|e| e.localizedDescription().to_string())?;
            Ok(from_attributed(&a, true))
        }
    }
}

/// Convert an attributed string. Text with no real formatting (or code-font
/// text that is really Markdown) goes through the Markdown/plain converter.
pub fn from_attributed(a: &NSAttributedString, as_document: bool) -> Converted {
    let paras = rich_paras(a);
    let text = a.string().to_string();
    let all_mono = paras.iter().filter(|p| !p.runs.iter().all(|r| r.text.trim().is_empty())).all(|p| {
        p.runs.iter().filter(|r| !r.text.trim().is_empty()).all(|r| r.style.mono)
    });
    if convert::rich_is_plain(&paras) || (all_mono && looks_like_markdown(&text)) {
        if convert::looks_like_latex(&text) {
            return Converted { body: text, ..Default::default() };
        }
        return convert::from_plain(&text, as_document);
    }
    convert::from_rich(&paras, as_document)
}

fn looks_like_markdown(text: &str) -> bool {
    text.contains("**") || text.contains("](") || text.contains("```") || text.contains("|---")
        || text.lines().any(|l| {
            let t = l.trim_start();
            t.starts_with("# ") || t.starts_with("## ") || t.starts_with("- ") || t.starts_with("* ")
        })
}

fn attr(a: &NSAttributedString, key: &NSAttributedStringKey, at: usize) -> Option<Retained<AnyObject>> {
    unsafe { a.attribute_atIndex_effectiveRange(key, at, ptr::null_mut()) }
}

fn number(obj: Option<Retained<AnyObject>>) -> isize {
    obj.and_then(|o| o.downcast::<NSNumber>().ok()).map(|n| n.integerValue()).unwrap_or(0)
}

/// Split an attributed string into paragraphs of styled runs.
pub fn rich_paras(a: &NSAttributedString) -> Vec<RichPara> {
    let s = a.string();
    let len = s.length();
    let mut out = Vec::new();
    let mut loc = 0;
    while loc < len {
        let pr = s.paragraphRangeForRange(NSRange::new(loc, 0));
        if pr.length == 0 {
            break;
        }
        let end = pr.location + pr.length;
        let mut para = RichPara::default();

        // Lists and tables live in the paragraph style.
        if let Some(ps) = attr(a, unsafe { NSParagraphStyleAttributeName }, pr.location)
            .and_then(|o| o.downcast::<NSParagraphStyle>().ok())
        {
            let lists = ps.textLists();
            if lists.count() > 0 {
                let fmt = lists.lastObject().map(|l| l.markerFormat().to_string()).unwrap_or_default();
                let ordered = ["decimal", "alpha", "roman"].iter().any(|k| fmt.contains(k));
                para.list = Some((ordered, lists.count()));
            }
            for block in ps.textBlocks().iter() {
                if let Ok(cell) = block.downcast::<NSTextTableBlock>() {
                    let table = cell.table();
                    para.cell = Some(TableCell {
                        table: Retained::as_ptr(&table) as usize,
                        row: cell.startingRow().max(0) as usize,
                        col: cell.startingColumn().max(0) as usize,
                        columns: table.numberOfColumns().max(1),
                    });
                }
            }
        }

        // Styled runs.
        let mut i = pr.location;
        while i < end {
            let mut eff = NSRange::new(i, 1);
            let _ = unsafe { a.attributesAtIndex_effectiveRange(i, &mut eff) };
            let run_end = (eff.location + eff.length).clamp(i + 1, end);
            let text = s.substringWithRange(NSRange::new(i, run_end - i)).to_string();

            let mut style = Style::default();
            let mut size = 12.0;
            if let Some(font) = attr(a, unsafe { NSFontAttributeName }, i).and_then(|o| o.downcast::<NSFont>().ok()) {
                size = font.pointSize();
                let traits = font.fontDescriptor().symbolicTraits();
                let name = font.fontName().to_string().to_ascii_lowercase();
                style.bold = traits.contains(NSFontDescriptorSymbolicTraits::TraitBold)
                    || name.contains("bold") || name.contains("black") || name.contains("heavy");
                style.italic = traits.contains(NSFontDescriptorSymbolicTraits::TraitItalic)
                    || name.contains("italic") || name.contains("oblique");
                style.mono = traits.contains(NSFontDescriptorSymbolicTraits::TraitMonoSpace)
                    || ["courier", "menlo", "monaco", "mono", "consolas", "inconsolata"].iter().any(|m| name.contains(m));
            }
            style.underline = number(attr(a, unsafe { NSUnderlineStyleAttributeName }, i)) != 0;
            style.strike = number(attr(a, unsafe { NSStrikethroughStyleAttributeName }, i)) != 0;
            style.script = number(attr(a, unsafe { NSSuperscriptAttributeName }, i)).signum() as i8;
            if let Some(link) = attr(a, unsafe { NSLinkAttributeName }, i) {
                let url = match link.downcast::<NSURL>() {
                    Ok(u) => u.absoluteString().map(|s| s.to_string()),
                    Err(o) => o.downcast::<NSString>().ok().map(|s| s.to_string()),
                };
                style.link = url.filter(|u| !u.is_empty());
            }
            para.runs.push(RichRun { text, style, size });
            i = run_end;
        }

        // Drop the paragraph terminator and any list marker ("\t•\t", "\t1.\t").
        if let Some(last) = para.runs.last_mut() {
            while last.text.ends_with(['\n', '\r', '\u{2029}']) {
                last.text.pop();
            }
        }
        if para.list.is_some() {
            strip_marker(&mut para.runs);
        }
        out.push(para);
        loc = end;
    }
    out
}

/// Remove a leading "\t•\t" / "\t12.\t" list marker spread over runs.
fn strip_marker(runs: &mut [RichRun]) {
    let all: String = runs.iter().map(|r| r.text.as_str()).collect();
    let t = all.trim_start_matches('\t');
    let lead_tabs = all.len() - t.len();
    let Some(tab) = t.find('\t') else { return };
    if tab == 0 || t[..tab].chars().count() > 10 {
        return;
    }
    let mut remove = lead_tabs + tab + 1; // bytes
    for r in runs.iter_mut() {
        if remove == 0 {
            break;
        }
        let n = remove.min(r.text.len());
        r.text.drain(..n);
        remove -= n;
    }
}

/// The "paste here" box in Paste & Format. Pastes go through our sanitising
/// clipboard reader, and drag-and-drop only accepts RTF/plain text, so raw
/// HTML never reaches the system HTML reader unsanitised.
pub struct PasteSourceIvars;

define_class!(
    #[unsafe(super(NSTextView, NSText, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TypesetPasteSourceView"]
    #[ivars = PasteSourceIvars]
    pub struct PasteSourceView;

    impl PasteSourceView {
        #[unsafe(method(paste:))]
        fn paste(&self, _sender: Option<&AnyObject>) {
            let content: Option<Retained<AnyObject>> = match clipboard_rich() {
                Some(a) => Some(Retained::into_super(Retained::into_super(a))),
                None => NSPasteboard::generalPasteboard()
                    .stringForType(unsafe { NSPasteboardTypeString })
                    .map(|s| Retained::into_super(Retained::into_super(s))),
            };
            if let Some(c) = content {
                let range = self.selectedRange();
                unsafe { self.insertText_replacementRange(&c, range) };
            }
        }

        #[unsafe(method_id(readablePasteboardTypes))]
        fn readable_types(&self) -> Retained<NSArray<NSString>> {
            let types: [&NSString; 3] = unsafe { [NSPasteboardTypeRTF, NSPasteboardTypeRTFD, NSPasteboardTypeString] };
            NSArray::from_slice(&types)
        }
    }
);

impl PasteSourceView {
    pub fn new(mtm: MainThreadMarker) -> Retained<PasteSourceView> {
        let this = Self::alloc(mtm).set_ivars(PasteSourceIvars);
        unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_list_markers() {
        let mut runs = vec![
            RichRun { text: "\t•".into(), ..Default::default() },
            RichRun { text: "\tMilk".into(), ..Default::default() },
        ];
        strip_marker(&mut runs);
        assert_eq!(runs.iter().map(|r| r.text.as_str()).collect::<String>(), "Milk");
        let mut runs = vec![RichRun { text: "\t12.\tStep".into(), ..Default::default() }];
        strip_marker(&mut runs);
        assert_eq!(runs[0].text, "Step");
    }
}
