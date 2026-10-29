//! The macOS UI: AppKit editor on the left, PDFKit live preview on the right.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, AllocAnyThread, DefinedClass, MainThreadOnly, Message};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSBackgroundColorAttributeName,
    NSBox, NSMutableParagraphStyle, NSParagraphStyleAttributeName, NSScrollerKnobStyle, NSWindowTitleVisibility,
    NSAlert, NSAlertFirstButtonReturn, NSControlStateValueOff, NSControlStateValueOn, NSAlertSecondButtonReturn, NSApplication,
    NSApplicationActivationPolicy, NSApplicationDelegate, NSApplicationTerminateReply,
    NSAutoresizingMaskOptions, NSBackingStoreType, NSButton, NSColor, NSFont,
    NSForegroundColorAttributeName, NSLineBreakMode, NSMenu, NSMenuItem, NSModalResponseOK,
    NSOpenPanel, NSSavePanel, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
    NSBaselineOffsetAttributeName, NSFontAttributeName, NSFontDescriptorSystemDesignSerif, NSSplitViewDividerStyle, NSTextAlignment,
    NSTextDelegate, NSTextStorage, NSTextStorageDelegate, NSTextStorageEditActions, NSTextView,
    NSEventModifierFlags, NSScrollView, NSTextField, NSTextViewDelegate, NSView, NSWindow, NSWindowDelegate,
    NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSAttributedString, NSMutableAttributedString, NSDictionary, NSNumber, NSData, NSInteger, NSNotification, NSObject, NSObjectProtocol,
    NSPoint, NSRange, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSTimer, NSURL,
};
use objc2_pdf_kit::PDFDocument;
use objc2_uniform_type_identifiers::UTType;

use crate::compile::{self, Done, Job, Outcome, TexError};
use crate::editor::TexTextView;
use crate::preview::PdfCanvas;
use crate::sync::SyncIndex;
use crate::{convert, fixes, paste};
use crate::hints;
use crate::highlight::{self, Kind};
use crate::theme::{self, ThemedSplit};

/// How the status line and the build pill are coloured.
#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Ok,
    Busy,
    Warn,
    Error,
}

/// Pause after the last keystroke before recompiling.
const DEBOUNCE: Duration = Duration::from_millis(250);
const STATUS_HEIGHT: f64 = 28.0;
/// Width kept for "Ln 12, Col 4" at the right of the status bar.
const CURSOR_W: f64 = 150.0;
const PILL_W: f64 = 104.0;
const PILL_H: f64 = 18.0;
/// Menu-bar icon: cycles F → E → A, like the app icon.
const BAR_LETTERS: [&str; 3] = ["F", "E", "A"];
/// The letter changes every 15–20 minutes (a fresh random pick each time).
const BAR_MIN_SECS: u64 = 15 * 60;
const BAR_MAX_SECS: u64 = 20 * 60;

/// A random wait between BAR_MIN_SECS and BAR_MAX_SECS (std-only: seeded from the clock).
fn next_bar_period() -> Duration {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    Duration::from_secs(BAR_MIN_SECS + h.finish() % (BAR_MAX_SECS - BAR_MIN_SECS + 1))
}

const TEMPLATE: &str = r"\documentclass{article}
\usepackage{amsmath}

\title{Hello, Typeset}
\date{\today}

\begin{document}
\maketitle

Type on the left. The preview on the right updates a moment after you stop.

\section{Some math}
Euler's identity: $e^{i\pi} + 1 = 0$.

\begin{equation}
  \int_{-\infty}^{\infty} e^{-x^2}\,dx = \sqrt{\pi}
\end{equation}

\begin{align}
  (a+b)^2 &= a^2 + 2ab + b^2 \\
  \sum_{k=1}^{n} k &= \frac{n(n+1)}{2}
\end{align}

\end{document}
";

#[derive(Default)]
pub struct Ivars {
    window: OnceCell<Retained<NSWindow>>,
    text: OnceCell<Retained<NSTextView>>,
    editor: OnceCell<Retained<TexTextView>>,
    /// Package that would fix the current error (⇧⌘U adds it).
    fix_package: RefCell<Option<String>>,
    /// Paste & Format window: source (rich) and LaTeX views, and the packages needed.
    paste_window: OnceCell<Retained<NSWindow>>,
    paste_src: OnceCell<Retained<NSTextView>>,
    paste_out: OnceCell<Retained<NSTextView>>,
    paste_note: OnceCell<Retained<NSTextField>>,
    paste_packages: RefCell<Vec<&'static str>>,
    /// The preview: our own page drawing (see preview.rs), never blanks out.
    canvas: OnceCell<Retained<PdfCanvas>>,
    /// Click-to-sync data for the PDF on screen.
    sync: RefCell<Option<SyncIndex>>,
    preview_scroll: OnceCell<Retained<NSScrollView>>,
    status: OnceCell<Retained<NSButton>>,
    /// "Fix N…" button beside the status line, and the fixes it offers.
    fix_button: OnceCell<Retained<NSButton>>,
    fixes: RefCell<Vec<fixes::Fix>>,
    /// The rounded chip behind the Fix button.
    fix_chip: OnceCell<Retained<NSBox>>,
    /// Header: document name; build pill (dot + word).
    header_title: OnceCell<Retained<NSTextField>>,
    pill: OnceCell<Retained<NSBox>>,
    pill_dot: OnceCell<Retained<NSBox>>,
    pill_label: OnceCell<Retained<NSTextField>>,
    /// "Ln 12, Col 4" in the status bar.
    cursor_label: OnceCell<Retained<NSTextField>>,
    timer: OnceCell<Retained<NSTimer>>,
    bar_item: OnceCell<Retained<NSStatusItem>>,
    bar_index: Cell<usize>,
    bar_changed: Cell<Option<Instant>>,
    bar_period: Cell<Duration>,
    jobs: OnceCell<Sender<Job>>,
    done: OnceCell<Receiver<Done>>,
    build_dir: OnceCell<PathBuf>,
    latest: Arc<AtomicU64>,

