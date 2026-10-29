//! The LaTeX text view: an NSTextView subclass with Overleaf-style help.
//!
//! - Typing `\` + letters, `\begin{`, `\ref{`, `\cite{`, `\usepackage{`,
//!   `\includegraphics{`, `\input{` pops up suggestions under the cursor.
//!   ↑/↓ choose, Return or Tab insert, Esc closes (Esc again re-opens), click works too.
//! - Choosing a command inserts a template (`\frac{|}{}`); choosing an
//!   environment inserts the whole `\begin … \end` block.
//! - `{` auto-closes; typing `}` before an auto-inserted `}` steps over it.
//! - Finishing `\begin{name}` by hand adds the matching `\end{name}`.
//! - Line numbers in a gutter and a highlight on the cursor's line, drawn
//!   by the text view itself (so they scroll with the text, never lag).

use std::cell::{Cell, OnceCell, RefCell};
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{define_class, msg_send, sel, AllocAnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSBezierPath, NSBorderType, NSBox, NSButton,
    NSColor, NSEvent, NSFont, NSFontAttributeName, NSFontWeightRegular,
    NSForegroundColorAttributeName, NSLineBreakMode, NSPanel, NSScrollView, NSScrollerKnobStyle, NSStringDrawing, NSText,
    NSTextAlignment, NSTextInputClient, NSTextView, NSView, NSWindowOrderingMode, NSWindowStyleMask, NSResponder,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSDictionary, NSMutableAttributedString, NSNotFound,
    NSObject, NSPoint, NSRange, NSRect, NSSize, NSString,
};

use crate::complete::{self, Candidate, Ctx};
use crate::theme;

const ROWS: usize = 8;
const ROW_H: f64 = 24.0;
const PAD: f64 = 5.0;
const POPUP_W: f64 = 480.0;

/// Space between the gutter and the text, and to the right of the text.
const PAD_L: f64 = 14.0;
const PAD_R: f64 = 18.0;
const INSET_TOP: f64 = 14.0;
const NUMBER_SIZE: f64 = 11.0;

#[derive(Default)]
pub struct EditorIvars {
    project_dir: RefCell<Option<PathBuf>>,
    popup: OnceCell<Retained<NSPanel>>,
    frame_box: OnceCell<Retained<NSBox>>,
    highlight: OnceCell<Retained<NSBox>>,
    rows: RefCell<Vec<Retained<NSButton>>>,
    items: RefCell<Vec<Candidate>>,
    ctx: RefCell<Option<Ctx>>,
    /// UTF-16 range of the text being completed.
    partial: Cell<(usize, usize)>,
    selected: Cell<usize>,
    top: Cell<usize>,
    visible: Cell<bool>,
    /// Esc closed the list; don't reopen it until the context changes.
    dismissed_at: Cell<Option<usize>>,
    /// Thin accent bar on the selected suggestion.
    accent_bar: OnceCell<Retained<NSBox>>,
    /// UTF-16 offset where each line starts (None = recompute).
    line_starts: RefCell<Option<Vec<usize>>>,
    /// Width of the line-number gutter (grows with the number of digits).
    gutter: Cell<f64>,
    /// Where the current-line highlight was last drawn.
    line_rect: Cell<Option<NSRect>>,
}

