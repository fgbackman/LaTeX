//! Flicker-free PDF preview.
//!
//! PDFKit's PDFView renders pages asynchronously, so swapping in a new
//! document briefly shows an empty grey pane. This view instead draws the
//! pages itself, synchronously, inside `drawRect:`. When a new PDF arrives
//! the old pixels stay on screen until the new frame is completely drawn,
//! so there is never a blank frame. Pages are laid out top-to-bottom,
//! scaled to the pane's width; pinch or ⌘= / ⌘- to zoom (scroll-view magnification).

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;

use block2::RcBlock;

use objc2::rc::Retained;
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly, Message};
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSAffineTransformNSAppKitAdditions, NSBezierPath, NSBorderType, NSColor, NSEvent, NSEventModifierFlags,
    NSFontAttributeName, NSForegroundColorAttributeName, NSGraphicsContext, NSKernAttributeName, NSResponder,
    NSScrollView, NSScrollerKnobStyle, NSShadow, NSStringDrawing, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSAffineTransform, NSDictionary, NSNumber, NSObject, NSPoint, NSRect, NSRunLoop,
    NSRunLoopCommonModes, NSSize, NSString, NSTimer,
};

use crate::theme;
use objc2_pdf_kit::{PDFDisplayBox, PDFDocument, PDFPage};

/// Called with (1-based page, x, y in points from the page's top-left).
pub type SyncClick = Box<dyn Fn(u32, f64, f64)>;

const MARGIN: f64 = 28.0;
const GAP: f64 = 24.0;
/// How far a page's shadow reaches beyond its edges.
const SHADOW_REACH: f64 = 28.0;

#[derive(Default)]
pub struct CanvasIvars {
    doc: RefCell<Option<Retained<PDFDocument>>>,
    /// (page, rect in view coordinates, scale)
    layout: RefCell<Vec<(Retained<PDFPage>, NSRect, f64)>>,
    on_sync_click: RefCell<Option<SyncClick>>,
    /// Yellow flash shown by reveal(), in view coordinates.
    flash: Cell<Option<NSRect>>,
    flash_gen: Cell<u64>,
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TypesetPdfCanvas"]
    #[ivars = CanvasIvars]
    pub struct PdfCanvas;

    impl PdfCanvas {
        /// Top-to-bottom coordinates, like a document.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            true
        }

        /// The scroll view changes our width when the pane resizes; recompute
        /// the page layout and our height to match.
        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            let height = self.relayout(size.width);
            let size = NSSize::new(size.width, height.max(self.visible_height()));
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
        }

        /// Double-click or ⌘-click: jump to the source for that spot.
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let cmd = event.modifierFlags().contains(NSEventModifierFlags::Command);
            if event.clickCount() >= 2 || cmd {
                let p = self.convertPoint_fromView(event.locationInWindow(), None);
                let hit = self.ivars().layout.borrow().iter().enumerate().find_map(|(i, (_, r, s))| {
                    let inside = p.x >= r.origin.x && p.x <= r.origin.x + r.size.width
                        && p.y >= r.origin.y && p.y <= r.origin.y + r.size.height;
                    inside.then(|| (i as u32 + 1, (p.x - r.origin.x) / s, (p.y - r.origin.y) / s))
                });
                if let Some((page, x, y)) = hit {
                    if let Some(cb) = self.ivars().on_sync_click.borrow().as_ref() {
                        cb(page, x, y);
                    }
                }
                return;
            }
            let _: () = unsafe { msg_send![super(self), mouseDown: event] };
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            theme::canvas().setFill();
            NSBezierPath::fillRect(dirty);

            let Some(gc) = NSGraphicsContext::currentContext() else { return };
            let layout = self.ivars().layout.borrow();
            if layout.is_empty() {
                self.draw_empty_state();
            }
            for (page, rect, scale) in layout.iter() {
                // Include the shadow, which spills past the page.
                let reach = NSRect::new(
                    NSPoint::new(rect.origin.x - SHADOW_REACH, rect.origin.y - SHADOW_REACH),
                    NSSize::new(rect.size.width + 2.0 * SHADOW_REACH, rect.size.height + 2.0 * SHADOW_REACH),
                );
                if !intersects(reach, dirty) {
                    continue;
                }
                // The paper floats on the canvas: white sheet, soft shadow.
                gc.saveGraphicsState();
                let shadow = NSShadow::new();
                shadow.setShadowColor(Some(&NSColor::blackColor().colorWithAlphaComponent(0.6)));
                shadow.setShadowBlurRadius(18.0);
                shadow.setShadowOffset(NSSize::new(0.0, -5.0));
                shadow.set();
                NSColor::whiteColor().setFill();
                NSBezierPath::fillRect(*rect);
                gc.restoreGraphicsState();
                if !intersects(*rect, dirty) {
                    continue;
                }

                gc.saveGraphicsState();
                // PDF space is bottom-up; flip it into our top-down page rect.
                let t = NSAffineTransform::transform();
                t.translateXBy_yBy(rect.origin.x, rect.origin.y + rect.size.height);
                t.scaleXBy_yBy(*scale, -*scale);
                t.concat();
                let cg = gc.CGContext();
                unsafe {
                    page.transformContext_forBox(&cg, PDFDisplayBox::CropBox);
                    page.drawWithBox_toContext(PDFDisplayBox::CropBox, &cg);
                }
                gc.restoreGraphicsState();
            }
            if let Some(f) = self.ivars().flash.get() {
                theme::amber_alpha(0.38).setFill();
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(f, 3.0, 3.0).fill();
            }
        }
    }
);