    path: RefCell<Option<PathBuf>>,
    last_pdf: RefCell<Option<Vec<u8>>>,
    last_log: RefCell<String>,
    dirty: Cell<bool>,
    pending: Cell<bool>,
    last_edit: Cell<Option<Instant>>,
    /// Bumped whenever a different document is loaded (gets a fresh, isolated workspace).
    doc_id: Cell<u64>,
    compiling_since: Cell<Option<Instant>>,
    error_line: Cell<Option<usize>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TypesetAppDelegate"]
    #[ivars = Ivars]
    pub struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _n: &NSNotification) {
            self.setup();
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn terminate_after_last_window(&self, _app: &NSApplication) -> bool {
            true
        }

        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _app: &NSApplication) -> NSApplicationTerminateReply {
            if self.confirm_discard() {
                NSApplicationTerminateReply::TerminateNow
            } else {
                NSApplicationTerminateReply::TerminateCancel
            }
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _n: &NSNotification) {
            if let Some(dir) = self.ivars().build_dir.get() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    unsafe impl NSWindowDelegate for Delegate {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _w: &NSWindow) -> bool {
            self.confirm_discard()
        }
    }

    unsafe impl NSTextDelegate for Delegate {
        #[unsafe(method(textDidChange:))]
        fn text_did_change(&self, n: &NSNotification) {
            let iv = self.ivars();
            // Typing/pasting in the Paste & Format window: just refresh its LaTeX.
            if let (Some(src), Some(obj)) = (iv.paste_src.get(), n.object()) {
                if std::ptr::eq(Retained::as_ptr(src).cast::<AnyObject>(), Retained::as_ptr(&obj)) {
                    self.update_paste_preview();
                    return;
                }
            }
            iv.pending.set(true);
            iv.last_edit.set(Some(Instant::now()));
            if let Some(ed) = iv.editor.get() {
                ed.text_changed();
            }
            self.refresh_cursor_label();
            if !iv.dirty.get() {
                iv.dirty.set(true);
                if let Some(w) = iv.window.get() {
                    w.setDocumentEdited(true);
                }
                self.refresh_title();
            }
        }
    }

    unsafe impl NSTextViewDelegate for Delegate {
        /// The cursor moved: move the current-line highlight, update Ln/Col.
        #[unsafe(method(textViewDidChangeSelection:))]
        fn did_change_selection(&self, n: &NSNotification) {
            let iv = self.ivars();
            let Some(ed) = iv.editor.get() else { return };
            if let Some(obj) = n.object() {
                if !std::ptr::eq(Retained::as_ptr(ed).cast::<AnyObject>(), Retained::as_ptr(&obj)) {
                    return;
                }
            }
            ed.refresh_current_line();
            self.refresh_cursor_label();
        }
    }

    unsafe impl NSTextStorageDelegate for Delegate {
        #[unsafe(method(textStorage:didProcessEditing:range:changeInLength:))]
        fn did_process_editing(
            &self,
            storage: &NSTextStorage,
            mask: NSTextStorageEditActions,
            range: NSRange,
            _delta: NSInteger,
        ) {
            if mask.contains(NSTextStorageEditActions::EditedCharacters) {
                highlight_range(storage, range);
            }
        }
    }

    // Menu actions. Target is nil, so they arrive through the responder chain,
    // which ends at the application delegate.
    impl Delegate {
        #[unsafe(method(newDocument:))]
        fn new_document(&self, _sender: Option<&AnyObject>) {
            if self.confirm_discard() {
                self.set_document(TEMPLATE, None);
            }
        }

        #[unsafe(method(openDocument:))]
        fn open_document(&self, _sender: Option<&AnyObject>) {
            if !self.confirm_discard() {
                return;
            }
            let mtm = self.mtm();
            let panel = NSOpenPanel::openPanel(mtm);
            panel.setCanChooseFiles(true);
            panel.setCanChooseDirectories(false);
            panel.setAllowsMultipleSelection(false);
            panel.setAllowedContentTypes(&content_types(&["tex", "ltx", "txt"]));
            if panel.runModal() == NSModalResponseOK {
                if let Some(path) = panel.URL().and_then(|u| url_path(&u)) {
                    self.open_path(&path);
                }
            }
        }

        #[unsafe(method(saveDocument:))]
        fn save_document(&self, _sender: Option<&AnyObject>) {
            self.save();
        }

        #[unsafe(method(saveDocumentAs:))]
        fn save_document_as(&self, _sender: Option<&AnyObject>) {
            self.save_as();
        }

        #[unsafe(method(exportPDF:))]
        fn export_pdf(&self, _sender: Option<&AnyObject>) {
            let Some(pdf) = self.ivars().last_pdf.borrow().clone() else {
                alert("Nothing to export yet", "The document hasn't compiled successfully yet.");
                return;
            };
            let stem = self.doc_name().trim_end_matches(".tex").to_string();
            let panel = NSSavePanel::savePanel(self.mtm());
            panel.setAllowedContentTypes(&content_types(&["pdf"]));
            panel.setNameFieldStringValue(&NSString::from_str(&format!("{stem}.pdf")));
            if panel.runModal() == NSModalResponseOK {
                if let Some(path) = panel.URL().and_then(|u| url_path(&u)) {
                    if let Err(e) = write_atomic(&path, &pdf) {
                        alert("Couldn't export the PDF", &e.to_string());
                    }
                }
            }
        }

        #[unsafe(method(showMainWindow:))]
        fn show_main_window(&self, _sender: Option<&AnyObject>) {
            let app = NSApplication::sharedApplication(self.mtm());
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
            if let Some(w) = self.ivars().window.get() {
                w.makeKeyAndOrderFront(None);
            }
        }

        #[unsafe(method(compileNow:))]
        fn compile_now(&self, _sender: Option<&AnyObject>) {
            self.request_compile();
        }

        #[unsafe(method(jumpToError:))]
        fn jump_to_error(&self, _sender: Option<&AnyObject>) {
            let iv = self.ivars();
            if let Some(line) = iv.error_line.get() {
                self.go_to_line(line);
            }
        }

        /// ⌘J: highlight the cursor's line in the PDF preview.
        #[unsafe(method(showInPreview:))]
        fn show_in_preview(&self, _sender: Option<&AnyObject>) {
            let iv = self.ivars();
            let Some(tv) = iv.text.get() else { return };
            let loc = tv.selectedRange().location;
            let text = tv.string();
            let before = text.substringWithRange(NSRange::new(0, loc.min(text.length()))).to_string();
            let line = before.matches('\n').count() as u32 + 1;
            let spot = iv.sync.borrow().as_ref().and_then(|s| s.spot_for_line(line));
            match (spot, iv.canvas.get()) {
                (Some(s), Some(c)) => c.reveal(s.page, s.x, s.y, s.w, s.h),
                _ => self.set_status("Nothing to show yet — wait for the preview to update", false),
            }
        }

        /// ⇧⌘U or the "Fix N…" button: show the proposed fixes for approval.
        #[unsafe(method(fixErrors:))]
        fn fix_errors(&self, _sender: Option<&AnyObject>) {
            let iv = self.ivars();
            let all = iv.fixes.borrow().clone();
            if all.is_empty() {
                self.set_status("Nothing to fix right now", false);
                return;
            }
            let Some(tv) = iv.text.get() else { return };
            let mtm = self.mtm();
            // One checkbox per fix (the first 20; any others come back after the next compile).
            let shown: Vec<fixes::Fix> = all.into_iter().take(20).collect();
            let row = 24.0;
            let width = 600.0;
            let height = row * shown.len() as f64;
            let list = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height)));
            let mut boxes = Vec::new();
            for (i, f) in shown.iter().enumerate() {
                let cb = unsafe { NSButton::checkboxWithTitle_target_action(&NSString::from_str(&f.description), None, None, mtm) };
                cb.setFrame(NSRect::new(NSPoint::new(0.0, height - row * (i as f64 + 1.0)), NSSize::new(width, row - 2.0)));
                cb.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
                cb.setToolTip(Some(&NSString::from_str(&f.description)));
                cb.setState(if f.default_on { NSControlStateValueOn } else { NSControlStateValueOff });
                list.addSubview(&cb);
                boxes.push(cb);
            }
            let alert = NSAlert::new(mtm);
            let n = shown.len();
            alert.setMessageText(&NSString::from_str(&if n == 1 { "Fix this problem?".to_string() } else { format!("Fix {n} problems?") }));
            alert.setInformativeText(&NSString::from_str("Untick anything you don't want. A single ⌘Z undoes all of it."));
            alert.setAccessoryView(Some(&list));
            alert.addButtonWithTitle(&NSString::from_str("Apply Fixes"));
            alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            if alert.runModal() != NSAlertFirstButtonReturn {
                return;
            }
            let chosen: Vec<&fixes::Fix> = shown.iter().zip(&boxes).filter(|(_, cb)| cb.state() == NSControlStateValueOn).map(|(f, _)| f).collect();
            if chosen.is_empty() {
                return;
            }
            let doc = tv.string().to_string();
            let edits = fixes::applicable_edits(&doc, &chosen);
            if edits.is_empty() {
                self.set_status("The document changed since the last compile — fixes will refresh in a moment", false);
                self.request_compile();
                return;
            }
            // Apply last-first so earlier offsets stay valid; one undo step for all.
            let undo = tv.undoManager();
            if let Some(u) = &undo {
                u.beginUndoGrouping();
            }
            for e in &edits {
                let start = crate::complete::utf16_len(&doc[..e.start]);
                let len = crate::complete::utf16_len(&doc[e.start..e.end]);
                self.replace_text(NSRange::new(start, len), &e.text);
            }
            if let Some(u) = &undo {
                u.endUndoGrouping();
                u.setActionName(&NSString::from_str("Fix Errors"));
            }
            iv.fixes.borrow_mut().clear();
            self.refresh_fix_button();
            self.set_status("Fixes applied — recompiling (⌘Z undoes them)", false);
            self.request_compile();
        }

        /// ⇧⌘V: paste the clipboard converted to LaTeX.
        #[unsafe(method(pasteAsLatex:))]
        fn paste_as_latex(&self, _sender: Option<&AnyObject>) {
            match paste::from_clipboard(false) {
                paste::Pasted::Converted(c) => {
                    let pkgs: Vec<&'static str> = c.packages.iter().copied().collect();
                    self.insert_latex(&c.body, &pkgs);
                }
                paste::Pasted::Raw(text) => {
                    self.insert_latex(&text, &[]);
                    let risky = convert::risky_latex(&text);
                    if risky.is_empty() {
                        self.set_status("That's already LaTeX — pasted as is", false);
                    } else {
                        self.set_status_tone(&format!("Pasted LaTeX uses {} — check what it reads or writes before relying on it", risky.join(", ")), Tone::Warn);
                    }
                }
                paste::Pasted::Nothing => self.set_status("The clipboard is empty", false),
            }
        }

        /// ⌥⌘V: open the Paste & Format window.
        #[unsafe(method(showPasteAndFormat:))]
        fn show_paste_and_format(&self, _sender: Option<&AnyObject>) {
            self.ensure_paste_window();
            let iv = self.ivars();
            if let (Some(w), Some(src)) = (iv.paste_window.get(), iv.paste_src.get()) {
                w.makeKeyAndOrderFront(None);
                w.makeFirstResponder(Some(src));
            }
        }

        #[unsafe(method(pasteFormatInsert:))]
        fn paste_format_insert(&self, _sender: Option<&AnyObject>) {
            let iv = self.ivars();
            let Some(out) = iv.paste_out.get() else { return };
            let text = out.string().to_string();
            if text.trim().is_empty() {
                return;
            }
            let pkgs = iv.paste_packages.borrow().clone();
            if let Some(w) = iv.paste_window.get() {
                w.orderOut(None);
            }
            self.insert_latex(&text, &pkgs);
            if let Some(w) = iv.window.get() {
                w.makeKeyAndOrderFront(None);
            }
        }

        #[unsafe(method(pasteFormatNewDocument:))]
        fn paste_format_new_document(&self, _sender: Option<&AnyObject>) {
            let iv = self.ivars();
            let Some(out) = iv.paste_out.get() else { return };
            let text = out.string().to_string();
            if text.trim().is_empty() || !self.confirm_discard() {
                return;
            }
            let packages = iv.paste_packages.borrow().iter().copied().collect();
            let doc = convert::wrap_document(&convert::Converted { body: text, packages, title: None });
            if let Some(w) = iv.paste_window.get() {
                w.orderOut(None);
            }
            self.set_document(&doc, None);
            if let Some(w) = iv.window.get() {
                w.makeKeyAndOrderFront(None);
            }
        }

        /// File → Import Document…: Word, Pages/RTF, OpenDocument, HTML, Markdown, text.
        #[unsafe(method(importDocument:))]
        fn import_document(&self, _sender: Option<&AnyObject>) {
            if !self.confirm_discard() {
                return;
            }
            let panel = NSOpenPanel::openPanel(self.mtm());
            panel.setCanChooseFiles(true);
            panel.setCanChooseDirectories(false);
            panel.setAllowsMultipleSelection(false);
            panel.setMessage(Some(&NSString::from_str("Choose a Word (.docx), RTF, OpenDocument, web page, Markdown or text file")));
            panel.setAllowedContentTypes(&content_types(paste::IMPORT_TYPES));
            if panel.runModal() != NSModalResponseOK {
                return;
            }
            let Some(path) = panel.URL().and_then(|u| url_path(&u)) else { return };
            match paste::from_file(&path) {
                Ok(c) => {
                    let doc = convert::wrap_document(&c);
                    self.set_document(&doc, None);
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.set_status(&format!("Imported {name} — save it to keep it"), false);
                }
                Err(e) => alert("Couldn't import that file", &e),
            }
        }

        #[unsafe(method(showCompilerOutput:))]
        fn show_compiler_output(&self, _sender: Option<&AnyObject>) {
            let log = self.ivars().last_log.borrow().clone();
            let log = log.trim();
            let shown: String = if log.is_empty() {
                "No output from the last compile.".into()
            } else {
                // Keep the alert a sensible size: show the last ~60 lines.
                let lines: Vec<&str> = log.lines().collect();
                lines[lines.len().saturating_sub(60)..].join("\n")
            };
            alert("Compiler output", &shown);
        }

        #[unsafe(method(zoomPreviewIn:))]
        fn zoom_preview_in(&self, _sender: Option<&AnyObject>) {
            if let Some(s) = self.ivars().preview_scroll.get() {
                s.setMagnification((s.magnification() * 1.25).min(6.0));
            }
        }

        #[unsafe(method(zoomPreviewOut:))]
        fn zoom_preview_out(&self, _sender: Option<&AnyObject>) {
            if let Some(s) = self.ivars().preview_scroll.get() {
                s.setMagnification((s.magnification() / 1.25).max(0.3));
            }
        }

        #[unsafe(method(fitPreview:))]
        fn fit_preview(&self, _sender: Option<&AnyObject>) {
            if let Some(s) = self.ivars().preview_scroll.get() {
                s.setMagnification(1.0);
            }
        }
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars::default());
        unsafe { msg_send![super(this), init] }
    }

    fn setup(&self) {
        let mtm = self.mtm();
        let iv = self.ivars();
        let app = NSApplication::sharedApplication(mtm);
        app.setMainMenu(Some(&build_menu(mtm, &app)));

        // Compiler worker.
        match compile::make_build_dir() {
            Ok(dir) => {
                let (tx, rx) = compile::spawn_worker(dir.clone(), iv.latest.clone());
                let _ = iv.jobs.set(tx);
                let _ = iv.done.set(rx);
                let _ = iv.build_dir.set(dir);
            }
            Err(e) => alert("Couldn't create a private build folder", &e.to_string()),
        }

        // Window: one dark surface, the title bar blended into our header.
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1320.0, 840.0));
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Miniaturizable
            | NSWindowStyleMask::Resizable
            | NSWindowStyleMask::FullSizeContentView;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setDelegate(Some(ProtocolObject::from_ref(self)));
        window.setMinSize(NSSize::new(720.0, 420.0));
        window.setTitlebarAppearsTransparent(true);
        window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        window.setBackgroundColor(Some(&theme::chrome()));
        let content = window.contentView().expect("content view");
        let bounds = content.bounds();
        let (w, h) = (bounds.size.width, bounds.size.height);
        // The header is exactly as tall as the title bar, so the window
        // buttons sit centred in it.
        let header = {
            let t = h - window.contentLayoutRect().size.height;
            if (20.0..=64.0).contains(&t) { t } else { 28.0 }
        };
        let head_y = h - header;
        let head_mid = head_y + header / 2.0;
        let top_band = NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin;
        let bottom_band = NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMaxYMargin;

        // Editor (with Overleaf-style suggestions, line numbers; see editor.rs).
        let (scroll, editor) = TexTextView::new_scrollable(mtm);
        let text: Retained<NSTextView> = Retained::into_super(editor.clone());
        text.setRichText(false);
        text.setFont(Some(&theme::mono(theme::EDITOR_SIZE)));
        text.setTextColor(Some(&theme::text()));
        text.setBackgroundColor(&theme::editor_bg());
        text.setDrawsBackground(true);
        text.setInsertionPointColor(Some(&theme::amber()));
        {
            let keys = unsafe { [NSBackgroundColorAttributeName] };
            let sel_bg = theme::accent_alpha(0.22);
            let values: [&AnyObject; 1] = [&sel_bg];
            unsafe { text.setSelectedTextAttributes(&NSDictionary::from_slices(&keys, &values)) };
        }
        text.setDefaultParagraphStyle(Some(&line_style()));
        text.setAllowsUndo(true);
        text.setUsesFindBar(true);
        text.setIncrementalSearchingEnabled(true);
        text.setAutomaticQuoteSubstitutionEnabled(false);
        text.setAutomaticDashSubstitutionEnabled(false);
        text.setAutomaticTextReplacementEnabled(false);
        text.setAutomaticSpellingCorrectionEnabled(false);
        text.setContinuousSpellCheckingEnabled(false);
        text.setSmartInsertDeleteEnabled(false);
        text.setDelegate(Some(ProtocolObject::from_ref(self)));
        if let Some(storage) = unsafe { text.textStorage() } {
            storage.setDelegate(Some(ProtocolObject::from_ref(self)));
        }

        // Preview (flicker-free, see preview.rs).
        let body_h = h - header - STATUS_HEIGHT;
        let preview_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w / 2.0, body_h));
        let (preview, canvas) = PdfCanvas::new_scrollable(mtm, preview_frame);
        // Our header already clears the title bar; no automatic insets.
        scroll.setAutomaticallyAdjustsContentInsets(false);
        preview.setAutomaticallyAdjustsContentInsets(false);

        // Split view fills everything between the header and the status bar.
        let split = ThemedSplit::new(mtm, NSRect::new(NSPoint::new(0.0, STATUS_HEIGHT), NSSize::new(w, body_h)));
        split.setVertical(true);
        split.setDividerStyle(NSSplitViewDividerStyle::Thin);
        split.addSubview(&scroll);
        split.addSubview(&preview);
        split.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content.addSubview(&split);

        // ---- header: wordmark · document name · build pill ----
        let header_bg = theme::surface(mtm, rect(0.0, head_y, w, header), &theme::chrome(), None, 0.0);
        header_bg.setAutoresizingMask(top_band);
        content.addSubview(&header_bg);
        let header_line = theme::surface(mtm, rect(0.0, head_y, w, 1.0), &theme::hairline(), None, 0.0);
        header_line.setAutoresizingMask(top_band);
        content.addSubview(&header_line);

        let mark = theme::label(mtm, rect(0.0, 0.0, 10.0, 10.0), &theme::join(&[
            theme::styled("TYPESET", &theme::mono_semibold(10.0), &theme::text_faint(), 2.6),
            theme::styled(" \u{258D}", &theme::mono(10.0), &theme::amber(), 0.0),
        ]));
        place(&mark, 80.0, head_mid, 110.0);
        mark.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMaxXMargin | NSAutoresizingMaskOptions::ViewMinYMargin);
        content.addSubview(&mark);

        let title = theme::label(mtm, rect(0.0, 0.0, 10.0, 10.0), &theme::styled("Untitled.tex", &theme::mono_medium(12.0), &theme::text(), 0.0));
        place(&title, (w / 2.0 - 280.0).round(), head_mid, 560.0);
        title.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
        title.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewMaxXMargin | NSAutoresizingMaskOptions::ViewMinYMargin,
        );
        content.addSubview(&title);

        let right_top = NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewMinYMargin;
        let px = w - PILL_W - 14.0;
        let py = (head_mid - PILL_H / 2.0).round();
        let pill = theme::surface(mtm, rect(px, py, PILL_W, PILL_H), &theme::raised(), Some(&theme::raised_border()), PILL_H / 2.0);
        pill.setAutoresizingMask(right_top);
        content.addSubview(&pill);
        let dot = theme::surface(mtm, rect(px + 11.0, py + (PILL_H - 6.0) / 2.0, 6.0, 6.0), &theme::text_faint(), None, 3.0);
        dot.setAutoresizingMask(right_top);
        content.addSubview(&dot);
        let pill_label = theme::label(mtm, rect(0.0, 0.0, 10.0, 10.0), &pill_text("STARTING", &theme::text_dim()));
        place(&pill_label, px + 23.0, py + PILL_H / 2.0, PILL_W - 28.0);
        pill_label.setAutoresizingMask(right_top);
        content.addSubview(&pill_label);

        // ---- status bar: message · Fix chip · Ln/Col ----
        let bar = theme::surface(mtm, rect(0.0, 0.0, w, STATUS_HEIGHT), &theme::chrome(), None, 0.0);
        bar.setAutoresizingMask(bottom_band);
        content.addSubview(&bar);
        let bar_line = theme::surface(mtm, rect(0.0, STATUS_HEIGHT - 1.0, w, 1.0), &theme::hairline(), None, 0.0);
        bar_line.setAutoresizingMask(bottom_band);
        content.addSubview(&bar_line);

        // Status line (click it to jump to the error).
        let target: &AnyObject = self;
        let status = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Starting…"),
                Some(target),
                Some(sel!(jumpToError:)),
                mtm,
            )
        };
        status.setBordered(false);
        status.setAlignment(NSTextAlignment::Left);
        status.setFont(Some(&theme::mono(11.0)));
        status.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        status.setContentTintColor(Some(&theme::text_dim()));
        status.setFrame(rect(12.0, (STATUS_HEIGHT - 18.0) / 2.0, w - 24.0 - CURSOR_W, 18.0));
        status.setAutoresizingMask(bottom_band);
        content.addSubview(&status);

        // "Fix N problems": shown when Typeset knows how to fix the current errors.
        let chip = theme::surface(mtm, NSRect::ZERO, &theme::accent_alpha(0.10), Some(&theme::accent_alpha(0.45)), 10.0);
        chip.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewMaxYMargin);
        chip.setHidden(true);
        content.addSubview(&chip);
        let fix_button = unsafe {
            NSButton::buttonWithTitle_target_action(&NSString::from_str("Fix…"), Some(target), Some(sel!(fixErrors:)), mtm)
        };
        fix_button.setBordered(false);
        fix_button.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewMaxYMargin);
        fix_button.setToolTip(Some(&NSString::from_str("Review and apply fixes for these errors (⇧⌘U)")));
        fix_button.setHidden(true);
        content.addSubview(&fix_button);

        let cursor = NSTextField::labelWithString(&NSString::from_str("Ln 1, Col 1"), mtm);
        cursor.setFont(Some(&theme::mono(11.0)));
        cursor.setTextColor(Some(&theme::text_faint()));
        cursor.setAlignment(NSTextAlignment::Right);
        cursor.setSelectable(false);
        place(&cursor, w - CURSOR_W - 12.0, STATUS_HEIGHT / 2.0, CURSOR_W);
        cursor.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewMaxYMargin);
        content.addSubview(&cursor);

        let _ = iv.fix_button.set(fix_button);
        let _ = iv.fix_chip.set(chip);
        let _ = iv.header_title.set(title);
        let _ = iv.pill.set(pill);
        let _ = iv.pill_dot.set(dot);
        let _ = iv.pill_label.set(pill_label);
        let _ = iv.cursor_label.set(cursor);

        let _ = iv.window.set(window.clone());
        let _ = iv.text.set(text.clone());
        let _ = iv.editor.set(editor);
        // Double-click or ⌘-click in the preview: jump to that line of source.
        let this = self.retain();
        canvas.set_on_sync_click(Box::new(move |page, x, y| {
            let line = this.ivars().sync.borrow().as_ref().and_then(|s| s.line_at(page, x, y));
            match line {
                Some(l) => this.go_to_line(l as usize),
                None => this.set_status("Couldn't find that spot in the source", false),
            }
        }));
        let _ = iv.canvas.set(canvas);
        let _ = iv.preview_scroll.set(preview);
        let _ = iv.status.set(status);
        self.refresh_fix_button();

        window.center();
        window.setFrameAutosaveName(&NSString::from_str("TypesetMainWindow"));
        split.adjustSubviews();
        split.setPosition_ofDividerAtIndex(split.frame().size.width * 0.5, 0);
        window.makeKeyAndOrderFront(None);
        window.makeFirstResponder(Some(&text));

        // First document: a path given on the command line, else the template.
        let arg = std::env::args().skip(1).find(|a| !a.starts_with("-psn_"));
        match arg.map(PathBuf::from) {
            Some(p) if p.is_file() => self.open_path(&p),
            _ => self.set_document(TEMPLATE, None),
        }

        self.setup_menu_bar_icon();

        // 30 ms main-thread tick: debounce edits and collect compile results.
        // Registered for common modes so the preview keeps updating during
        // window resizes and divider drags.
        let this = self.retain();
        let block = RcBlock::new(move |_t: NonNull<NSTimer>| this.tick());
        let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(0.03, true, &block) };
        unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
        let _ = iv.timer.set(timer);

        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }

    fn tick(&self) {
        let iv = self.ivars();
        // Keep the preview pages fitted to the pane as it's resized.
        if let (Some(scroll), Some(canvas)) = (iv.preview_scroll.get(), iv.canvas.get()) {
            canvas.fit_width(scroll.contentSize().width);
        }
        if iv.bar_changed.get().is_some_and(|t| t.elapsed() >= iv.bar_period.get()) {
            iv.bar_index.set((iv.bar_index.get() + 1) % BAR_LETTERS.len());
            self.show_bar_letter();
        }
        if let Some(rx) = iv.done.get() {
            while let Ok(done) = rx.try_recv() {
                if done.generation == iv.latest.load(Ordering::SeqCst) {
                    self.apply(done.outcome);
                }
            }
        }
        if iv.pending.get() && iv.last_edit.get().is_some_and(|t| t.elapsed() >= DEBOUNCE) {
            self.request_compile();
        }
        if let Some(since) = iv.compiling_since.get() {
            let waited = since.elapsed();
            if iv.last_pdf.borrow().is_none() && waited > Duration::from_secs(2) {
                self.set_status(
                    &format!(
                        "Compiling… {}s — the first run downloads LaTeX packages and can take a minute",
                        waited.as_secs()
                    ),
                    false,
                );
            } else if waited > Duration::from_millis(400) {
                self.set_status("Compiling…", false);
            }
        }
    }

    fn setup_menu_bar_icon(&self) {
        let mtm = self.mtm();
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        if let Some(button) = item.button(mtm) {
            button.setToolTip(Some(&NSString::from_str("Typeset")));
        }
        let target: &AnyObject = self;
        let menu = NSMenu::new(mtm);
        for (title, action, key) in [
            ("Show Typeset", sel!(showMainWindow:), ""),
            ("Compile Now", sel!(compileNow:), ""),
        ] {
            let mi = menu_item(mtm, title, Some(action), key);
            unsafe { mi.setTarget(Some(target)) };
            menu.addItem(&mi);
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&menu_item(mtm, "Quit Typeset", Some(sel!(terminate:)), ""));
        item.setMenu(Some(&menu));
        let _ = self.ivars().bar_item.set(item);
        self.show_bar_letter();
    }

    /// Draw the current letter in the menu bar. Serif bold, matching the icon;
    /// the E is a touch smaller and dropped below the baseline, as in the TeX logo.
    fn show_bar_letter(&self) {
        let iv = self.ivars();
        iv.bar_changed.set(Some(Instant::now()));
        iv.bar_period.set(next_bar_period());
        let Some(button) = iv.bar_item.get().and_then(|i| i.button(self.mtm())) else { return };
        let letter = BAR_LETTERS[iv.bar_index.get()];
        let is_e = letter == "E";
        let size = if is_e { 13.0 } else { 15.0 };
        let font = serif_bold(size);
        let offset = NSNumber::new_f64(if is_e { -1.5 } else { 0.0 });
        let keys = unsafe { [NSFontAttributeName, NSBaselineOffsetAttributeName] };
        let values: [&AnyObject; 2] = [&font, &offset];
        let attrs = NSDictionary::from_slices(&keys, &values);
        let title = unsafe {
            NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                &NSString::from_str(letter),
                Some(&attrs),
            )
        };
        button.setAttributedTitle(&title);
    }

    fn request_compile(&self) {
        let iv = self.ivars();
        iv.pending.set(false);
        let (Some(tx), Some(tv), Some(build)) = (iv.jobs.get(), iv.text.get(), iv.build_dir.get()) else {
            return;
        };
        let source = tv.string().to_string();
        let _ = build;
        // Unsaved documents get no folder access at all (empty path).
        let workdir = iv.path.borrow().as_ref().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default();
        let generation = iv.latest.fetch_add(1, Ordering::SeqCst) + 1;
        let doc_id = iv.doc_id.get();
        if tx.send(Job { generation, source, workdir, doc_id }).is_ok() && iv.compiling_since.get().is_none() {
            iv.compiling_since.set(Some(Instant::now()));
        }
    }

    fn apply(&self, outcome: Outcome) {
        let iv = self.ivars();
        iv.compiling_since.set(None);
        match outcome {
            Outcome::Pdf { pdf, elapsed, errors, tier, log, sync, note } => {
                *iv.last_log.borrow_mut() = log;
                *iv.sync.borrow_mut() = sync;
                self.show_pdf(&pdf);
                *iv.last_pdf.borrow_mut() = Some(pdf);
                let lock = format!(" · {}", tier.label());
                self.update_fixes(&errors);
                match errors.first() {
                    None => {
                        iv.error_line.set(None);
                        *iv.fix_package.borrow_mut() = None;
                        match note {
                            Some(n) => self.set_status_tone(&format!("{n}{lock}"), Tone::Warn),
                            None => self.set_status(&format!("Updated in {:.2} s{lock}", elapsed.as_secs_f64()), false),
                        }
                    }
                    Some(first) => {
                        let msg = self.describe_error(first, errors.len() - 1);
                        self.set_status(&msg, true);
                    }
                }
            }
            Outcome::Failed { errors, log } => {
                *iv.last_log.borrow_mut() = log;
                self.update_fixes(&errors);
                let msg = match errors.first() {
                    Some(first) => self.describe_error(first, errors.len() - 1),
                    None => {
                        iv.error_line.set(None);
                        *iv.fix_package.borrow_mut() = None;
                        "Compile failed".into()
                    }
                };
                self.set_status(&format!("{msg}  (preview not updated)"), true);
            }
            Outcome::EngineMissing(msg) => self.set_status(&msg, true),
            Outcome::Cancelled => {}
        }
    }

    /// Work out fixes for the latest errors and show/hide the "Fix N…" button.
    fn update_fixes(&self, errors: &[TexError]) {
        let iv = self.ivars();
        let doc = iv.text.get().map(|t| t.string().to_string()).unwrap_or_default();
        *iv.fixes.borrow_mut() = if errors.is_empty() { Vec::new() } else { fixes::propose(&doc, errors) };
        self.refresh_fix_button();
    }

    fn refresh_fix_button(&self) {
        let iv = self.ivars();
        let Some(b) = iv.fix_button.get() else { return };
        let n = iv.fixes.borrow().len();
        let width = iv.window.get().and_then(|w| w.contentView()).map(|v| v.bounds().size.width).unwrap_or(1320.0);
        let right = width - CURSOR_W - 24.0;
        let mut status_right = right;
        b.setHidden(n == 0);
        if let Some(chip) = iv.fix_chip.get() {
            chip.setHidden(n == 0);
            if n > 0 {
                let words = if n == 1 { "Fix 1 problem".to_string() } else { format!("Fix {n} problems") };
                b.setAttributedTitle(&theme::join(&[
                    theme::styled("\u{2726}  ", &theme::mono(10.0), &theme::accent(), 0.0),
                    theme::styled(&words, &theme::mono_semibold(11.0), &theme::accent(), 0.2),
                ]));
                b.sizeToFit();
                let bw = b.frame().size.width.ceil();
                let chip_w = bw + 20.0;
                let chip_x = right - chip_w;
                let y = ((STATUS_HEIGHT - 20.0) / 2.0).round();
                chip.setFrame(rect(chip_x, y, chip_w, 20.0));
                b.setFrame(rect(chip_x + 10.0, y, bw, 20.0));
                status_right = chip_x - 12.0;
            }
        }
        if let Some(st) = iv.status.get() {
            let f = st.frame();
            st.setFrame(rect(f.origin.x, f.origin.y, (status_right - f.origin.x).max(40.0), f.size.height));
        }
    }

    /// Header title: "folder / name.tex", plus an amber dot while unsaved.
    fn refresh_title(&self) {
        let iv = self.ivars();
        let Some(label) = iv.header_title.get() else { return };
        let folder = iv
            .path
            .borrow()
            .as_ref()
            .and_then(|p| p.parent().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned()));
        let mut parts = Vec::new();
        match folder {
            Some(f) => parts.push(theme::styled(&format!("{f}  /  "), &theme::mono(12.0), &theme::text_faint(), 0.0)),
            None if iv.path.borrow().is_none() => {
                parts.push(theme::styled("unsaved  /  ", &theme::mono(12.0), &theme::text_faint(), 0.0))
            }
            None => {}
        }
        parts.push(theme::styled(&self.doc_name(), &theme::mono_medium(12.0), &theme::text(), 0.0));
        if iv.dirty.get() {
            parts.push(theme::styled("  \u{25CF}", &theme::mono(9.0), &theme::amber(), 0.0));
        }
        let joined = theme::join(&parts);
        let centred = NSMutableAttributedString::new();
        centred.appendAttributedString(&joined);
        let ps = NSMutableParagraphStyle::new();
        ps.setAlignment(NSTextAlignment::Center);
        ps.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
        unsafe {
            centred.addAttribute_value_range(NSParagraphStyleAttributeName, &ps, NSRange::new(0, centred.length()));
        }
        label.setAttributedStringValue(&centred);
    }

    /// "Ln 12, Col 4" (and how much is selected).
    fn refresh_cursor_label(&self) {
        let iv = self.ivars();
        let (Some(ed), Some(label)) = (iv.editor.get(), iv.cursor_label.get()) else { return };
        let sel = ed.selectedRange();
        let (line, col) = ed.line_col(sel.location);
        let text = if sel.length > 0 {
            format!("{} sel  ·  Ln {line}, Col {col}", sel.length)
        } else {
            format!("Ln {line}, Col {col}")
        };
        label.setStringValue(&NSString::from_str(&text));
    }

    /// "Line 7: Undefined control sequence — \\mathbb needs the amssymb package
    /// — press ⇧⌘U to add it". Also remembers the line and any one-key fix.
    fn describe_error(&self, err: &TexError, more: usize) -> String {
        let iv = self.ivars();
        iv.error_line.set(err.line);
        let doc = iv.text.get().map(|t| t.string().to_string()).unwrap_or_default();
        let source_line = err.line.and_then(|n| doc.lines().nth(n.saturating_sub(1)));
        let help = hints::explain(&err.message, source_line, &doc);
        *iv.fix_package.borrow_mut() = help.as_ref().and_then(|h| h.add_package.clone());

        let mut msg = String::new();
        if let Some(l) = err.line {
            msg.push_str(&format!("Line {l}: "));
        }
        msg.push_str(err.message.trim_end_matches('.'));
        if let Some(h) = &help {
            msg.push_str(&format!(" — {}", h.text));

        }
        if more > 0 {
            msg.push_str(&format!("  (+{more} more)"));
        }
        msg
    }

    /// Replace a range of the editor's text through the normal edit path (undoable).
    fn replace_text(&self, range: NSRange, text: &str) {
        let Some(tv) = self.ivars().text.get() else { return };
        let ns = NSString::from_str(text);
        if tv.shouldChangeTextInRange_replacementString(range, Some(&ns)) {
            if let Some(storage) = unsafe { tv.textStorage() } {
                storage.replaceCharactersInRange_withString(range, &ns);
            }
            tv.didChangeText();
        }
    }

    /// Add a line to the preamble (after the last \usepackage).
    fn add_preamble_line(&self, line: &str) {
        let Some(tv) = self.ivars().text.get() else { return };
        let doc = tv.string().to_string();
        let (byte_at, text) = hints::line_insertion(&doc, line);
        let at = crate::complete::utf16_len(&doc[..byte_at]);
        self.replace_text(NSRange::new(at, 0), &text);
    }

    /// Insert converted LaTeX at the cursor and load any packages it needs.
    fn insert_latex(&self, latex: &str, packages: &[&'static str]) {
        let iv = self.ivars();
        let Some(tv) = iv.text.get() else { return };
        let doc = tv.string().to_string();
        if doc.trim().is_empty() {
            // Empty editor: make it a complete document.
            let full = convert::wrap_document(&convert::Converted {
                body: latex.to_string(),
                packages: packages.iter().copied().collect(),
                title: None,
            });
            self.replace_text(NSRange::new(0, tv.string().length()), &full);
            self.set_status("Pasted as a new document", false);
            return;
        }
        let sel = tv.selectedRange();
        self.replace_text(sel, latex);
        let after = sel.location + crate::complete::utf16_len(latex);
        tv.setSelectedRange(NSRange::new(after, 0));
        let mut added = Vec::new();
        for pkg in packages {
            let doc = tv.string().to_string();
            if doc.contains("\\documentclass") && !hints::has_package(&doc, pkg) {
                let before = tv.string().length();
                self.add_preamble_line(&convert::package_line(pkg));
                // Keep the cursor after the pasted text.
                let grew = tv.string().length() - before;
                let cur = tv.selectedRange().location;
                tv.setSelectedRange(NSRange::new(cur + grew, 0));
                added.push(format!("\\usepackage{{{pkg}}}"));
            }
        }
        tv.scrollRangeToVisible(tv.selectedRange());
        if added.is_empty() {
            self.set_status("Pasted as LaTeX", false);
        } else {
            self.set_status(&format!("Pasted as LaTeX — also added {}", added.join(", ")), false);
        }
    }

    /// Build the Paste & Format window (once).
    fn ensure_paste_window(&self) {
        let iv = self.ivars();
        if iv.paste_window.get().is_some() {
            return;
        }
        let mtm = self.mtm();
        let (w, h) = (1000.0, 620.0);
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str("Paste & Format"));
        window.setMinSize(NSSize::new(640.0, 360.0));
        window.setTitlebarAppearsTransparent(true);
        window.setBackgroundColor(Some(&theme::chrome()));
        let content = window.contentView().expect("content view");
        let top = 34.0;
        let bottom = 52.0;

        let label = NSTextField::labelWithString(
            &NSString::from_str("Paste a document, web page or note on the left (⌘V). The LaTeX appears on the right — edit it if you like, then insert."),
            mtm,
        );
        label.setFrame(NSRect::new(NSPoint::new(14.0, h - top + 8.0), NSSize::new(w - 28.0, 18.0)));
        label.setTextColor(Some(&theme::text_dim()));
        label.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin);
        content.addSubview(&label);

        // Left: rich text (keeps formatting when you paste). Right: the LaTeX.
        // A text view that only accepts sanitised rich text (see paste.rs).
        let src_view = paste::PasteSourceView::new(mtm);
        let src_scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w / 2.0, h - top - bottom)));
        src_scroll.setHasVerticalScroller(true);
        src_scroll.setAutohidesScrollers(true);
        // The pasted document shows as it would on paper (light), so its own
        // colours stay readable; the LaTeX beside it is in the dark editor style.
        if let Some(light) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }) {
            src_scroll.setAppearance(Some(&light));
        }
        let size = src_scroll.contentSize();
        src_view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), size));
        src_view.setMinSize(NSSize::new(0.0, size.height));
        src_view.setMaxSize(NSSize::new(f64::MAX, f64::MAX));
        src_view.setVerticallyResizable(true);
        src_view.setHorizontallyResizable(false);
        src_view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        if let Some(container) = unsafe { src_view.textContainer() } {
            container.setContainerSize(NSSize::new(size.width, f64::MAX));
            container.setWidthTracksTextView(true);
        }
        src_scroll.setDocumentView(Some(&src_view));
        let src: Retained<NSTextView> = Retained::into_super(src_view);
        src.setRichText(true);
        src.setImportsGraphics(false);
        src.setAllowsUndo(true);
        src.setTextContainerInset(NSSize::new(12.0, 14.0));
        src.setBackgroundColor(&theme::paper());
        src.setDelegate(Some(ProtocolObject::from_ref(self)));

        let out_scroll = NSTextView::scrollableTextView(mtm);
        let out: Retained<NSTextView> = out_scroll.documentView().and_then(|v| v.downcast::<NSTextView>().ok()).expect("text view");
        out.setRichText(false);
        out.setAllowsUndo(true);
        out.setFont(Some(&theme::mono(12.0)));
        out.setTextColor(Some(&theme::text()));
        out.setBackgroundColor(&theme::editor_bg());
        out.setInsertionPointColor(Some(&theme::amber()));
        out_scroll.setBackgroundColor(&theme::editor_bg());
        out_scroll.setScrollerKnobStyle(NSScrollerKnobStyle::Light);
        out.setAutomaticQuoteSubstitutionEnabled(false);
        out.setAutomaticDashSubstitutionEnabled(false);
        out.setAutomaticTextReplacementEnabled(false);
        out.setAutomaticSpellingCorrectionEnabled(false);
        out.setTextContainerInset(NSSize::new(12.0, 14.0));

        let split = ThemedSplit::new(mtm, NSRect::new(NSPoint::new(0.0, bottom), NSSize::new(w, h - top - bottom)));
        split.setVertical(true);
        split.setDividerStyle(NSSplitViewDividerStyle::Thin);
        split.addSubview(&src_scroll);
        split.addSubview(&out_scroll);
        split.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
        content.addSubview(&split);
        split.adjustSubviews();
        split.setPosition_ofDividerAtIndex(w / 2.0, 0);

        let note = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        note.setFrame(NSRect::new(NSPoint::new(14.0, 17.0), NSSize::new(w - 330.0, 18.0)));
        note.setTextColor(Some(&theme::amber()));
        note.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        note.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMaxYMargin);
        content.addSubview(&note);

        let target: &AnyObject = self;
        let insert = unsafe {
            NSButton::buttonWithTitle_target_action(&NSString::from_str("Insert at Cursor"), Some(target), Some(sel!(pasteFormatInsert:)), mtm)
        };
        insert.setKeyEquivalent(&NSString::from_str("\r"));
        insert.setFrame(NSRect::new(NSPoint::new(w - 160.0, 12.0), NSSize::new(146.0, 30.0)));
        insert.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewMaxYMargin);
        content.addSubview(&insert);
        let newdoc = unsafe {
            NSButton::buttonWithTitle_target_action(&NSString::from_str("New Document"), Some(target), Some(sel!(pasteFormatNewDocument:)), mtm)
        };
        newdoc.setFrame(NSRect::new(NSPoint::new(w - 306.0, 12.0), NSSize::new(140.0, 30.0)));
        newdoc.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewMaxYMargin);
        content.addSubview(&newdoc);

        window.center();
        let _ = iv.paste_window.set(window);
        let _ = iv.paste_src.set(src);
        let _ = iv.paste_out.set(out);
        let _ = iv.paste_note.set(note);
    }

    /// Re-convert the Paste & Format source into LaTeX.
    fn update_paste_preview(&self) {
        let iv = self.ivars();
        let (Some(src), Some(out)) = (iv.paste_src.get(), iv.paste_out.get()) else { return };
        let Some(storage) = (unsafe { src.textStorage() }) else { return };
        let c = paste::from_attributed(&storage, false);
        out.setString(&NSString::from_str(&c.body));
        let pkgs: Vec<&'static str> = c.packages.iter().copied().collect();
        if let Some(note) = iv.paste_note.get() {
            let msg = if pkgs.is_empty() {
                String::new()
            } else {
                format!("Inserting also adds {} to your preamble", pkgs.iter().map(|p| convert::package_line(p)).collect::<Vec<_>>().join(", "))
            };
            note.setStringValue(&NSString::from_str(&msg));
        }
        *iv.paste_packages.borrow_mut() = pkgs;
    }

    /// Select a 1-based source line, scroll to it and flash it.
    fn go_to_line(&self, line: usize) {
        let iv = self.ivars();
        let Some(tv) = iv.text.get() else { return };
        let (start, len) = highlight::line_range_utf16(&tv.string().to_string(), line);
        let r = NSRange::new(start, len);
        tv.setSelectedRange(r);
        tv.scrollRangeToVisible(r);
        tv.showFindIndicatorForRange(r);
        if let Some(w) = iv.window.get() {
            w.makeFirstResponder(Some(tv));
        }
    }

    /// Show a new PDF. The canvas keeps the old pages on screen until the new
    /// ones are fully drawn, and keeps your scroll position and zoom.
    fn show_pdf(&self, bytes: &[u8]) {
        let iv = self.ivars();
        // Same output as before (e.g. you typed in a comment): nothing to redraw.
        if iv.last_pdf.borrow().as_deref() == Some(bytes) {
            return;
        }
        let data = NSData::with_bytes(bytes);
        let Some(doc) = (unsafe { PDFDocument::initWithData(PDFDocument::alloc(), &data) }) else {
            self.set_status("Tectonic produced a PDF that PDFKit couldn't read", true);
            return;
        };
        if let Some(c) = iv.canvas.get() {
            c.set_document(Some(doc));
        }
    }

    fn set_status(&self, text: &str, error: bool) {
        let tone = if error {
            Tone::Error
        } else if self.ivars().compiling_since.get().is_some() {
            Tone::Busy
        } else {
            Tone::Ok
        };
        self.set_status_tone(text, tone);
    }

    /// Status line text, plus the header pill: ● LIVE / BUILDING / NOTICE / ERROR.
    fn set_status_tone(&self, text: &str, tone: Tone) {
        let iv = self.ivars();
        if let Some(b) = iv.status.get() {
            b.setTitle(&NSString::from_str(text));
            let color = match tone {
                Tone::Ok | Tone::Busy => theme::text_dim(),
                Tone::Warn => theme::amber(),
                Tone::Error => theme::red_text(),
            };
            b.setContentTintColor(Some(&color));
        }
        let (word, dot, word_color) = match tone {
            Tone::Ok => ("LIVE", theme::green(), theme::text_dim()),
            Tone::Busy => ("BUILDING", theme::amber(), theme::text_dim()),
            Tone::Warn => ("NOTICE", theme::amber(), theme::amber()),
            Tone::Error => ("ERROR", theme::red(), theme::red_text()),
        };
        if let Some(d) = iv.pill_dot.get() {
            d.setFillColor(&dot);
        }
        if let Some(l) = iv.pill_label.get() {
            l.setAttributedStringValue(&pill_text(word, &word_color));
        }
        if let Some(p) = iv.pill.get() {
            p.setToolTip(Some(&NSString::from_str(text)));
        }
    }

    // ---- documents --------------------------------------------------------

    fn set_document(&self, contents: &str, path: Option<PathBuf>) {
        let iv = self.ivars();
        iv.doc_id.set(iv.doc_id.get() + 1);
        *iv.sync.borrow_mut() = None;
        if let Some(ed) = iv.editor.get() {
            ed.set_project_dir(path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)));
        }
        *iv.path.borrow_mut() = path;
        *iv.last_pdf.borrow_mut() = None;
        iv.error_line.set(None);
        if let Some(tv) = iv.text.get() {
            tv.setString(&NSString::from_str(contents));
            tv.setSelectedRange(NSRange::new(0, 0));
            if let Some(um) = tv.undoManager() {
                um.removeAllActions();
            }
        }
        if let Some(ed) = iv.editor.get() {
            ed.text_changed();
        }
        self.refresh_cursor_label();
        if let Some(c) = iv.canvas.get() {
            c.set_document(None);
        }
        self.mark_clean();
        self.request_compile();
    }

    fn open_path(&self, path: &Path) {
        const MAX_DOC: u64 = 20 * 1024 * 1024;
        if std::fs::metadata(path).map(|m| m.len() > MAX_DOC).unwrap_or(false) {
            alert("Couldn't open the file", "It's over 20 MB — that isn't a normal LaTeX document.");
            return;
        }
        match std::fs::read(path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(s) => {
                    self.set_document(&s, Some(path.canonicalize().unwrap_or(path.to_path_buf())));
                    if convert::has_bidi_overrides(&s) {
                        alert(
                            "This document contains hidden text-direction characters",
                            "Characters like these can make text look different from what it really is (a known way to disguise code). Review the document before trusting it.",
                        );
                    }
                }
                Err(_) => alert("Couldn't open the file", "It isn't UTF-8 text."),
            },
            Err(e) => alert("Couldn't open the file", &e.to_string()),
        }
    }

    fn doc_name(&self) -> String {
        self.ivars()
            .path
            .borrow()
            .as_ref()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "Untitled.tex".into())
    }

    fn mark_clean(&self) {
        let iv = self.ivars();
        iv.dirty.set(false);
        let Some(w) = iv.window.get() else { return };
        w.setTitle(&NSString::from_str(&self.doc_name()));
        let url = iv
            .path
            .borrow()
            .as_ref()
            .map(|p| NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy())));
        w.setRepresentedURL(url.as_deref());
        w.setDocumentEdited(false);
        self.refresh_title();
    }

    fn save(&self) -> bool {
        let path = self.ivars().path.borrow().clone();
        match path {
            Some(p) => self.write_to(&p),
            None => self.save_as(),
        }
    }

    fn save_as(&self) -> bool {
        let panel = NSSavePanel::savePanel(self.mtm());
        panel.setAllowedContentTypes(&content_types(&["tex"]));
        panel.setNameFieldStringValue(&NSString::from_str(&self.doc_name()));
        if panel.runModal() != NSModalResponseOK {
            return false;
        }
        let Some(path) = panel.URL().and_then(|u| url_path(&u)) else { return false };
        if self.write_to(&path) {
            if let Some(ed) = self.ivars().editor.get() {
                ed.set_project_dir(path.parent().map(Path::to_path_buf));
            }
            *self.ivars().path.borrow_mut() = Some(path);
            self.mark_clean();
            // \input paths now resolve against the new folder.
            self.request_compile();
            true
        } else {
            false
        }
    }

    fn write_to(&self, path: &Path) -> bool {
        let Some(tv) = self.ivars().text.get() else { return false };
        match write_atomic(path, tv.string().to_string().as_bytes()) {
            Ok(()) => {
                self.mark_clean();
                true
            }
            Err(e) => {
                alert("Couldn't save", &e.to_string());
                false
            }
        }
    }

    /// Returns true when it's OK to throw away the current text.
    fn confirm_discard(&self) -> bool {
        let iv = self.ivars();
        if !iv.dirty.get() {
            return true;
        }
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str(&format!(
            "Do you want to save the changes to “{}”?",
            self.doc_name()
        )));
        alert.setInformativeText(&NSString::from_str("Your changes will be lost if you don't save them."));
        alert.addButtonWithTitle(&NSString::from_str("Save"));
        alert.addButtonWithTitle(&NSString::from_str("Don't Save"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        let r = alert.runModal();
        if r == NSAlertFirstButtonReturn {
            self.save()
        } else if r == NSAlertSecondButtonReturn {
            iv.dirty.set(false);
            true
        } else {
            false
        }
    }
}