define_class!(
    #[unsafe(super(NSTextView, NSText, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TypesetTexTextView"]
    #[ivars = EditorIvars]
    pub struct TexTextView;

    impl TexTextView {
        /// Every typed character arrives here.
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text(&self, string: &AnyObject, replacement: NSRange) {
            let typed = any_to_string(string);
            let sel = self.selectedRange();
            let simple = replacement.location == NSNotFound as usize && sel.length == 0;

            if simple && typed == "}" && self.char_after_cursor() == Some('}') {
                // Step over the brace we auto-inserted.
                self.setSelectedRange(NSRange::new(sel.location + 1, 0));
                self.after_close_brace();
                self.refresh_suggestions(true);
                return;
            }
            if simple && typed == "{" && !self.char_after_cursor().is_some_and(|c| c.is_alphanumeric() || c == '\\') {
                self.super_insert("{}", NSRange::new(NSNotFound as usize, 0));
                self.setSelectedRange(NSRange::new(sel.location + 1, 0));
                self.refresh_suggestions(true);
                return;
            }

            unsafe { msg_send![super(self), insertText: string, replacementRange: replacement] }

            if typed == "}" {
                self.after_close_brace();
            }
            // Only single keystrokes open the list (not pastes or our own inserts).
            self.refresh_suggestions(typed.chars().count() == 1);
        }

        /// Arrow keys, Return, Tab, Esc, Delete… arrive here as selectors.
        #[unsafe(method(doCommandBySelector:))]
        fn do_command(&self, selector: Sel) {
            let iv = self.ivars();
            if iv.visible.get() {
                if selector == sel!(moveDown:) {
                    self.move_selection(1);
                    return;
                }
                if selector == sel!(moveUp:) {
                    self.move_selection(-1);
                    return;
                }
                if selector == sel!(insertNewline:) || selector == sel!(insertTab:) {
                    self.accept(iv.selected.get());
                    return;
                }
                if selector == sel!(cancelOperation:) {
                    iv.dismissed_at.set(Some(iv.partial.get().0));
                    self.hide_popup();
                    return;
                }
                if selector == sel!(pageDown:) || selector == sel!(scrollPageDown:) {
                    self.move_selection(ROWS as isize);
                    return;
                }
                if selector == sel!(pageUp:) || selector == sel!(scrollPageUp:) {
                    self.move_selection(-(ROWS as isize));
                    return;
                }
            } else if selector == sel!(cancelOperation:) {
                // Esc with no list open: show suggestions for where the cursor is.
                iv.dismissed_at.set(None);
                if self.refresh_suggestions(true) {
                    return;
                }
            }

            let _: () = unsafe { msg_send![super(self), doCommandBySelector: selector] };

            if selector == sel!(deleteBackward:) {
                // Keep the list in step while backspacing, but don't open it.
                if iv.visible.get() {
                    self.refresh_suggestions(true);
                }
            } else if iv.visible.get() {
                self.hide_popup();
            }
        }

        /// Text starts to the right of the line-number gutter.
        #[unsafe(method(textContainerOrigin))]
        fn container_origin(&self) -> NSPoint {
            NSPoint::new(self.gutter_width() + PAD_L, INSET_TOP)
        }

        /// Background, then the current-line band and the line numbers
        /// (all behind the text).
        #[unsafe(method(drawViewBackgroundInRect:))]
        fn draw_background(&self, rect: NSRect) {
            let _: () = unsafe { msg_send![super(self), drawViewBackgroundInRect: rect] };
            self.draw_gutter(rect);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.hide_popup();
            let _: () = unsafe { msg_send![super(self), mouseDown: event] };
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            self.hide_popup();
            unsafe { msg_send![super(self), resignFirstResponder] }
        }

        /// A row in the list was clicked.
        #[unsafe(method(pickSuggestion:))]
        fn pick_suggestion(&self, sender: &NSButton) {
            let row = sender.tag().max(0) as usize;
            self.accept(self.ivars().top.get() + row);
        }
    }
);

impl TexTextView {
    /// A scroll view containing a configured LaTeX text view.
    pub fn new_scrollable(mtm: MainThreadMarker) -> (Retained<NSScrollView>, Retained<TexTextView>) {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(600.0, 800.0));
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        scroll.setHasVerticalScroller(true);
        scroll.setHasHorizontalScroller(false);
        scroll.setAutohidesScrollers(true);
        scroll.setBorderType(NSBorderType::NoBorder);
        scroll.setScrollerKnobStyle(NSScrollerKnobStyle::Light);
        scroll.setDrawsBackground(true);
        scroll.setBackgroundColor(&theme::editor_bg());
        let size = scroll.contentSize();