impl PdfCanvas {
    /// A scroll view (with pinch-zoom) containing the canvas.
    pub fn new_scrollable(mtm: MainThreadMarker, frame: NSRect) -> (Retained<NSScrollView>, Retained<PdfCanvas>) {
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        scroll.setHasVerticalScroller(true);
        scroll.setHasHorizontalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setBorderType(NSBorderType::NoBorder);
        scroll.setAllowsMagnification(true);
        scroll.setMinMagnification(0.3);
        scroll.setMaxMagnification(6.0);
        scroll.setDrawsBackground(true);
        scroll.setBackgroundColor(&theme::canvas());
        scroll.setScrollerKnobStyle(NSScrollerKnobStyle::Light);

        let this = Self::alloc(mtm).set_ivars(CanvasIvars::default());
        let size = scroll.contentSize();
        let canvas: Retained<Self> =
            unsafe { msg_send![super(this), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), size)] };
        // No autoresizing: magnification changes the clip view's bounds, and an
        // autoresizing canvas would re-fit and undo the zoom. The app calls
        // fit_width() when the pane itself is resized.
        scroll.setDocumentView(Some(&canvas));
        (scroll, canvas)
    }

    /// Show a new document (or none). The previous pages stay on screen until
    /// the new ones have been drawn, so this never flashes.
    pub fn set_document(&self, doc: Option<Retained<PDFDocument>>) {
        *self.ivars().doc.borrow_mut() = doc;
        let width = self.frame().size.width;
        let height = self.relayout(width);
        let size = NSSize::new(width, height.max(self.visible_height()));
        let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
        self.setNeedsDisplay(true);
    }

    /// Match the pane width (unmagnified). Cheap no-op when unchanged.
    pub fn fit_width(&self, width: f64) {
        if (self.frame().size.width - width).abs() > 0.5 {
            self.setFrameSize(NSSize::new(width, self.frame().size.height));
            self.setNeedsDisplay(true);
        }
    }

    pub fn set_on_sync_click(&self, f: SyncClick) {
        *self.ivars().on_sync_click.borrow_mut() = Some(f);
    }

    /// Scroll to a spot (1-based page; points from the page's top-left) and
    /// flash it yellow for a moment.
    pub fn reveal(&self, page: u32, x: f64, y: f64, w: f64, h: f64) {
        let target = {
            let layout = self.ivars().layout.borrow();
            let Some((_, r, s)) = layout.get(page.saturating_sub(1) as usize) else { return };
            NSRect::new(
                NSPoint::new(r.origin.x + x * s - 3.0, r.origin.y + y * s - 2.0),
                NSSize::new(w * s + 6.0, h * s + 4.0),
            )
        };
        // Centre it vertically if possible.
        let visible = self.visibleRect();
        let pad = ((visible.size.height - target.size.height) / 2.0).max(0.0);
        self.scrollRectToVisible(NSRect::new(
            NSPoint::new(target.origin.x, target.origin.y - pad),
            NSSize::new(target.size.width, target.size.height + 2.0 * pad),
        ));
        let iv = self.ivars();
        if let Some(old) = iv.flash.get() {
            self.setNeedsDisplayInRect(old);
        }
        iv.flash.set(Some(target));
        self.setNeedsDisplayInRect(target);
        let generation = iv.flash_gen.get() + 1;
        iv.flash_gen.set(generation);
        let this = self.retain();
        let block = RcBlock::new(move |_t: NonNull<NSTimer>| {
            let iv = this.ivars();
            if iv.flash_gen.get() == generation {
                if let Some(f) = iv.flash.take() {
                    this.setNeedsDisplayInRect(f);
                }
            }
        });
        let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(1.6, false, &block) };
        unsafe { NSRunLoop::currentRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
    }

    /// Before the first PDF arrives: a quiet line of text in the middle.
    fn draw_empty_state(&self) {
        let v = self.visibleRect();
        let font = theme::mono_medium(10.5);
        let color = theme::text_faint();
        let kern = NSNumber::new_f64(1.6);
        let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName, NSKernAttributeName] };
        let values: [&AnyObject; 3] = [&font, &color, &kern];
        let attrs = NSDictionary::from_slices(&keys, &values);
        let s = NSString::from_str("RENDERING PREVIEW…");
        let size = unsafe { s.sizeWithAttributes(Some(&attrs)) };
        let at = NSPoint::new(
            (v.origin.x + (v.size.width - size.width) / 2.0).round(),
            (v.origin.y + (v.size.height - size.height) / 2.0).round(),
        );
        unsafe { s.drawAtPoint_withAttributes(at, Some(&attrs)) };
    }

    fn visible_height(&self) -> f64 {
        unsafe { self.superview() }.map(|v| v.frame().size.height).unwrap_or(0.0)
    }

    /// Lay the pages out for `width`; returns the total height needed.
    fn relayout(&self, width: f64) -> f64 {
        let iv = self.ivars();
        let mut layout = Vec::new();
        let mut y = MARGIN;
        if let Some(doc) = iv.doc.borrow().as_ref() {
            let count = unsafe { doc.pageCount() };
            let pages: Vec<(Retained<PDFPage>, NSSize)> = (0..count)
                .filter_map(|i| unsafe { doc.pageAtIndex(i) })
                .map(|p| {
                    let b = unsafe { p.boundsForBox(PDFDisplayBox::CropBox) };
                    let rotated = unsafe { p.rotation() } % 180 != 0;
                    let size = if rotated { NSSize::new(b.size.height, b.size.width) } else { b.size };
                    (p, size)
                })
                .collect();
            let widest = pages.iter().map(|(_, s)| s.width).fold(1.0_f64, f64::max);
            let scale = ((width - 2.0 * MARGIN) / widest).max(0.05);
            for (page, size) in pages {
                let w = size.width * scale;
                let h = size.height * scale;
                let x = ((width - w) / 2.0).max(MARGIN).round();
                layout.push((page, NSRect::new(NSPoint::new(x, y.round()), NSSize::new(w.round(), h.round())), scale));
                y += h + GAP;
            }
            y += MARGIN - GAP;
        }
        *iv.layout.borrow_mut() = layout;
        y
    }
}

fn intersects(a: NSRect, b: NSRect) -> bool {
    a.origin.x < b.origin.x + b.size.width
        && b.origin.x < a.origin.x + a.size.width
        && a.origin.y < b.origin.y + b.size.height
        && b.origin.y < a.origin.y + a.size.height
}