// ---- helpers ----------------------------------------------------------------

/// Re-colour the lines touched by an edit.
fn highlight_range(storage: &NSTextStorage, edited: NSRange) {
    let string = storage.string();
    let length = string.length();
    if length == 0 {
        return;
    }
    let edited = NSRange::new(edited.location.min(length), edited.length.min(length - edited.location.min(length)));
    let para = string.paragraphRangeForRange(edited);
    if para.length == 0 {
        return;
    }
    let sub = string.substringWithRange(para).to_string();
    let key = unsafe { NSForegroundColorAttributeName };
    unsafe {
        storage.addAttribute_value_range(NSParagraphStyleAttributeName, &line_style(), para);
        storage.addAttribute_value_range(key, &theme::text(), para);
        for span in highlight::scan(&sub) {
            let color = match span.kind {
                Kind::Command => theme::syn_command(),
                Kind::Math => theme::syn_math(),
                Kind::Env => theme::syn_env(),
                Kind::Comment => theme::syn_comment(),
                Kind::Brace => theme::syn_brace(),
            };
            let r = NSRange::new(para.location + span.start, span.len);
            if r.location + r.length <= para.location + para.length {
                storage.addAttribute_value_range(key, &color, r);
            }
        }
    }
}

/// The editor's line spacing (a little air between lines).
fn line_style() -> Retained<NSMutableParagraphStyle> {
    let ps = NSMutableParagraphStyle::new();
    ps.setLineSpacing(theme::LINE_SPACING);
    ps
}