        let this = Self::alloc(mtm).set_ivars(EditorIvars::default());
        let tv: Retained<Self> = unsafe {
            msg_send![super(this), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), size)]
        };
        tv.setMinSize(NSSize::new(0.0, size.height));
        tv.setMaxSize(NSSize::new(f64::MAX, f64::MAX));
        tv.setVerticallyResizable(true);
        tv.setHorizontallyResizable(false);
        tv.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        if let Some(container) = unsafe { tv.textContainer() } {
            container.setContainerSize(NSSize::new(size.width, f64::MAX));
            container.setWidthTracksTextView(true);
        }
        scroll.setDocumentView(Some(&tv));
        tv.ivars().gutter.set(gutter_for_lines(1));
        tv.apply_gutter();
        (scroll, tv)
    }

    // ---- gutter & current line ---------------------------------------------------

    fn gutter_width(&self) -> f64 {
        let g = self.ivars().gutter.get();
        if g > 0.0 { g } else { gutter_for_lines(1) }
    }

    /// The container inset is symmetric, so split gutter + padding across both
    /// sides; textContainerOrigin then shifts the text right of the gutter.
    fn apply_gutter(&self) {
        let w = self.gutter_width() + PAD_L + PAD_R;
        self.setTextContainerInset(NSSize::new(w / 2.0, INSET_TOP));
    }

    /// Call after every change to the text (typing, undo, paste, a new document).
    pub fn text_changed(&self) {
        let iv = self.ivars();
        let starts = line_starts(&self.string().to_string());
        let width = gutter_for_lines(starts.len());
        *iv.line_starts.borrow_mut() = Some(starts);
        if (width - iv.gutter.get()).abs() > 0.5 {
            iv.gutter.set(width);
            self.apply_gutter();
            self.setNeedsDisplay(true);
        } else {
            let v = self.visibleRect();
            self.setNeedsDisplayInRect(NSRect::new(NSPoint::new(0.0, v.origin.y), NSSize::new(width + PAD_L, v.size.height)));
        }
        self.refresh_current_line();
    }

    /// Move the current-line highlight to the cursor (call when the selection changes).
    pub fn refresh_current_line(&self) {
        let iv = self.ivars();
        let new = self.current_line_rect();
        for r in [iv.line_rect.get(), new].into_iter().flatten() {
            self.setNeedsDisplayInRect(NSRect::new(
                NSPoint::new(r.origin.x, r.origin.y - 2.0),
                NSSize::new(r.size.width, r.size.height + 4.0),
            ));
        }
        iv.line_rect.set(new);
    }

    /// 1-based (line, column) of a UTF-16 offset.
    pub fn line_col(&self, loc: usize) -> (usize, usize) {
        self.ensure_line_starts();
        let starts = self.ivars().line_starts.borrow();
        let starts = starts.as_deref().unwrap_or(&[0]);
        let i = starts.partition_point(|&s| s <= loc).max(1) - 1;
        (i + 1, loc - starts[i] + 1)
    }

    fn ensure_line_starts(&self) {
        let iv = self.ivars();
        if iv.line_starts.borrow().is_none() {
            *iv.line_starts.borrow_mut() = Some(line_starts(&self.string().to_string()));
        }
    }

    /// The band behind the cursor's line (all of its wrapped rows), full width.
    fn current_line_rect(&self) -> Option<NSRect> {
        let sel = self.selectedRange();
        if sel.length != 0 {
            return None;
        }
        let lm = unsafe { self.layoutManager() }?;
        let text = self.string();
        let len = text.length();
        let loc = sel.location.min(len);
        let origin = self.textContainerOrigin();
        let at_empty_last_line = loc == len && (len == 0 || text.characterAtIndex(len - 1) == u16::from(b'\n'));
        let (top, bottom) = if at_empty_last_line {
            let r = lm.extraLineFragmentRect();
            if r.size.height <= 0.0 {
                return None;
            }
            (r.origin.y, r.origin.y + r.size.height)
        } else {
            let para = text.lineRangeForRange(NSRange::new(loc, 0));
            let glyphs = unsafe { lm.glyphRangeForCharacterRange_actualCharacterRange(para, std::ptr::null_mut()) };
            if glyphs.length == 0 {
                return None;
            }
            let first = unsafe { lm.lineFragmentRectForGlyphAtIndex_effectiveRange(glyphs.location, std::ptr::null_mut()) };
            let last = unsafe {
                lm.lineFragmentRectForGlyphAtIndex_effectiveRange(glyphs.location + glyphs.length - 1, std::ptr::null_mut())
            };
            (first.origin.y, last.origin.y + last.size.height)
        };
        Some(NSRect::new(
            NSPoint::new(0.0, (top + origin.y - theme::LINE_SPACING / 2.0).round()),
            NSSize::new(self.bounds().size.width, (bottom - top).round()),
        ))
    }

    fn draw_gutter(&self, rect: NSRect) {
        let (Some(lm), Some(tc)) = (unsafe { self.layoutManager() }, unsafe { self.textContainer() }) else { return };
        let origin = self.textContainerOrigin();
        let gutter = self.gutter_width();

        let current = self.current_line_rect();
        self.ivars().line_rect.set(current);
        if let Some(r) = current {
            theme::current_line().setFill();
            NSBezierPath::fillRect(r);
        }

        self.ensure_line_starts();
        let starts_ref = self.ivars().line_starts.borrow();
        let Some(starts) = starts_ref.as_deref() else { return };
        let text = self.string();
        let len = text.length();
        let cursor_line = starts.partition_point(|&s| s <= self.selectedRange().location.min(len)).max(1) - 1;

        let text_font = self.font().unwrap_or_else(|| theme::mono(theme::EDITOR_SIZE));
        let num_font = theme::mono(NUMBER_SIZE);
        let dy = text_font.ascender() - num_font.ascender();
        let attrs = |color: &NSColor| {
            let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
            let values: [&AnyObject; 2] = [&num_font, color];
            NSDictionary::from_slices(&keys, &values)
        };
        let normal = attrs(&theme::line_number());
        let bright = attrs(&theme::line_number_current());

        // Lines that intersect the area being drawn.
        let area = NSRect::new(
            NSPoint::new(0.0, (rect.origin.y - origin.y).max(0.0)),
            NSSize::new(tc.containerSize().width.min(1.0e7), rect.size.height),
        );
        let glyphs = lm.glyphRangeForBoundingRect_inTextContainer(area, &tc);
        let chars = unsafe { lm.characterRangeForGlyphRange_actualGlyphRange(glyphs, std::ptr::null_mut()) };
        let end = chars.location + chars.length;
        let n_glyphs = lm.numberOfGlyphs();
        let bottom = rect.origin.y + rect.size.height;
        let mut i = starts.partition_point(|&s| s <= chars.location).max(1) - 1;
        while i < starts.len() && starts[i] <= end {
            let s = starts[i];
            let frag = if s < len {
                let g = lm.glyphIndexForCharacterAtIndex(s);
                if g >= n_glyphs {
                    break;
                }
                unsafe { lm.lineFragmentRectForGlyphAtIndex_effectiveRange(g, std::ptr::null_mut()) }
            } else {
                lm.extraLineFragmentRect()
            };
            if frag.size.height > 0.0 {
                let y = frag.origin.y + origin.y + dy;
                if y > bottom {
                    break;
                }
                let number = NSString::from_str(&(i + 1).to_string());
                let a = if i == cursor_line { &bright } else { &normal };
                let w = unsafe { number.sizeWithAttributes(Some(a)) }.width;
                unsafe { number.drawAtPoint_withAttributes(NSPoint::new(gutter - 4.0 - w, y), Some(a)) };
            }
            i += 1;
        }
    }

    pub fn set_project_dir(&self, dir: Option<PathBuf>) {
        *self.ivars().project_dir.borrow_mut() = dir;
    }

    // ---- text helpers -------------------------------------------------------

    fn text(&self) -> Retained<NSString> {
        self.string()
    }

    fn super_insert(&self, s: &str, range: NSRange) {
        let ns = NSString::from_str(s);
        let obj: &AnyObject = &ns;
        let _: () = unsafe { msg_send![super(self), insertText: obj, replacementRange: range] };
    }

    fn char_after_cursor(&self) -> Option<char> {
        let text = self.text();
        let loc = self.selectedRange().location;
        if loc >= text.length() {
            return None;
        }
        let r = text.rangeOfComposedCharacterSequenceAtIndex(loc);
        text.substringWithRange(r).to_string().chars().next()
    }

    /// (line start, text of the line before the cursor, text of the line after it).
    fn line_around_cursor(&self) -> (usize, String, String) {
        let text = self.text();
        let loc = self.selectedRange().location.min(text.length());
        let line = text.lineRangeForRange(NSRange::new(loc, 0));
        let before = text.substringWithRange(NSRange::new(line.location, loc - line.location)).to_string();
        let mut end = line.location + line.length;
        while end > loc {
            let last = text.substringWithRange(NSRange::new(end - 1, 1)).to_string();
            if last == "\n" || last == "\r" {
                end -= 1;
            } else {
                break;
            }
        }
        let after = text.substringWithRange(NSRange::new(loc, end - loc)).to_string();
        (line.location, before, after)
    }

    // ---- suggestions -----------------------------------------------------------

    /// Recompute the list for the cursor position. `may_open` is false when
    /// we only want to update a list that's already showing. Returns true if
    /// a list is now visible.
    fn refresh_suggestions(&self, may_open: bool) -> bool {
        let iv = self.ivars();
        if self.selectedRange().length != 0 {
            self.hide_popup();
            return false;
        }
        let (_, before, _) = self.line_around_cursor();
        let Some(ctx) = complete::context(&before) else {
            self.hide_popup();
            return false;
        };
        let cursor = self.selectedRange().location;
        let plen = complete::utf16_len(ctx.prefix());
        let start = cursor - plen.min(cursor);
        if iv.dismissed_at.get() == Some(start) {
            return false;
        }
        iv.dismissed_at.set(None);
        if !may_open && !iv.visible.get() {
            return false;
        }

        let doc = self.text().to_string();
        let dir = iv.project_dir.borrow().clone();
        // Only read .bib files from disk when completing a citation.
        let scan_dir = if matches!(ctx, Ctx::Cite { .. }) { dir.as_deref() } else { None };
        let found = complete::scan_document(&doc, scan_dir);
        let doc_before: String = {
            let t = self.text();
            t.substringWithRange(NSRange::new(0, start)).to_string()
        };
        let open = complete::open_environments(&doc_before);
        let items = complete::candidates(&ctx, &found, &open, dir.as_deref());
        if items.is_empty() {
            self.hide_popup();
            return false;
        }
        *iv.items.borrow_mut() = items;
        *iv.ctx.borrow_mut() = Some(ctx);
        iv.partial.set((start, plen));
        iv.selected.set(0);
        iv.top.set(0);
        self.show_popup();
        true
    }

    fn move_selection(&self, delta: isize) {
        let iv = self.ivars();
        let n = iv.items.borrow().len();
        if n == 0 {
            return;
        }
        let s = (iv.selected.get() as isize + delta).clamp(0, n as isize - 1) as usize;
        iv.selected.set(s);
        if s < iv.top.get() {
            iv.top.set(s);
        } else if s >= iv.top.get() + ROWS {
            iv.top.set(s + 1 - ROWS);
        }
        self.layout_rows();
    }

    /// Insert the chosen suggestion.
    fn accept(&self, index: usize) {
        let iv = self.ivars();
        let chosen = iv.items.borrow().get(index).cloned();
        let ctx = iv.ctx.borrow().clone();
        self.hide_popup();
        let (Some(chosen), Some(ctx)) = (chosen, ctx) else { return };

        let (start, plen) = iv.partial.get();
        let (line_start, _, after) = self.line_around_cursor();
        let indent: String = {
            let t = self.text();
            let line = t.lineRangeForRange(NSRange::new(line_start, 0));
            t.substringWithRange(line).to_string().chars().take_while(|c| *c == ' ' || *c == '\t').collect()
        };
        let e = complete::expansion(&ctx, &chosen.label, &after, &indent);
        let cursor = self.selectedRange().location;
        let replace = NSRange::new(start, cursor.saturating_sub(start).max(plen) + e.consume_after);
        self.super_insert(&e.text, replace);
        self.setSelectedRange(NSRange::new(start + e.cursor, 0));
        self.scrollRangeToVisible(self.selectedRange());

        // After `\begin` / `\usepackage` etc., go straight on to the argument list.
        if matches!(ctx, Ctx::Command { .. }) {
            self.refresh_suggestions(true);
        }
    }

    /// The user closed `\begin{name}` by hand: add `\end{name}` if it's missing.
    fn after_close_brace(&self) {
        let (_, before, after) = self.line_around_cursor();
        if !after.trim().is_empty() {
            return;
        }
        let Some(open) = before.rfind("\\begin{") else { return };
        let name = &before[open + 7..before.len().saturating_sub(1)];
        if !before.ends_with('}') || name.is_empty() || name.contains(['{', '}', '\\']) {
            return;
        }
        let doc = self.text().to_string();
        let begins = doc.matches(&format!("\\begin{{{name}}}")).count();
        let ends = doc.matches(&format!("\\end{{{name}}}")).count();
        if begins <= ends {
            return;
        }
        let indent: String = before.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let body = if name == "itemize" || name == "enumerate" { "\\item " } else { "" };
        let insert = format!("\n{indent}  {body}\n{indent}\\end{{{name}}}");
        let cursor = self.selectedRange().location;
        self.super_insert(&insert, NSRange::new(cursor, 0));
        let land = cursor + complete::utf16_len(&format!("\n{indent}  {body}"));
        self.setSelectedRange(NSRange::new(land, 0));
    }

    // ---- popup window ------------------------------------------------------------

    fn ensure_popup(&self) -> Option<Retained<NSPanel>> {
        let iv = self.ivars();
        if let Some(p) = iv.popup.get() {
            return Some(p.clone());
        }
        let mtm = self.mtm();
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(POPUP_W, ROWS as f64 * ROW_H + 2.0 * PAD));
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setHasShadow(true);
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));

        let frame_box = theme::surface(mtm, frame, &theme::raised(), Some(&theme::raised_border()), 10.0);
        panel.setContentView(Some(&frame_box));

        let highlight = theme::surface(mtm, NSRect::ZERO, &theme::accent_alpha(0.13), None, 6.0);
        let content: Retained<NSView> = frame_box.contentView().unwrap_or_else(|| Retained::into_super(frame_box.clone()));
        content.addSubview(&highlight);
        let bar = theme::surface(mtm, NSRect::ZERO, &theme::accent(), None, 1.0);
        content.addSubview(&bar);
        let _ = iv.accent_bar.set(bar);

        let target: &AnyObject = self;
        let mut rows = Vec::new();
        for i in 0..ROWS {
            let b = unsafe {
                NSButton::buttonWithTitle_target_action(&NSString::from_str(""), Some(target), Some(sel!(pickSuggestion:)), mtm)
            };
            b.setBordered(false);
            b.setAlignment(NSTextAlignment::Left);
            b.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            b.setTag(i as isize);
            content.addSubview(&b);
            rows.push(b);
        }
        *iv.rows.borrow_mut() = rows;
        let _ = iv.frame_box.set(frame_box);
        let _ = iv.highlight.set(highlight);
        let _ = iv.popup.set(panel.clone());
        Some(panel)
    }

    fn show_popup(&self) {
        let Some(panel) = self.ensure_popup() else { return };
        let Some(window) = self.window() else { return };
        let iv = self.ivars();
        let n = iv.items.borrow().len().min(ROWS);
        let h = n as f64 * ROW_H + 2.0 * PAD;

        // Anchor under the start of the text being completed.
        let (start, _) = iv.partial.get();
        let caret = unsafe { self.firstRectForCharacterRange_actualRange(NSRange::new(start, 0), std::ptr::null_mut()) };
        let mut origin = NSPoint::new(caret.origin.x - 8.0, caret.origin.y - h - 4.0);
        if let Some(screen) = window.screen() {
            let vis = screen.visibleFrame();
            if origin.y < vis.origin.y {
                origin.y = caret.origin.y + caret.size.height + 4.0; // flip above the line
            }
            let max_x = vis.origin.x + vis.size.width - POPUP_W;
            origin.x = origin.x.min(max_x).max(vis.origin.x);
        }
        panel.setFrame_display(NSRect::new(origin, NSSize::new(POPUP_W, h)), false);
        if let Some(fb) = iv.frame_box.get() {
            fb.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(POPUP_W, h)));
        }
        self.layout_rows();
        if !iv.visible.get() {
            unsafe { window.addChildWindow_ordered(&panel, NSWindowOrderingMode::Above) };
            panel.orderFront(None);
            iv.visible.set(true);
        }
    }

    fn hide_popup(&self) {
        let iv = self.ivars();
        if !iv.visible.get() {
            return;
        }
        iv.visible.set(false);
        if let Some(panel) = iv.popup.get() {
            if let Some(w) = self.window() {
                w.removeChildWindow(panel);
            }
            panel.orderOut(None);
        }
    }

    fn layout_rows(&self) {
        let iv = self.ivars();
        let items = iv.items.borrow();
        let rows = iv.rows.borrow();
        let n = items.len().min(ROWS);
        let h = n as f64 * ROW_H + 2.0 * PAD;
        let top = iv.top.get();
        let selected = iv.selected.get();
        for (i, row) in rows.iter().enumerate() {
            let idx = top + i;
            if i >= n || idx >= items.len() {
                row.setHidden(true);
                continue;
            }
            row.setHidden(false);
            let y = h - PAD - (i as f64 + 1.0) * ROW_H;
            row.setFrame(NSRect::new(NSPoint::new(PAD + 6.0, y), NSSize::new(POPUP_W - 2.0 * PAD - 12.0, ROW_H)));
            row.setAttributedTitle(&row_title(&items[idx], idx == selected));
            if idx == selected {
                if let Some(hl) = iv.highlight.get() {
                    hl.setFrame(NSRect::new(NSPoint::new(PAD, y), NSSize::new(POPUP_W - 2.0 * PAD, ROW_H)));
                }
                if let Some(bar) = iv.accent_bar.get() {
                    bar.setFrame(NSRect::new(NSPoint::new(PAD + 3.0, y + 6.0), NSSize::new(2.0, ROW_H - 12.0)));
                }
            }
        }
    }
}

