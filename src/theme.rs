//! Typeset's look: a dark "ink" theme taken from the app icon (deep ink
//! blue-black, cream, an amber cursor) with an electric-cyan accent.
//! Every colour and font the UI uses lives here.

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, AllocAnyThread, MainThreadOnly};
use objc2_app_kit::{
    NSBox, NSBoxType, NSColor, NSFont, NSFontAttributeName, NSFontWeightMedium, NSFontWeightRegular,
    NSFontWeightSemibold, NSForegroundColorAttributeName, NSKernAttributeName, NSResponder, NSSplitView,
    NSTextField, NSTitlePosition, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSDictionary, NSMutableAttributedString, NSNumber, NSObject, NSRect,
    NSString,
};

fn hex(rgb: u32, alpha: f64) -> Retained<NSColor> {
    let c = |shift: u32| ((rgb >> shift) & 0xff) as f64 / 255.0;
    NSColor::colorWithSRGBRed_green_blue_alpha(c(16), c(8), c(0), alpha)
}

// ---- surfaces -----------------------------------------------------------------
/// Window, header and status bar.
pub fn chrome() -> Retained<NSColor> { hex(0x0A0D14, 1.0) }
/// The editor.
pub fn editor_bg() -> Retained<NSColor> { hex(0x0E121B, 1.0) }
/// The row the cursor is on.
pub fn current_line() -> Retained<NSColor> { hex(0x151B28, 1.0) }
/// Behind the PDF pages.
pub fn canvas() -> Retained<NSColor> { hex(0x06080D, 1.0) }
/// 1-px separators.
pub fn hairline() -> Retained<NSColor> { hex(0x1C2331, 1.0) }
/// Small raised surfaces (status pill, suggestion list).
pub fn raised() -> Retained<NSColor> { hex(0x121826, 1.0) }
/// The Paste & Format source pane: the icon's cream paper.
pub fn paper() -> Retained<NSColor> { hex(0xF7F2E8, 1.0) }
pub fn raised_border() -> Retained<NSColor> { hex(0x252E40, 1.0) }

// ---- text ---------------------------------------------------------------------
pub fn text() -> Retained<NSColor> { hex(0xD6DDE9, 1.0) }
pub fn text_dim() -> Retained<NSColor> { hex(0x8693A9, 1.0) }
pub fn text_faint() -> Retained<NSColor> { hex(0x465166, 1.0) }
pub fn line_number() -> Retained<NSColor> { hex(0x394357, 1.0) }
pub fn line_number_current() -> Retained<NSColor> { hex(0xB3BDCF, 1.0) }

// ---- accents ------------------------------------------------------------------
pub fn accent() -> Retained<NSColor> { hex(0x5CCFE6, 1.0) }
pub fn accent_alpha(a: f64) -> Retained<NSColor> { hex(0x5CCFE6, a) }
/// The icon's cursor colour.
pub fn amber() -> Retained<NSColor> { hex(0xF0AA3E, 1.0) }
pub fn amber_alpha(a: f64) -> Retained<NSColor> { hex(0xF0AA3E, a) }
pub fn green() -> Retained<NSColor> { hex(0x3DDC97, 1.0) }
pub fn red() -> Retained<NSColor> { hex(0xFF5C70, 1.0) }
pub fn red_text() -> Retained<NSColor> { hex(0xFF7A8A, 1.0) }

// ---- syntax -------------------------------------------------------------------
pub fn syn_command() -> Retained<NSColor> { hex(0x5CCFE6, 1.0) }
pub fn syn_math() -> Retained<NSColor> { hex(0xC3A6FF, 1.0) }
pub fn syn_env() -> Retained<NSColor> { hex(0xF0AA3E, 1.0) }
pub fn syn_comment() -> Retained<NSColor> { hex(0x56627A, 1.0) }
pub fn syn_brace() -> Retained<NSColor> { hex(0x7C879B, 1.0) }

// ---- fonts --------------------------------------------------------------------
pub fn mono(size: f64) -> Retained<NSFont> {
    NSFont::monospacedSystemFontOfSize_weight(size, unsafe { NSFontWeightRegular })
}
pub fn mono_medium(size: f64) -> Retained<NSFont> {
    NSFont::monospacedSystemFontOfSize_weight(size, unsafe { NSFontWeightMedium })
}
pub fn mono_semibold(size: f64) -> Retained<NSFont> {
    NSFont::monospacedSystemFontOfSize_weight(size, unsafe { NSFontWeightSemibold })
}

/// Editor font size and the extra space under each line.
pub const EDITOR_SIZE: f64 = 13.0;
pub const LINE_SPACING: f64 = 3.0;

// ---- building blocks ------------------------------------------------------------

/// Text with a font, colour and letter-spacing.
pub fn styled(s: &str, font: &NSFont, color: &NSColor, kern: f64) -> Retained<NSAttributedString> {
    let kern = NSNumber::new_f64(kern);
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName, NSKernAttributeName] };
    let values: [&AnyObject; 3] = [font, color, &kern];
    let attrs = NSDictionary::from_slices(&keys, &values);
    unsafe { NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &NSString::from_str(s), Some(&attrs)) }
}

/// Several styled runs joined together.
pub fn join(parts: &[Retained<NSAttributedString>]) -> Retained<NSAttributedString> {
    let out = NSMutableAttributedString::new();
    for p in parts {
        out.appendAttributedString(p);
    }
    Retained::into_super(out)
}

/// A flat rectangle (optionally rounded and outlined) — backgrounds, chips, dots.
pub fn surface(mtm: MainThreadMarker, frame: NSRect, fill: &NSColor, border: Option<&NSColor>, radius: f64) -> Retained<NSBox> {
    let b = NSBox::initWithFrame(NSBox::alloc(mtm), frame);
    b.setBoxType(NSBoxType::Custom);
    b.setTitlePosition(NSTitlePosition::NoTitle);
    b.setFillColor(fill);
    match border {
        Some(c) => {
            b.setBorderColor(c);
            b.setBorderWidth(1.0);
        }
        None => b.setBorderWidth(0.0),
    }
    b.setCornerRadius(radius);
    b.setContentViewMargins(objc2_foundation::NSSize::new(0.0, 0.0));
    b
}

/// A non-editable, non-selectable label showing styled text.
pub fn label(mtm: MainThreadMarker, frame: NSRect, text: &NSAttributedString) -> Retained<NSTextField> {
    let l = NSTextField::labelWithAttributedString(text, mtm);
    l.setFrame(frame);
    l.setSelectable(false);
    l
}

define_class!(
    /// A split view with a 1-px divider in the theme's hairline colour.
    #[unsafe(super(NSSplitView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "TypesetSplitView"]
    pub struct ThemedSplit;

    impl ThemedSplit {
        #[unsafe(method_id(dividerColor))]
        fn divider_color(&self) -> Retained<NSColor> {
            hairline()
        }

        #[unsafe(method(dividerThickness))]
        fn divider_thickness(&self) -> f64 {
            1.0
        }
    }
);

impl ThemedSplit {
    pub fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}