/// The word inside the header pill: small, spaced-out capitals.
fn pill_text(word: &str, color: &NSColor) -> Retained<NSAttributedString> {
    theme::styled(word, &theme::mono_semibold(9.5), color, 1.4)
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// Size a label to its text and centre it vertically on `mid_y`.
fn place(label: &NSTextField, x: f64, mid_y: f64, width: f64) {
    label.sizeToFit();
    let h = label.frame().size.height;
    label.setFrame(rect(x, (mid_y - h / 2.0).round(), width, h));
}

/// Bold serif (New York on macOS), falling back to Georgia, then the system font.
fn serif_bold(size: f64) -> Retained<NSFont> {
    let base = NSFont::boldSystemFontOfSize(size);
    if let Some(desc) = base.fontDescriptor().fontDescriptorWithDesign(unsafe { NSFontDescriptorSystemDesignSerif }) {
        if let Some(f) = NSFont::fontWithDescriptor_size(&desc, size) {
            return f;
        }
    }
    NSFont::fontWithName_size(&NSString::from_str("Georgia-Bold"), size).unwrap_or(base)
}

fn content_types(exts: &[&str]) -> Retained<NSArray<UTType>> {
    let types: Vec<Retained<UTType>> = exts
        .iter()
        .filter_map(|e| UTType::typeWithFilenameExtension(&NSString::from_str(e)))
        .collect();
    NSArray::from_retained_slice(&types)
}

fn url_path(url: &NSURL) -> Option<PathBuf> {
    url.path().map(|p| PathBuf::from(p.to_string()))
}

/// Write via a temp file + rename so a crash never leaves a half-written file.
/// Save safely: write to a new, unguessable temp file beside the target
/// (create_new = O_EXCL, so a planted symlink or file with that name is never
/// followed or overwritten), flush it to disk, then rename it into place.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::{Read, Write};
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut rnd = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut rnd);
    }
    let tmp = path.with_file_name(format!(".{name}.{:016x}.typeset-saving", u64::from_le_bytes(rnd)));
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    let result = f.write_all(bytes).and_then(|_| f.sync_all()).and_then(|_| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn alert(title: &str, info: &str) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let a = NSAlert::new(mtm);
    a.setMessageText(&NSString::from_str(title));
    a.setInformativeText(&NSString::from_str(info));
    a.runModal();
}