/// "\section   Numbered section heading" — label in monospace, hint in grey.
fn row_title(c: &Candidate, selected: bool) -> Retained<NSAttributedString> {
    let label_color = if selected { theme::accent() } else { theme::text() };
    let hint_color = if selected { theme::text() } else { theme::text_dim() };
    let label_font = theme::mono_semibold(12.0);
    let hint_font = NSFont::systemFontOfSize_weight(11.5, unsafe { NSFontWeightRegular });
    let out = NSMutableAttributedString::new();
    out.appendAttributedString(&attributed(&c.label, &label_font, &label_color));
    if !c.hint.is_empty() {
        out.appendAttributedString(&attributed(&format!("    {}", c.hint), &hint_font, &hint_color));
    }
    Retained::into_super(out)
}

fn attributed(s: &str, font: &NSFont, color: &NSColor) -> Retained<NSAttributedString> {
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let values: [&AnyObject; 2] = [font, color];
    let attrs = NSDictionary::from_slices(&keys, &values);
    unsafe { NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &NSString::from_str(s), Some(&attrs)) }
}

/// insertText: hands us an NSString or an NSAttributedString.
fn any_to_string(obj: &AnyObject) -> String {
    if let Some(s) = obj.downcast_ref::<NSString>() {
        return s.to_string();
    }
    if let Some(a) = obj.downcast_ref::<NSAttributedString>() {
        return a.string().to_string();
    }
    String::new()
}

/// UTF-16 offset of the start of every line (always starts with 0; ends with
/// the text's length when the text ends in a newline).
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    let mut at = 0;
    for ch in text.chars() {
        at += ch.len_utf16();
        if ch == '\n' {
            starts.push(at);
        }
    }
    starts
}

/// Gutter width for this many lines (at least three digits wide).
fn gutter_for_lines(lines: usize) -> f64 {
    let digits = lines.to_string().len().max(3) as f64;
    (14.0 + digits * 6.8).round()
}
