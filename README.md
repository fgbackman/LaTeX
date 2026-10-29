# Typeset

A live LaTeX editor for macOS, written in Rust. Edit on the left; the PDF on the right updates a moment after you stop typing — no saving needed.

- **Rust all the way down**: AppKit editor + PDFKit preview through the `objc2` bindings, no Swift/Xcode project.
- **Real LaTeX**: compiles with [Tectonic](https://tectonic-typesetting.github.io) (XeTeX-based, downloads only the packages you use).
- **Locked-down compiles**: every compile runs in a separate, sandboxed process (see Security).

## Build & run

```sh
brew install tectonic          # the TeX engine
./make_app.sh install          # builds Typeset.app and copies it to /Applications
```

Or during development: `cargo run --release -- paper.tex`

The first compile downloads the LaTeX packages it needs (~30–60 s). After that, small documents update in well under a second.

## The look

A dark "ink" theme taken from the app icon — deep blue-black surfaces, cream paper, an amber cursor — with an electric-cyan accent.

- **One surface**: the title bar blends into a slim header showing the document's folder and name (an amber ● while unsaved) and a **build pill** — ● LIVE (green), BUILDING (amber), NOTICE, ERROR (red).
- **Editor**: SF Mono with a little extra line spacing, a line-number gutter, a highlight on the cursor's line, an amber caret, and syntax colours — cyan commands, lavender math, amber environments, muted comments.
- **Preview**: white pages floating on a near-black canvas with soft shadows.
- **Status bar**: monospace messages, a cyan **✦ Fix N problems** chip when fixes are available, and `Ln 12, Col 4` on the right.
- Menus, alerts and panels follow the dark theme too. In Paste & Format, the pasted document shows on light paper (so its own colours stay readable) beside the LaTeX in editor style.

## Using it

| | |
|---|---|
| ⌘N / ⌘O / ⌘S / ⇧⌘S | New, Open, Save, Save As |
| ⌘E | Export the current PDF |
| ⌘R | Compile now |
| ⌘' or click the status line | Jump to the error's line |
| ⌘L | Show the full compiler output |
| Esc | Show suggestions at the cursor |
| Double-click or ⌘-click the preview | Jump to that spot in the source |
| ⌘J | Show the cursor's line in the preview |
| ⇧⌘V | Paste as LaTeX (converts formatted text, notes, Markdown) |
| ⌥⌘V | Paste & Format window |
| ⇧⌘I | Import a Word / RTF / OpenDocument / HTML / Markdown / text file |
| ⇧⌘U or **Fix…** | Review and apply fixes for the current errors |
| ⌘F / ⇧⌘F | Find / Find and Replace |
| ⌘= ⌘- ⌘0 or pinch | Zoom preview in / out / fit width |

### Suggestions as you type (like Overleaf)

- Type `\` and a letter or two: a list of matching commands pops up under the cursor, each with a one-line explanation. Commands you've defined or already used in the document are included.
- **↑ ↓** choose · **Return** or **Tab** insert · **Esc** closes it (press **Esc** again any time to bring it back) · or just click a row.
- Commands come with their braces and the cursor in the right spot: `\frac` → `\frac{|}{}`.
- `\begin{` lists environments; picking one inserts the whole block, e.g. `itemize` with its first `\item` and the matching `\end{itemize}`. Typing `\begin{…}` by hand also adds the `\end` for you.
- `\ref{` / `\eqref{` list your `\label`s · `\cite{` lists `\bibitem`s and keys from `.bib` files next to your document · `\usepackage{` lists common packages · `\includegraphics{` and `\input{` list files in the document's folder (save the document first).
- `{` closes itself; typing `}` steps over the auto-inserted one.

### Click-to-sync

Double-click (or ⌘-click) anywhere in the preview and the editor jumps to the line that produced it. Press **⌘J** in the editor and the preview scrolls to your cursor's line and flashes it yellow. (Uses SyncTeX; positions verified against the official `synctex` tool.)

### Paste & Format

Bring in text from anywhere and get clean LaTeX:

- **⇧⌘V Paste as LaTeX** — converts what's on the clipboard and inserts it at the cursor.
- **⌥⌘V Paste & Format…** — a window: paste on the left, see (and tweak) the LaTeX on the right, then **Insert at Cursor** or **New Document**.
- **⇧⌘I Import Document…** — opens a .docx, .rtf, .odt, .html, .md or .txt file as a new LaTeX document.

It understands Word, Pages, Google Docs, TextEdit, web pages, Apple Notes and Markdown: headings become `\section`/`\subsection` (a single big first heading becomes the `\title`), **bold**/*italic*/underline/strikethrough/superscript carry over, bullet and numbered lists (nested too) become `itemize`/`enumerate`, tables become `tabular`, links become `\href`, code becomes `verbatim`, Markdown `$math$` stays math, and characters like `% $ & # _ { }` are escaped so they can't break your document. Packages the result needs (e.g. `hyperref` for links) are added to your preamble automatically. Text that is already LaTeX is pasted unchanged. Everything runs on your Mac — nothing is uploaded.

### Help with errors

The status line explains errors in plain English — e.g. *"Line 7: Undefined control sequence — \mathbb needs the amssymb package"*.

**One-click fixes.** When Typeset knows how to fix the errors, a **Fix N problems…** button appears at the right of the status line (also **⇧⌘U**, View → Fix Errors…). It lists each proposed change with a checkbox — untick anything you don't want, then **Apply Fixes**; a single **⌘Z** undoes them all. Nothing changes without your approval, and a fix is skipped if that part of the document changed since the compile. It handles:
- text before `\begin{document}` (moved into the body), or bare text with no `\documentclass` (made into a complete document)
- missing `\begin{document}` / `\end{document}`, environments left open
- misspelled commands (`\secton` → `\section`)
- commands/environments whose package isn't loaded (adds the `\usepackage`)
- `^`, `_`, `\alpha` outside math (wrapped in `$…$`; underscores in names like `file_name` escaped)
- a stray `&` or `#` in text (escaped), a `\\` with no line to end (removed)
- a package that doesn't exist (offers to remove the line — unticked by default)

Tested end-to-end with a real TeX engine: broken documents compile cleanly after one or two rounds of fixes.

- With errors, TeX keeps going and the preview still updates; the status line shows the first error in red.
- If the PDF can't be produced at all, the last good preview stays up.
- `\input`, `\include` and `\includegraphics` resolve against the saved file's folder.
- The preview keeps your page, scroll position and zoom across recompiles, and never blanks out: Typeset draws the pages itself, so the old page stays up until the new one is fully drawn. When an edit doesn't change the output (e.g. a comment), the preview isn't touched at all.

## Security

Every document, paste and import is treated as hostile. LaTeX is a programming language: a document can try to read files (`\input{/path}`), write files anywhere (`\openout` with an absolute or `../` path — Tectonic writes outputs to `outdir.join(name)`), run programs (`\write18`), or hang the machine.

**Compiling**
1. **Tectonic `--untrusted`**: shell escape off.
2. **A disposable workspace per document** in a private (0700) temp folder: its own copy-on-write clone of the package cache, its own empty `$HOME` (no user config is read) and its own output folder. The engine can write *only* there, so a malicious document can at most damage its own throwaway copy; it's deleted when you open another document. The shared package cache is written only by a trusted warm-up that compiles a fixed built-in document.
3. **macOS sandbox, fail-closed** (no sandbox → no compile): no forking, no running any other program, no Apple Events, no clipboard or LaunchServices.
   - **No network at all** — no connections, no DNS, no system download/certificate helpers. Documents compile entirely offline from the cache. When one needs a package that isn't downloaded yet, Typeset fetches it in a separate trusted step (our own TeX that only opens validated file names — no document code runs with network access), then recompiles offline.
   - **From the document's folder, only LaTeX-type files**: sources, bibliographies, images, fonts, data and code listings. Never hidden files or folders (`.git`, `.env`…) and never keys, certificates or `.json`/`.yaml`/`.toml`/database files.
   - Nothing else in your home folder — never keychains, SSH/GPG keys, cloud credentials, shell history and rc files, browser, Mail, Messages or app data. Other apps' temp files are off-limits. A document saved directly in your home folder gets no folder access at all.
4. **Limits**: CPU time, max file size, no core dumps, 3-minute wall clock, output size caps; a new keystroke kills a stale compile.
5. **Clean engine environment** and a **verified engine**: Typeset only runs `tectonic` from standard locations and refuses one that other users could modify.
6. **Outputs read defensively**: no symlinks, hard links or special files; size-capped; SyncTeX decompression capped (zip-bomb safe).

**Pasting & importing**
- HTML (Google Docs, web pages) is sanitised before macOS reads it: only formatting tags survive — no scripts, frames, images, stylesheets or anything that could load remote content.
- Links become clickable only for `http`, `https` and `mailto` (no `javascript:`, `file:` or app URLs).
- Markdown `$math$` passes through only if it uses known math commands; anything else (e.g. `\input`, `\write`, `\def`, `^^` tricks) is shown as text.
- Hidden control and text-direction ("Trojan Source") characters are removed; opening a `.tex` containing them shows a warning.
- Pasting raw LaTeX that reads/writes files shows a warning. Legacy `.doc` and `.webarchive` aren't imported. Size limits on everything.

**Saving**: written to an unguessable temp file opened with `O_EXCL` (a planted symlink can't redirect it), flushed, then renamed into place.

**Verify on your Mac**: `/Applications/Typeset.app/Contents/MacOS/Typeset --security-check` runs hostile documents through the real compiler and probes every sandbox rule (with harmless decoy files, cleaned up afterwards). Status bar shows the sandbox level after each compile.

**Tested**: unit tests for every defence, 200,000 fuzzed inputs against the parsers and sanitiser, `cargo audit` (0 known vulnerabilities in dependencies). Builds use `--locked` dependency versions.

**Residual risks** (no software is unbreakable): the PDF preview and rich-text import use Apple's PDFKit/AppKit inside the app (same exposure as Preview/TextEdit — keep macOS updated). A bug in Tectonic's C engine could give code execution *inside* the sandbox — where it can read only LaTeX-type files in the document's own folder and has no network, so the only place anything could end up is the PDF you see.

## Layout

```
src/main.rs       entry point
src/app.rs        AppKit/PDFKit UI, menus, file handling, debounce
src/theme.rs      the look: colours, fonts, themed building blocks
src/compile.rs    sandboxed Tectonic worker, cancellation, error parsing
src/highlight.rs  LaTeX syntax colouring (pure Rust, unit-tested)
src/preview.rs    flicker-free PDF preview (draws pages itself; pinch-zoom)
src/sync.rs       SyncTeX reader: PDF spot ↔ source line (unit-tested)
src/convert.rs    documents/notes/Markdown → LaTeX (unit-tested; output compiled with XeTeX)
src/paste.rs      reads clipboard & files via AppKit for convert.rs
src/editor.rs     text view: line numbers, current line, suggestion popup, auto-closing braces, \end insertion
src/complete.rs   what to suggest and what to insert (pure Rust, unit-tested)
src/hints.rs      plain-English error help and missing-package fixes (unit-tested)
src/selfcheck.rs  Typeset --security-check: attacks the installed app on this Mac
make_app.sh       builds and signs Typeset.app (with icon)
icon/             app icon: make_icon.py draws icon_1024.png; Typeset.icns fallback
```

`cargo test` runs the scanner and compiler-worker tests on any OS.