fn menu_item(mtm: MainThreadMarker, title: &str, action: Option<Sel>, key: &str) -> Retained<NSMenuItem> {
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    }
}

fn submenu(mtm: MainThreadMarker, title: &str, items: Vec<Retained<NSMenuItem>>) -> (Retained<NSMenuItem>, Retained<NSMenu>) {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    for item in &items {
        menu.addItem(item);
    }
    let top = menu_item(mtm, title, None, "");
    top.setSubmenu(Some(&menu));
    (top, menu)
}

fn build_menu(mtm: MainThreadMarker, app: &NSApplication) -> Retained<NSMenu> {
    let sep = || NSMenuItem::separatorItem(mtm);
    let item = |t: &str, a: Sel, k: &str| menu_item(mtm, t, Some(a), k);
    let tagged = |t: &str, a: Sel, k: &str, tag: isize| {
        let i = item(t, a, k);
        i.setTag(tag);
        i
    };

    let (app_menu, _) = submenu(mtm, "Typeset", vec![
        item("About Typeset", sel!(orderFrontStandardAboutPanel:), ""),
        sep(),
        item("Hide Typeset", sel!(hide:), "h"),
        sep(),
        item("Quit Typeset", sel!(terminate:), "q"),
    ]);
    let (file_menu, _) = submenu(mtm, "File", vec![
        item("New", sel!(newDocument:), "n"),
        item("Open…", sel!(openDocument:), "o"),
        item("Import Document…", sel!(importDocument:), "I"),
        sep(),
        item("Save", sel!(saveDocument:), "s"),
        item("Save As…", sel!(saveDocumentAs:), "S"),
        item("Export PDF…", sel!(exportPDF:), "e"),
        sep(),
        item("Close", sel!(performClose:), "w"),
    ]);
    let (edit_menu, _) = submenu(mtm, "Edit", vec![
        item("Undo", sel!(undo:), "z"),
        item("Redo", sel!(redo:), "Z"),
        sep(),
        item("Cut", sel!(cut:), "x"),
        item("Copy", sel!(copy:), "c"),
        item("Paste", sel!(paste:), "v"),
        item("Paste as LaTeX", sel!(pasteAsLatex:), "V"),
        {
            let i = item("Paste & Format…", sel!(showPasteAndFormat:), "v");
            i.setKeyEquivalentModifierMask(NSEventModifierFlags::Option | NSEventModifierFlags::Command);
            i
        },
        item("Select All", sel!(selectAll:), "a"),
        sep(),
        // NSTextFinderAction: 1 = show find bar, 2 = next, 3 = previous, 12 = show replace
        tagged("Find…", sel!(performTextFinderAction:), "f", 1),
        tagged("Find and Replace…", sel!(performTextFinderAction:), "F", 12),
        tagged("Find Next", sel!(performTextFinderAction:), "g", 2),
        tagged("Find Previous", sel!(performTextFinderAction:), "G", 3),
    ]);
    let (view_menu, _) = submenu(mtm, "View", vec![
        item("Compile Now", sel!(compileNow:), "r"),
        item("Go to Error", sel!(jumpToError:), "'"),
        item("Show Cursor in Preview", sel!(showInPreview:), "j"),
        item("Show Compiler Output", sel!(showCompilerOutput:), "l"),
        item("Fix Errors…", sel!(fixErrors:), "U"),
        sep(),
        item("Zoom In Preview", sel!(zoomPreviewIn:), "="),
        item("Zoom Out Preview", sel!(zoomPreviewOut:), "-"),
        item("Fit Preview to Width", sel!(fitPreview:), "0"),
    ]);
    let (window_item, window_menu) = submenu(mtm, "Window", vec![
        item("Minimize", sel!(performMiniaturize:), "m"),
        item("Zoom", sel!(performZoom:), ""),
    ]);

    let main = NSMenu::new(mtm);
    for top in [&app_menu, &file_menu, &edit_menu, &view_menu, &window_item] {
        main.addItem(top);
    }
    app.setWindowsMenu(Some(&window_menu));
    main
}

pub fn run() {
    let mtm = MainThreadMarker::new().expect("must start on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    // Typeset is always dark (menus, alerts and panels too).
    if let Some(dark) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
        app.setAppearance(Some(&dark));
    }
    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
    drop(delegate);
}
