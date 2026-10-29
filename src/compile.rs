//! Background LaTeX compiler — the security boundary.
//!
//! Every document is treated as hostile. TeX is a programming language: a
//! document can try to read any file (`\input{/path}`), write any file
//! (`\openout` with an absolute path — Tectonic writes outputs to
//! `outdir.join(name)`, so absolute names escape the output folder), run
//! programs (`\write18`), or hang the machine.
//!
//! Defences, in layers:
//!  1. Tectonic `--untrusted` (+ `TECTONIC_UNTRUSTED_MODE=1`): no shell escape.
//!  2. A disposable per-document *workspace* in a private 0700 folder: its own
//!     copy-on-write clone of the package cache, its own empty HOME (so no user
//!     config is read) and its own output folder. The engine may write only
//!     there, so a malicious document can at most damage its own throwaway
//!     copy — never the shared cache, never anything else. The shared cache
//!     is written only by a *trusted warm-up* that compiles a fixed document.
//!  3. macOS sandbox (`sandbox-exec`, fail-closed): no fork, no exec of any
//!     other program, no Apple Events, no clipboard / LaunchServices, no
//!     listening sockets; reads of the home folder limited to the document's
//!     own folder (never keychains, SSH keys, shell history, browser or mail
//!     data, credentials…), reads of other temp files blocked.
//!  4. Resource limits: CPU time, file size, no core dumps, wall-clock limit,
//!     output-size caps, and outputs read without following symlinks.
//!  5. A clean environment and a verified engine binary.

use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const TIME_LIMIT: Duration = Duration::from_secs(180);
const WARMUP_LIMIT: Duration = Duration::from_secs(600);
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
/// Largest PDF we'll load (bytes).
pub const MAX_PDF: u64 = 300 * 1024 * 1024;
/// Largest compressed SyncTeX file we'll read.
const MAX_SYNC_GZ: u64 = 64 * 1024 * 1024;
/// Largest file the engine may write (RLIMIT_FSIZE).
const MAX_WRITE: u64 = 1024 * 1024 * 1024;
/// CPU seconds before the kernel stops the engine (backstop for TIME_LIMIT).
const MAX_CPU_SECS: u64 = 300;

/// Paths under $HOME the engine must never read, even when a document lives
/// next to them. (Relative to $HOME.)
const SENSITIVE: &[&str] = &[
    ".ssh", ".gnupg", ".aws", ".azure", ".config", ".kube", ".docker", ".netrc",
    ".git-credentials", ".gitconfig", ".npmrc", ".pypirc", ".cargo/credentials",
    ".cargo/credentials.toml", ".password-store", ".zsh_history", ".bash_history",
    ".zshrc", ".zprofile", ".zshenv", ".bashrc", ".bash_profile", ".profile",
    "Library/Keychains", "Library/Cookies", "Library/Mail", "Library/Messages",
    "Library/Safari", "Library/Application Support", "Library/Containers",
    "Library/Group Containers", "Library/Accounts", "Library/Calendars",
    "Library/IdentityServices", "Library/Preferences", "Library/Caches",
    "Library/HTTPStorages", "Library/Logs", "Library/Suggestions",
];

/// How strict the sandbox is. The app picks the strictest one that compiles
/// a fixed test document on this Mac (never weaker than `Basic`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Everything below, plus: no clipboard/LaunchServices/Apple-Event
    /// services, no listening sockets.
    Strict,
    /// File, exec, fork and Apple-Event rules only.
    Reduced,
    /// File and fork rules only (exec of other programs not blocked).
    Basic,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::Strict => "sandboxed",
            Tier::Reduced => "sandboxed (reduced)",
            Tier::Basic => "sandboxed (basic)",
        }
    }
}

/// Build the SBPL profile. Paths arrive as -D parameters (never spliced into
/// the text), so odd characters in folder names can't change the rules.
/// Later rules win over earlier ones.
/// File types LaTeX legitimately reads from a document's folder: sources,
/// bibliographies, images, data and code listings. Everything else there
/// (keys, certificates, .env/.json/.yaml/.toml configs, databases…) is off-limits.
pub(crate) const PROJECT_EXTS: &[&str] = &[
    "tex", "ltx", "sty", "cls", "clo", "cfg", "def", "fd", "bib", "bst", "bbl", "bbx", "cbx", "lbx",
    "aux", "toc", "lof", "lot", "out", "idx", "ind", "ilg", "gls", "glo", "ist", "nlo", "nls", "tikz",
    "pgf", "asy", "mp", "mps", "png", "jpg", "jpeg", "pdf", "eps", "ps", "svg", "tif", "tiff", "gif",
    "bmp", "csv", "tsv", "dat", "txt", "md", "py", "c", "h", "cpp", "hpp", "cc", "rs", "go", "java",
    "js", "ts", "m", "r", "jl", "sh", "rb", "swift", "kt", "cs", "sql", "html", "css", "xml", "asm", "s",
    "otf", "ttf", "ttc", "pfb", "afm", "tfm", "vf", "enc", "map", "dtx", "ins",
];

/// `\.(tex|...)$` with each extension matched case-insensitively.
pub(crate) fn project_ext_regex() -> String {
    let alts: Vec<String> = PROJECT_EXTS
        .iter()
        .map(|e| e.chars().map(|c| if c.is_ascii_alphabetic() { format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase()) } else { c.to_string() }).collect())
        .collect();
    format!("\\.({})$", alts.join("|"))
}

fn profile(tier: Tier, trusted: bool) -> String {
    let mut p = String::from("(version 1)\n(allow default)\n(deny process-fork)\n(deny user-preference-write)\n");
    if !trusted {
        // Documents get no network at all: nothing they read can be sent
        // anywhere. (Missing packages are fetched by a separate trusted step.)
        match tier {
            Tier::Strict | Tier::Reduced => p.push_str("(deny network*)\n"),
            Tier::Basic => p.push_str("(deny network-outbound (remote ip \"*:*\"))\n(deny network-bind network-inbound)\n"),
        }
        if tier == Tier::Strict {
            // …nor via system helpers that make network requests for you (DNS, downloads, certificate checks).
            p.push_str("(deny mach-lookup (global-name \"com.apple.dnssd.service\") (global-name \"com.apple.nsurlsessiond\") (global-name \"com.apple.trustd\") (global-name \"com.apple.trustd.agent\"))\n");
        }
    }
    if tier != Tier::Basic {
        p.push_str("(deny process-exec*)\n(allow process-exec* (literal (param \"ENGINE\")))\n(deny appleevent-send)\n");
    }
    if tier == Tier::Strict {
        p.push_str(
            "(deny mach-lookup\n    (global-name \"com.apple.pasteboard.1\")\n    (global-name \"com.apple.coreservices.launchservicesd\")\n    (global-name \"com.apple.coreservices.appleevents\"))\n(deny network-bind network-inbound)\n",
        );
    }
    // Reads: nothing in $HOME, external volumes or other temp files…
    p.push_str("(deny file-read-data (subpath (param \"HOME\")) (subpath \"/Volumes\") (subpath (param \"TMPROOT\")))\n");
    // …except what the compile needs: its workspace, fonts, the engine…
    p.push_str("(allow file-read-data (subpath (param \"WORK\")) (subpath (param \"FONTS\")) (subpath (param \"ENGINE_DIR\")))\n");
    // …and from the document's folder only LaTeX-type files and folder listings…
    p.push_str(&format!(
        "(allow file-read-data (require-all (subpath (param \"PROJECT\")) (regex #\"{}\")))\n",
        project_ext_regex()
    ));
    p.push_str("(allow file-read-data (require-all (subpath (param \"PROJECT\")) (vnode-type DIRECTORY)))\n");
    // …never hidden files or folders there (.git, .env, .ssh…)…
    p.push_str("(deny file-read-data (require-all (subpath (param \"PROJECT\")) (regex #\"/\\.\")))\n");
    // …and never secrets, even if the document sits next to them.
    p.push_str("(deny file-read-data");
    for i in 0..SENSITIVE.len() {
        p.push_str(&format!(" (subpath (param \"S{i}\"))"));
    }
    p.push_str(")\n");
    // Writes: only the workspace (+ the shared cache for the trusted warm-up).
    p.push_str("(deny file-write*)\n(allow file-write* (subpath (param \"WORK\")) (literal \"/dev/null\") (literal \"/dev/tty\") (literal \"/dev/dtracehelper\"))\n");
    if trusted {
        p.push_str("(allow file-read-data (subpath (param \"CACHE\")))\n(allow file-write* (subpath (param \"CACHE\")))\n");
    }
    p
}

/// A disposable, private place for one document's compiles.
#[derive(Debug, Clone)]
pub struct Workspace {
    pub dir: PathBuf,
    pub cache: PathBuf,
    pub home: PathBuf,
    pub tmp: PathBuf,
    pub out: PathBuf,
}

impl Workspace {
    fn create(dir: PathBuf) -> std::io::Result<Workspace> {
        use std::os::unix::fs::DirBuilderExt;
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?; // does not follow symlinks
        }
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let ws = Workspace {
            cache: dir.join("cache"),
            home: dir.join("home"),
            tmp: dir.join("tmp"),
            out: dir.join("out"),
            dir,
        };
        for d in [&ws.home, &ws.tmp, &ws.out] {
            std::fs::DirBuilder::new().mode(0o700).create(d)?;
        }
        Ok(ws)
    }
}

pub struct Job {
    pub generation: u64,
    pub source: String,
    /// Folder `\input`/`\includegraphics` resolve against (the .tex file's folder).
    pub workdir: PathBuf,
    /// Changes whenever a different document is opened; a new workspace is made.
    pub doc_id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TexError {
    pub message: String,
    /// 1-based line in the edited document, when the error is in it.
    pub line: Option<usize>,
}

#[derive(Debug)]
pub enum Outcome {
    /// A PDF was produced. `errors` is non-empty when TeX recovered from errors.
    Pdf {
        pdf: Vec<u8>,
        elapsed: Duration,
        errors: Vec<TexError>,
        tier: Tier,
        log: String,
        sync: Option<crate::sync::SyncIndex>,
        /// Something the user should know (e.g. why files beside the document can't be read).
        note: Option<String>,
    },
    /// No PDF.
    Failed { errors: Vec<TexError>, log: String },
    /// Tectonic isn't installed (or its install looks unsafe — see the message).
    EngineMissing(String),
    /// A newer edit superseded this compile.
    Cancelled,
}

pub struct Done {
    pub generation: u64,
    pub outcome: Outcome,
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/nonexistent"))
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
pub(crate) static TEST_CACHE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);
/// Tests that change TEST_CACHE hold this so they don't run at the same time.
#[cfg(test)]
pub(crate) static TEST_CACHE_USER: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The shared package cache, written only by the trusted warm-up and fetches.
pub fn cache_dir() -> PathBuf {
    #[cfg(test)]
    if let Some(p) = TEST_CACHE.lock().unwrap().clone() {
        return p;
    }
    home_dir().join("Library/Caches/Typeset/tectonic")
}

/// Find the `tectonic` binary in the usual install locations (not $PATH,
/// which could point anywhere) and make sure nobody else could have swapped it.
pub fn find_tectonic() -> Result<PathBuf, String> {
    let candidates = [
        PathBuf::from("/opt/homebrew/bin/tectonic"),
        PathBuf::from("/usr/local/bin/tectonic"),
        PathBuf::from("/opt/local/bin/tectonic"),
        home_dir().join(".cargo/bin/tectonic"),
    ];
    let Some(found) = candidates.iter().find(|p| p.is_file()) else {
        return Err("Tectonic isn't installed. In Terminal run:  brew install tectonic".into());
    };
    let real = found.canonicalize().map_err(|e| e.to_string())?;
    check_trustworthy(&real)?;
    Ok(real)
}

/// The engine and its folder must be owned by you or root and not writable
/// by anyone else; otherwise another account could replace it.
fn check_trustworthy(binary: &Path) -> Result<(), String> {
    let me = unsafe { libc::getuid() };
    for p in [Some(binary), binary.parent()].into_iter().flatten() {
        let m = std::fs::metadata(p).map_err(|e| e.to_string())?;
        if (m.uid() != me && m.uid() != 0) || m.mode() & 0o022 != 0 {
            return Err(format!(
                "Refusing to run Tectonic: {} can be changed by other users. Fix with:  chmod go-w \"{}\"",
                p.display(),
                p.display()
            ));
        }
    }
    let m = std::fs::metadata(binary).map_err(|e| e.to_string())?;
    if m.mode() & 0o111 == 0 {
        return Err(format!("{} isn't executable", binary.display()));
    }
    Ok(())
}

/// Create a private (0700) build folder in the system temp dir.
pub fn make_build_dir() -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let base = canon(&std::env::temp_dir());
    let mut rnd = [0u8; 8];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut rnd);
    }
    let dir = base.join(format!("typeset-{}-{}", std::process::id(), u64::from_le_bytes(rnd)));
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?; // fails if it already exists
    Ok(dir)
}

/// The document's folder, if it's safe to let the engine read it. A document
/// directly in $HOME (or /) would otherwise expose the whole home folder.
pub fn safe_project_dir(dir: &Path, home: &Path) -> Option<PathBuf> {
    let c = dir.canonicalize().ok()?;
    if !c.is_dir() || c == Path::new("/") {
        return None;
    }
    let home = canon(home);
    if home.starts_with(&c) {
        return None; // $HOME itself or one of its parents
    }
    if c.starts_with("/Volumes") && c.components().count() <= 3 {
        return None; // the root of a drive
    }
    Some(c)
}

/// Seconds since 1970, rounded down to the hour.
fn hour_epoch() -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now - now % 3600
}

/// Read a file the engine produced: no symlinks, no hard links to other
/// files, regular files only, size-capped.
fn read_output(path: &Path, max: u64) -> Option<Vec<u8>> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let m = f.metadata().ok()?;
    if !m.is_file() || m.nlink() != 1 || m.len() > max || m.len() == 0 {
        return None;
    }
    let mut buf = Vec::with_capacity(m.len() as usize);
    f.take(max).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Everything the sandbox profile refers to.
struct Params {
    engine: PathBuf,
    work: PathBuf,
    project: PathBuf,
    cache: PathBuf,
}

fn sandbox_args(p: &Params) -> Vec<String> {
    let home = canon(&home_dir());
    let engine_dir = p.engine.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/nonexistent"));
    let mut v: Vec<(String, PathBuf)> = vec![
        ("ENGINE".into(), p.engine.clone()),
        ("HOME".into(), home.clone()),
        ("TMPROOT".into(), canon(&std::env::temp_dir())),
        // No document folder (unsaved, or unsafe to expose): point at nothing.
        ("PROJECT".into(), if p.project == p.work { PathBuf::from("/var/empty/typeset-no-project") } else { p.project.clone() }),
        ("WORK".into(), canon(&p.work)),
        ("FONTS".into(), home.join("Library/Fonts")),
        ("ENGINE_DIR".into(), engine_dir),
        ("CACHE".into(), canon(&p.cache)),
    ];
    for (i, rel) in SENSITIVE.iter().enumerate() {
        v.push((format!("S{i}"), home.join(rel)));
    }
    let mut args = Vec::new();
    for (k, val) in v {
        args.push("-D".into());
        args.push(format!("{k}={}", val.display()));
    }
    args
}

/// A command that runs the engine inside the sandbox, with a clean
/// environment and resource limits.
fn engine_command(tier: Tier, trusted: bool, p: &Params, ws_home: &Path, ws_tmp: &Path, cache: &Path) -> Command {
    let mut cmd = if cfg!(test) {
        // Unit tests run on Linux with a fake engine; there is no sandbox-exec.
        Command::new(&p.engine)
    } else {
        let mut c = Command::new(SANDBOX_EXEC);
        c.args(sandbox_args(p)).arg("-p").arg(profile(tier, trusted)).arg(&p.engine);
        c
    };
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", ws_home) // no user config; nothing persists outside the workspace
        .env("TMPDIR", ws_tmp)
        .env("LANG", "en_US.UTF-8")
        .env("TECTONIC_UNTRUSTED_MODE", "1")
        .env("TECTONIC_CACHE_DIR", cache)
        // Pin the PDF's timestamp to the hour so identical input gives a
        // byte-identical PDF (the app then skips redrawing). \today is unaffected.
        .env("SOURCE_DATE_EPOCH", hour_epoch().to_string());
    unsafe {
        cmd.pre_exec(|| {
            let lim = |res, soft: u64, hard: u64| {
                let r = libc::rlimit { rlim_cur: soft as libc::rlim_t, rlim_max: hard as libc::rlim_t };
                libc::setrlimit(res, &r);
            };
            lim(libc::RLIMIT_FSIZE, MAX_WRITE, MAX_WRITE);
            lim(libc::RLIMIT_CPU, MAX_CPU_SECS, MAX_CPU_SECS + 10);
            lim(libc::RLIMIT_CORE, 0, 0);
            Ok(())
        });
    }
    cmd
}

/// Run any program under exactly the sandbox a document compile gets (used
/// by the security self-check to probe the rules directly).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn probe_command(tier: Tier, program: &Path, ws: &Workspace, project: &Path) -> Command {
    let params = Params { engine: canon(program), work: ws.dir.clone(), project: project.to_path_buf(), cache: ws.cache.clone() };
    engine_command(tier, false, &params, &ws.home, &ws.tmp, &ws.cache)
}

/// Can the sandbox be used at all? (Fail closed if not.)
fn sandbox_available() -> bool {
    cfg!(test) || Path::new(SANDBOX_EXEC).is_file()
}

/// A fixed, harmless document used for the warm-up and the sandbox self-test.
const WARMUP_TEX: &str = r"\documentclass{article}
\usepackage{amsmath,amssymb,amsthm,mathtools}
\usepackage{graphicx,xcolor,geometry,booktabs,enumitem,float,caption,fancyhdr,url,listings}
\usepackage[normalem]{ulem}
\usepackage{tikz}
\usepackage{hyperref}
\begin{document}
Warm-up $e^{i\pi}+1=0$ \textbf{bold} \emph{it} \texttt{tt} \sout{x}.
\end{document}
";

const SELFTEST_TEX: &str = "\\documentclass{article}\\begin{document}ok\\end{document}\n";

/// Fill the shared cache by compiling WARMUP_TEX (trusted: our own text).
/// Skipped when the cache was already warmed for this exact engine binary.
pub(crate) fn warm_up(engine: &Path, build: &Path) -> bool {
    warm_up_with(engine, build, false)
}

fn warm_up_with(engine: &Path, build: &Path, force: bool) -> bool {
    let cache = cache_dir();
    let _ = std::fs::create_dir_all(&cache);
    let marker = cache.join(".typeset-ready");
    let expected = std::fs::metadata(engine)
        .map(|m| format!("{}|{}|{}", engine.display(), m.len(), m.mtime()))
        .unwrap_or_default();
    if !force && std::fs::read_to_string(&marker).ok().as_deref() == Some(expected.as_str()) {
        return true;
    }
    let Ok(ws) = Workspace::create(build.join("warmup")) else { return false };
    let params = Params { engine: engine.to_path_buf(), work: ws.dir.clone(), project: ws.dir.clone(), cache: cache.clone() };
    let mut cmd = engine_command(Tier::Basic, true, &params, &ws.home, &ws.tmp, &cache);
    cmd.args(["-X", "compile", "-", "--untrusted", "--outdir"])
        .arg(&ws.out)
        .current_dir(&ws.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let ok = run_to_end(cmd, WARMUP_TEX, WARMUP_LIMIT) && read_output(&ws.out.join("texput.pdf"), MAX_PDF).is_some();
    if ok {
        let _ = std::fs::write(&marker, expected);
    }
    let _ = std::fs::remove_dir_all(&ws.dir);
    ok
}

/// Run a command to completion feeding `input`; false on failure/timeout.
fn run_to_end(mut cmd: Command, input: &str, limit: Duration) -> bool {
    let Ok(mut child) = cmd.spawn() else { return false };
    if let Some(mut stdin) = child.stdin.take() {
        let src = input.as_bytes().to_vec();
        thread::spawn(move || {
            let _ = stdin.write_all(&src);
        });
    }
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true, // the caller checks for output
            Ok(None) => {}
            Err(_) => return false,
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Make a fresh workspace for a document, with a copy-on-write clone of the
/// shared cache (APFS clones are instant and share no writable data).
pub(crate) fn new_workspace(build: &Path, doc_id: u64) -> std::io::Result<Workspace> {
    // Throw away every older workspace first.
    if let Ok(rd) = std::fs::read_dir(build) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with("doc-") {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let ws = Workspace::create(build.join(format!("doc-{doc_id}")))?;
    let shared = cache_dir();
    if shared.is_dir() {
        let src = format!("{}/.", shared.display());
        let cloned = Command::new("/bin/cp").args(["-c", "-R"]).arg(&src).arg(&ws.cache).stderr(Stdio::null()).status();
        if !cloned.map(|s| s.success()).unwrap_or(false) {
            let _ = std::fs::remove_dir_all(&ws.cache);
            let _ = Command::new("/bin/cp").args(["-R"]).arg(&src).arg(&ws.cache).stderr(Stdio::null()).status();
        }
    }
    let _ = std::fs::create_dir_all(&ws.cache);
    Ok(ws)
}

/// A file name that is safe to fetch from the package bundle.
pub(crate) fn valid_package_name(n: &str) -> bool {
    let b = n.as_bytes();
    !b.is_empty()
        && b.len() <= 80
        && b[0].is_ascii_alphanumeric()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || b"._+-".contains(c))
        && !n.contains("..")
}

/// Files the last compile wanted but the offline cache didn't have, read from
/// Tectonic's errors and the TeX log. (Attacker-influenced text: every name
/// is validated, and fetching can only ever download genuine bundle files.)
pub(crate) fn missing_files(log: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |n: &str| {
        let n = n.trim().trim_matches('"');
        if valid_package_name(n) && !out.iter().any(|x| x == n) {
            out.push(n.to_string());
        }
    };
    let between = |s: &str, a: &str, b: &str| -> Vec<String> {
        let mut v = Vec::new();
        let mut rest = s;
        while let Some(i) = rest.find(a) {
            let after = &rest[i + a.len()..];
            match after.find(b) {
                Some(j) => {
                    v.push(after[..j].to_string());
                    rest = &after[j..];
                }
                None => break,
            }
        }
        v
    };
    // Classic LaTeX uses `name', LaTeX3 (\file_input:n etc.) uses 'name'.
    for n in between(log, "File `", "' not found").into_iter().chain(between(log, "File '", "' not found")) {
        add(&n);
        // e.g. lipsum.ltd → lipsum.ltd.tex (but not foo.sty → foo.sty.tex)
        const TEX_EXTS: &[&str] = &["tex", "sty", "cls", "clo", "cfg", "def", "fd", "ldf", "bib", "bst", "dtx", "ins", "ltx", "code.tex"];
        let ext = n.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        if !TEX_EXTS.contains(&ext) {
            add(&format!("{n}.tex"));
        }
    }
    for n in between(log, "I can't find file `", "'") {
        add(&n);
        if !n.contains('.') {
            add(&format!("{n}.tex"));
        }
    }
    // "! Font \x="[lmroman10-regular]:mapping=tex-text" at 10pt not loadable" / "Font \x=cmr10 not loadable"
    for spec in between(log, "=", " not loadable") {
        let spec = spec.split(" at ").next().unwrap_or("").trim().trim_matches('"');
        let name = spec.trim_start_matches('[').split([']', ':', '/']).next().unwrap_or("");
        if !name.is_empty() {
            add(name);
            if !name.contains('.') {
                add(&format!("{name}.tfm"));
                add(&format!("{name}.otf"));
            }
        }
    }
    // "Font shape `TU/lmr/m/n' undefined" → tulmr.fd
    for shape in between(log, "Font shape `", "' undefined") {
        let mut parts = shape.split('/');
        if let (Some(enc), Some(fam)) = (parts.next(), parts.next()) {
            add(&format!("{}{}.fd", enc.to_ascii_lowercase(), fam));
        }
    }
    // fontspec: The font "texgyrepagella-regular.otf" cannot be found
    for n in between(log, "The font \"", "\" cannot be found") {
        add(&n);
    }
    out.truncate(40);
    out
}

/// The bundle's file list from the shared (trusted) cache: (file name, path).
/// Handles both index formats Tectonic uses; gzip-compressed or plain.
fn bundle_index() -> Vec<(String, String)> {
    // Tectonic keeps it at <cache>/data/<hash>.index or <cache>/bundles/data/…;
    // look a few levels down rather than assume.
    fn find(dir: &Path, depth: usize, found: &mut Vec<(std::time::SystemTime, PathBuf)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() && depth > 0 {
                find(&e.path(), depth - 1, found);
            } else if ft.is_file() && e.file_name().to_string_lossy().ends_with(".index") {
                let t = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                found.push((t, e.path()));
            }
        }
    }
    let mut found = Vec::new();
    find(&cache_dir(), 3, &mut found);
    let Some((_, newest)) = found.into_iter().max_by_key(|(t, _)| *t) else { return Vec::new() };
    let Some(raw) = read_output(&newest, 128 * 1024 * 1024) else { return Vec::new() };
    let text = if raw.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        if flate2::read::GzDecoder::new(&raw[..]).take(256 * 1024 * 1024).read_to_end(&mut out).is_err() {
            return Vec::new();
        }
        String::from_utf8_lossy(&out).into_owned()
    } else {
        String::from_utf8_lossy(&raw).into_owned()
    };
    let mut files = Vec::new();
    let mut section = String::new();
    for line in text.lines() {
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).to_string();
            continue;
        }
        let bits: Vec<&str> = line.split_whitespace().collect();
        if section.starts_with("FILELIST") && bits.len() >= 5 {
            // TTB: start gzip_len real_len hash path
            let path = bits[4..].join(" ");
            let name = path.rsplit('/').next().unwrap_or(&path).to_string();
            files.push((name, path));
        } else if section.is_empty() && bits.len() == 3 {
            // ITAR: name offset length (no folders)
            files.push((bits[0].to_string(), bits[0].to_string()));
        }
    }
    files
}

/// Turn "missing" names into what to fetch: each name that exists in the
/// bundle, plus the other files in its package folder (data files, configs,
/// sub-modules that packages load in ways LaTeX doesn't report).
fn expand_with_package_folders(names: &[String], index: &[(String, String)]) -> Vec<String> {
    const MAX_FOLDER: usize = 80;
    let mut out: Vec<String> = Vec::new();
    let push = |n: &str, out: &mut Vec<String>| {
        if valid_package_name(n) && !out.iter().any(|x| x == n) {
            out.push(n.to_string());
        }
    };
    if index.is_empty() {
        for n in names {
            push(n, &mut out);
        }
        return out;
    }
    for n in names {
        for (_, path) in index.iter().filter(|(name, _)| name == n) {
            push(n, &mut out);
            if let Some((dir, _)) = path.rsplit_once('/') {
                let siblings: Vec<&String> = index
                    .iter()
                    .filter(|(_, p)| p.rsplit_once('/').map(|(d, _)| d) == Some(dir))
                    .map(|(name, _)| name)
                    .collect();
                if siblings.len() <= MAX_FOLDER {
                    for sib in siblings {
                        push(sib, &mut out);
                    }
                }
            }
        }
    }
    out.truncate(300);
    out
}

/// Download bundle files into the shared cache. Trusted: the only TeX that
/// runs is our own, which just opens each (validated) name.
fn fetch_into_shared_cache(engine: &Path, build: &Path, names: &[String]) -> bool {
    let names: Vec<&String> = names.iter().filter(|n| valid_package_name(n)).collect();
    if names.is_empty() {
        return false;
    }
    let mut doc = String::from("\\documentclass{article}\\newread\\fetchr\\begin{document}\n");
    for n in &names {
        doc.push_str(&format!("\\openin\\fetchr={n} \\closein\\fetchr\n"));
    }
    doc.push_str("x\\end{document}\n");
    let cache = cache_dir();
    let Ok(ws) = Workspace::create(build.join("fetch")) else { return false };
    let params = Params { engine: engine.to_path_buf(), work: ws.dir.clone(), project: ws.dir.clone(), cache: cache.clone() };
    let mut cmd = engine_command(Tier::Basic, true, &params, &ws.home, &ws.tmp, &cache);
    cmd.args(["-X", "compile", "-", "--untrusted", "--outdir"])
        .arg(&ws.out)
        .current_dir(&ws.dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let ok = run_to_end(cmd, &doc, WARMUP_LIMIT);
    let _ = std::fs::remove_dir_all(&ws.dir);
    ok
}

/// Compile offline; if packages are missing, fetch them (trusted, no document
/// code involved), refresh the workspace's cache copy and compile again.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compile_fetching(
    engine: &Path,
    build: &Path,
    ws: &mut Workspace,
    doc_id: u64,
    job: &Job,
    latest: &AtomicU64,
    tier: Tier,
    attempted: &mut Vec<String>,
) -> Outcome {
    let mut rewarmed = false;
    // Packages pull in other packages, so a chain may take a few rounds.
    for _round in 0..12 {
        let outcome = run(engine, ws, job, latest, tier);
        let log = match &outcome {
            Outcome::Pdf { log, .. } | Outcome::Failed { log, .. } => log.clone(),
            _ => return outcome,
        };
        // The cache was never filled (e.g. first launch was offline): try again now.
        if log.contains("isn't cached") && !rewarmed {
            rewarmed = true;
            if warm_up_with(engine, build, true) {
                if let Ok(w) = new_workspace(build, doc_id) {
                    *ws = w;
                    continue;
                }
            }
            return outcome;
        }
        let wanted: Vec<String> = missing_files(&log).into_iter().filter(|n| !attempted.contains(n)).collect();
        if wanted.is_empty() || attempted.len() > 400 || latest.load(Ordering::SeqCst) != job.generation {
            return outcome;
        }
        attempted.extend(wanted.iter().cloned());
        let to_fetch = expand_with_package_folders(&wanted, &bundle_index());
        if to_fetch.is_empty() || !fetch_into_shared_cache(engine, build, &to_fetch) {
            return outcome;
        }
        match new_workspace(build, doc_id) {
            Ok(w) => *ws = w,
            Err(_) => return outcome,
        }
    }
    run(engine, ws, job, latest, tier)
}

/// Pick the strictest sandbox tier that compiles a fixed test document.
pub(crate) fn choose_tier(engine: &Path, ws: &Workspace) -> Tier {
    for tier in [Tier::Strict, Tier::Reduced, Tier::Basic] {
        let _ = std::fs::remove_file(ws.out.join("texput.pdf"));
        let params = Params { engine: engine.to_path_buf(), work: ws.dir.clone(), project: ws.dir.clone(), cache: ws.cache.clone() };
        let mut cmd = engine_command(tier, false, &params, &ws.home, &ws.tmp, &ws.cache);
        cmd.args(["-X", "compile", "-", "--untrusted", "--outdir"])
            .arg(&ws.out)
            .current_dir(&ws.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if run_to_end(cmd, SELFTEST_TEX, Duration::from_secs(120)) && read_output(&ws.out.join("texput.pdf"), MAX_PDF).is_some() {
            let _ = std::fs::remove_file(ws.out.join("texput.pdf"));
            return tier;
        }
    }
    Tier::Strict // nothing worked (e.g. offline): stay strict, the real error will show
}

/// Start the compile worker. Send jobs; receive results. Bump `latest` before
/// sending a job so any in-flight compile for an older generation is killed.
pub fn spawn_worker(build_dir: PathBuf, latest: Arc<AtomicU64>) -> (Sender<Job>, Receiver<Done>) {
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (done_tx, done_rx) = mpsc::channel::<Done>();
    thread::Builder::new()
        .name("typeset-compiler".into())
        .spawn(move || {
            let mut warmed = false;
            let mut tier: Option<Tier> = None;
            let mut ws: Option<(u64, Workspace)> = None;
            let mut attempted: Vec<String> = Vec::new();
            while let Ok(mut job) = job_rx.recv() {
                // Coalesce: only the newest queued job matters.
                while let Ok(newer) = job_rx.try_recv() {
                    job = newer;
                }
                let outcome = (|| {
                    if !sandbox_available() {
                        let msg = "The macOS sandbox (sandbox-exec) isn't available, so Typeset won't compile untrusted documents.".to_string();
                        return Outcome::Failed { errors: vec![TexError { message: msg.clone(), line: None }], log: msg };
                    }
                    let engine = match find_tectonic() {
                        Ok(e) => e,
                        Err(msg) => return Outcome::EngineMissing(msg),
                    };
                    if !warmed {
                        warmed = warm_up(&engine, &build_dir);
                    }
                    if ws.as_ref().map(|(id, _)| *id) != Some(job.doc_id) {
                        attempted.clear();
                        match new_workspace(&build_dir, job.doc_id) {
                            Ok(w) => ws = Some((job.doc_id, w)),
                            Err(e) => {
                                let msg = format!("Couldn't create a private workspace: {e}");
                                return Outcome::Failed { errors: vec![TexError { message: msg.clone(), line: None }], log: msg };
                            }
                        }
                    }
                    let (doc_id, w) = ws.as_mut().expect("workspace");
                    let t = *tier.get_or_insert_with(|| choose_tier(&engine, w));
                    compile_fetching(&engine, &build_dir, w, *doc_id, &job, &latest, t, &mut attempted)
                })();
                if done_tx.send(Done { generation: job.generation, outcome }).is_err() {
                    break;
                }
            }
        })
        .expect("spawn compiler thread");
    (job_tx, done_rx)
}

pub(crate) fn run(engine: &Path, ws: &Workspace, job: &Job, latest: &AtomicU64, tier: Tier) -> Outcome {
    let start = Instant::now();
    let pdf_path = ws.out.join("texput.pdf");
    let sync_path = ws.out.join("texput.synctex.gz");
    let log_path = ws.out.join("texput.log");
    let _ = std::fs::remove_file(&pdf_path);
    let _ = std::fs::remove_file(&sync_path);
    let _ = std::fs::remove_file(&log_path);

    // The document's folder is readable (for \input and images) unless that
    // would expose too much; unsaved documents get only the workspace.
    let mut note = None;
    let project = if job.workdir.as_os_str().is_empty() {
        ws.dir.clone()
    } else {
        match safe_project_dir(&job.workdir, &home_dir()) {
            Some(p) => p,
            None => {
                note = Some("For safety, files beside this document can't be used: move it into its own folder".to_string());
                ws.dir.clone()
            }
        }
    };

    let params = Params { engine: engine.to_path_buf(), work: ws.dir.clone(), project: project.clone(), cache: ws.cache.clone() };
    let mut cmd = engine_command(tier, false, &params, &ws.home, &ws.tmp, &ws.cache);
    // --only-cached: documents never download anything themselves.
    // --keep-logs: the log names any package that still needs fetching.
    cmd.args(["-X", "compile", "-", "--untrusted", "--only-cached", "--keep-logs", "--synctex", "-Z", "continue-on-errors", "--outdir"])
        .arg(&ws.out)
        .current_dir(&project)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("Could not start Tectonic: {e}");
            return Outcome::Failed { errors: vec![TexError { message: msg.clone(), line: None }], log: msg };
        }
    };

    // Feed stdin and drain stderr on helper threads so large documents or
    // chatty logs can never deadlock the pipes.
    if let Some(mut stdin) = child.stdin.take() {
        let src = job.source.clone().into_bytes();
        thread::spawn(move || {
            let _ = stdin.write_all(&src);
        });
    }
    let stderr_reader = child.stderr.take().map(|err| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err.take(4 * 1024 * 1024).read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        })
    });

    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(_) => break None,
        }
        if latest.load(Ordering::SeqCst) != job.generation {
            let _ = child.kill();
            let _ = child.wait();
            return Outcome::Cancelled;
        }
        if start.elapsed() > TIME_LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            return Outcome::Failed {
                errors: vec![TexError { message: "Compile took over 3 minutes and was stopped (infinite loop?)".into(), line: None }],
                log: String::new(),
            };
        }
        thread::sleep(Duration::from_millis(10));
    };

    let mut stderr = stderr_reader.and_then(|h| h.join().ok()).unwrap_or_default();
    let errors = parse_errors(&stderr);
    // Append the TeX log (capped) so missing packages can be spotted.
    if let Some(log) = read_output(&log_path, 16 * 1024 * 1024) {
        stderr.push_str("\n--- texput.log ---\n");
        stderr.push_str(&String::from_utf8_lossy(&log));
    }

    match read_output(&pdf_path, MAX_PDF) {
        Some(pdf) => {
            // Parsed here, off the main thread; used for click-to-sync.
            let sync = read_output(&sync_path, MAX_SYNC_GZ).and_then(|gz| crate::sync::SyncIndex::from_gz(&gz));
            Outcome::Pdf { pdf, elapsed: start.elapsed(), errors, tier, log: stderr, sync, note }
        }
        None => {
            let mut errors = errors;
            if errors.is_empty() {
                let detail = last_meaningful_line(&stderr)
                    .unwrap_or_else(|| format!("Tectonic exited ({})", status.map(|s| s.to_string()).unwrap_or_default()));
                errors.push(TexError { message: detail, line: None });
            }
            Outcome::Failed { errors, log: stderr }
        }
    }
}

/// Lines Tectonic prints that are summaries rather than real TeX errors.
fn is_boilerplate(msg: &str) -> bool {
    const NOISE: [&str; 6] = [
        "errors were issued by the TeX engine",
        "halted on",
        "the TeX engine had an error",
        "failed to execute the TeX engine",
        "use --print and/or --keep-logs",
        "unhandled TeX engine error",
    ];
    NOISE.iter().any(|n| msg.contains(n))
}

/// Pull `error: texput.tex:12: Undefined control sequence.` style lines out of
/// Tectonic's stderr. Errors in the edited buffer come first.
pub fn parse_errors(stderr: &str) -> Vec<TexError> {
    let mut out: Vec<TexError> = Vec::new();
    for raw in stderr.lines() {
        let line = raw.trim();
        // Tectonic prints the reason for a failure on "caused by:" lines;
        // fold them into the error they belong to so the status bar says
        // *what* failed, not just "Operation not permitted".
        if let Some(cause) = line.strip_prefix("caused by:") {
            if let Some(last) = out.last_mut() {
                if last.line.is_none() && !is_boilerplate(cause) {
                    last.message = format!("{} — {}", last.message, cause.trim());
                }
            }
            continue;
        }
        let Some(rest) = line.strip_prefix("error:") else { continue };
        let rest = rest.trim();
        if rest.is_empty() || is_boilerplate(rest) {
            continue;
        }
        let (file, lineno, msg) = split_location(rest);
        let in_buffer = matches!(file, Some(f) if f == "texput.tex" || f.ends_with("/texput.tex"));
        let message = match (file, lineno) {
            (Some(f), Some(n)) if !in_buffer => format!("{f}:{n}: {msg}"),
            _ => msg.to_string(),
        };
        let err = TexError { message, line: if in_buffer { lineno } else { None } };
        if !out.contains(&err) {
            out.push(err);
        }
    }
    out.sort_by_key(|e| e.line.is_none());
    out
}

/// "file.tex:12: message" -> (Some(file), Some(12), message)
fn split_location(s: &str) -> (Option<&str>, Option<usize>, &str) {
    let mut parts = s.splitn(3, ':');
    if let (Some(f), Some(n), Some(m)) = (parts.next(), parts.next(), parts.next()) {
        if f.ends_with(".tex") || f.ends_with(".sty") || f.ends_with(".cls") || f.ends_with(".bib") {
            if let Ok(n) = n.trim().parse::<usize>() {
                return (Some(f), Some(n), m.trim());
            }
        }
    }
    if let Some(m) = s.strip_prefix('!') {
        return (None, None, m.trim());
    }
    (None, None, s)
}

fn last_meaningful_line(stderr: &str) -> Option<String> {
    stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("note:"))
        .map(|l| l.trim_start_matches("error:").trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_buffer_errors_with_lines() {
        let s = "note: Running TeX ...\n\
                 error: texput.tex:7: Undefined control sequence.\n\
                 error: chapter2.tex:3: Missing $ inserted.\n\
                 warning: texput.tex:2: Overfull \\hbox\n\
                 error: errors were issued by the TeX engine, but were ignored; use --print and/or --keep-logs for details.\n";
        let e = parse_errors(s);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0], TexError { message: "Undefined control sequence.".into(), line: Some(7) });
        assert_eq!(e[1].line, None);
        assert!(e[1].message.starts_with("chapter2.tex:3:"));
    }

    #[test]
    fn folds_caused_by_into_message() {
        let e = parse_errors("error: while making cache root\ncaused by: Operation not permitted (os error 1)\n");
        assert_eq!(e[0].message, "while making cache root — Operation not permitted (os error 1)");
    }

    #[test]
    fn timestamp_is_whole_hours() {
        let t = hour_epoch();
        assert_eq!(t % 3600, 0);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        assert!(now - t < 3600);
    }

    #[test]
    fn cache_path() {
        assert!(cache_dir().ends_with("Library/Caches/Typeset/tectonic"));
    }

    /// Every (param "X") in every profile variant must be supplied, and the
    /// parentheses must balance — a typo here would break compiling on the Mac.
    #[test]
    fn profiles_are_well_formed() {
        let p = Params { engine: "/e/bin/tectonic".into(), work: "/w".into(), project: "/p".into(), cache: "/c".into() };
        let args = sandbox_args(&p);
        let supplied: Vec<String> = args.iter().filter(|a| a.contains('=')).map(|a| a.split('=').next().unwrap().to_string()).collect();
        for tier in [Tier::Strict, Tier::Reduced, Tier::Basic] {
            for trusted in [false, true] {
                let text = profile(tier, trusted);
                assert_eq!(text.matches('(').count(), text.matches(')').count(), "{tier:?} {trusted}");
                for chunk in text.split("(param \"").skip(1) {
                    let name = chunk.split('"').next().unwrap();
                    assert!(supplied.iter().any(|s| s == name), "param {name} not supplied");
                }
                // Core rules are always there.
                assert!(text.contains("(deny process-fork)"));
                assert!(text.contains("(deny file-write*)"));
                assert!(text.contains("(deny file-read-data (subpath (param \"HOME\"))"));
                // The shared cache is writable only in the trusted warm-up.
                assert_eq!(text.contains("(allow file-write* (subpath (param \"CACHE\")))"), trusted);
                assert_eq!(text.contains("(deny process-exec*)"), tier != Tier::Basic);
                assert_eq!(text.contains("com.apple.pasteboard.1"), tier == Tier::Strict);
                // Documents get no network; trusted fetches do.
                let blocks_outgoing = text.contains("(deny network*)") || text.contains("(deny network-outbound");
                assert_eq!(blocks_outgoing, !trusted, "{tier:?} {trusted}");
                // The document folder is filtered by file type, hidden files denied.
                assert!(text.contains("(require-all (subpath (param \"PROJECT\")) (regex #\"\\.("));
                assert!(text.contains("(regex #\"/\\.\")"));
            }
        }
        // Secrets are denied *after* the project folder is allowed (later rules win).
        let t = profile(Tier::Strict, false);
        assert!(t.find("(param \"PROJECT\")").unwrap() < t.find("(param \"S0\")").unwrap());
        assert!(SENSITIVE.contains(&".ssh") && SENSITIVE.contains(&"Library/Keychains"));
    }

    #[test]
    fn project_file_types() {
        let re = project_ext_regex();
        assert!(re.starts_with("\\.(") && re.ends_with(")$"));
        assert!(re.contains("|[pP][nN][gG]|") && re.contains("|[tT][eE][xX]|") || re.contains("([tT][eE][xX]|"));
        for secret in ["env", "pem", "key", "json", "yaml", "yml", "toml", "p12", "pfx", "sqlite", "db", "plist", "ini", "conf", "kdbx", "gpg", "asc"] {
            assert!(!PROJECT_EXTS.contains(&secret), "{secret} must not be readable");
        }
    }

    #[test]
    fn package_names_are_validated() {
        for ok in ["lipsum.sty", "tulmr.fd", "lmroman10-regular.otf", "cmr10.tfm", "pgf.revision.tex", "t1cmr.fd", "x+y.def"] {
            assert!(valid_package_name(ok), "{ok}");
        }
        for bad in ["", "../etc/passwd", "/abs.sty", "a b.sty", "a\\b", "foo..sty", ".hidden", "x;rm", "a`b", &"x".repeat(81), "naïve.sty", "a{b}.sty"] {
            assert!(!valid_package_name(bad), "{bad}");
        }
    }

    #[test]
    fn finds_missing_packages_in_logs() {
        let log = "error: texput.tex:2: LaTeX Error: File `lipsum.sty' not found.\n\
                   ! I can't find file `chapter2'.\n\
                   ! Font \\TU/foo/m/n/10=\"[foo-regular]:mapping=tex-text\" at 10.0pt not loadable: Metric (TFM) file or installed font not found.\n\
                   ! Font \\x=cmbx12 not loadable: Metric (TFM) file not found.\n\
                   LaTeX Font Warning: Font shape `T1/lmr/m/n' undefined\n\
                   Package fontspec Error: The font \"texgyrepagella-regular.otf\" cannot be found.\n\
                   ! LaTeX Error: File `../../etc/passwd' not found.\n\
                   ! LaTeX Error: File `lipsum.sty' not found.\n";
        let m = missing_files(log);
        for want in ["lipsum.sty", "chapter2", "chapter2.tex", "foo-regular", "foo-regular.tfm", "cmbx12", "cmbx12.tfm", "t1lmr.fd", "texgyrepagella-regular.otf"] {
            assert!(m.iter().any(|x| x == want), "{want} missing from {m:?}");
        }
        assert!(!m.iter().any(|x| x.contains("..") || x.contains('/')), "{m:?}");
        assert_eq!(m.iter().filter(|x| *x == "lipsum.sty").count(), 1);
        // An attacker can't make us fetch hundreds of names in one go.
        let flood: String = (0..500).map(|i| format!("File `p{i}.sty' not found\n")).collect();
        assert!(missing_files(&flood).len() <= 40);
    }

    #[test]
    fn latex3_messages_and_package_folders() {
        let m = missing_files("! LaTeX Error: File 'lipsum.ltd' not found.\n");
        assert!(m.contains(&"lipsum.ltd".to_string()) && m.contains(&"lipsum.ltd.tex".to_string()), "{m:?}");
        let index: Vec<(String, String)> = [
            "texlive/tex/latex/lipsum/lipsum.sty",
            "texlive/tex/latex/lipsum/lipsum.ltd.tex",
            "texlive/tex/latex/lipsum/cicero.ltd.tex",
            "texlive/tex/latex/other/other.sty",
        ]
        .iter()
        .map(|p| (p.rsplit('/').next().unwrap().to_string(), p.to_string()))
        .collect();
        let got = expand_with_package_folders(&["lipsum.sty".into(), "nothere.sty".into()], &index);
        assert_eq!(got, vec!["lipsum.sty", "lipsum.ltd.tex", "cicero.ltd.tex"]);
        // Without an index, fall back to the names themselves.
        assert_eq!(expand_with_package_folders(&["x.sty".into()], &[]), vec!["x.sty"]);
    }

    #[test]
    fn reads_both_index_formats() {
        let tmp = make_build_dir().unwrap();
        let shared = tmp.join("cache");
        std::fs::create_dir_all(shared.join("data")).unwrap();
        let _serial = TEST_CACHE_USER.lock().unwrap_or_else(|e| e.into_inner());
        *TEST_CACHE.lock().unwrap() = Some(shared.clone());
        let ttb = "[DEFAULTSEARCH]\nx\n[FILELIST]\n0 10 20 abcd texlive/tex/latex/lipsum/lipsum.sty\n30 5 9 ef01 texlive/tex/latex/lipsum/lipsum.ltd.tex\n[SEARCH:x]\ny\n";
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, ttb.as_bytes()).unwrap();
        std::fs::write(shared.join("data/abc.index"), gz.finish().unwrap()).unwrap();
        let idx = bundle_index();
        *TEST_CACHE.lock().unwrap() = None;
        assert_eq!(idx, vec![
            ("lipsum.sty".to_string(), "texlive/tex/latex/lipsum/lipsum.sty".to_string()),
            ("lipsum.ltd.tex".to_string(), "texlive/tex/latex/lipsum/lipsum.ltd.tex".to_string()),
        ]);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// Offline compile → missing package found → trusted fetch → fresh
    /// workspace → compile succeeds. (Fake engine; real one is tested on the Mac.)
    #[test]
    fn fetches_missing_packages_then_recompiles() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = make_build_dir().unwrap();
        let shared = tmp.join("shared-cache");
        std::fs::create_dir_all(&shared).unwrap();
        let _serial = TEST_CACHE_USER.lock().unwrap_or_else(|e| e.into_inner());
        *TEST_CACHE.lock().unwrap() = Some(shared.clone());
        let fake = tmp.join("tectonic");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             out=\"\"; offline=0\n\
             while [ $# -gt 0 ]; do [ \"$1\" = --outdir ] && out=\"$2\"; [ \"$1\" = --only-cached ] && offline=1; shift; done\n\
             doc=$(cat)\n\
             if [ $offline = 0 ]; then\n\
               case \"$doc\" in *'openin\\fetchr=lipsum.sty'*) echo got > \"$TECTONIC_CACHE_DIR/lipsum.sty\";; esac\n\
               exit 0\n\
             fi\n\
             if [ ! -f \"$TECTONIC_CACHE_DIR/lipsum.sty\" ]; then\n\
               echo \"error: texput.tex:1: LaTeX Error: File \\`lipsum.sty' not found.\" >&2; exit 1\n\
             fi\n\
             printf ok > \"$out/texput.pdf\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let build = tmp.join("build");
        std::fs::create_dir_all(&build).unwrap();
        let mut ws = new_workspace(&build, 7).unwrap();
        let latest = AtomicU64::new(1);
        let job = Job { generation: 1, source: "\\usepackage{lipsum}".into(), workdir: PathBuf::new(), doc_id: 7 };
        let mut attempted = Vec::new();
        let o = compile_fetching(&fake, &build, &mut ws, 7, &job, &latest, Tier::Strict, &mut attempted);
        *TEST_CACHE.lock().unwrap() = None;
        assert!(matches!(o, Outcome::Pdf { .. }), "{o:?}");
        assert!(shared.join("lipsum.sty").exists(), "fetched into the shared cache");
        assert_eq!(attempted, vec!["lipsum.sty".to_string()]);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn project_dir_never_exposes_home() {
        let tmp = make_build_dir().unwrap();
        let home = tmp.join("home");
        let doc = home.join("Papers/thesis");
        std::fs::create_dir_all(&doc).unwrap();
        assert_eq!(safe_project_dir(&doc, &home), Some(doc.canonicalize().unwrap()));
        assert_eq!(safe_project_dir(&home, &home), None, "document directly in $HOME");
        assert_eq!(safe_project_dir(&tmp, &home), None, "a parent of $HOME");
        assert_eq!(safe_project_dir(Path::new("/"), &home), None);
        assert_eq!(safe_project_dir(&home.join("missing"), &home), None);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn outputs_are_read_safely() {
        let tmp = make_build_dir().unwrap();
        let secret = tmp.join("secret");
        std::fs::write(&secret, b"PRIVATE KEY").unwrap();
        // Symlink planted where the PDF should be.
        let link = tmp.join("link.pdf");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        assert!(read_output(&link, MAX_PDF).is_none(), "followed a symlink");
        // Hard link planted where the PDF should be.
        let hard = tmp.join("hard.pdf");
        std::fs::hard_link(&secret, &hard).unwrap();
        assert!(read_output(&hard, MAX_PDF).is_none(), "read a hard-linked file");
        // A FIFO must not hang us.
        let fifo = tmp.join("fifo.pdf");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(read_output(&fifo, MAX_PDF).is_none());
        // Too big.
        let big = tmp.join("big.pdf");
        std::fs::write(&big, vec![b'x'; 100]).unwrap();
        assert!(read_output(&big, 50).is_none());
        assert_eq!(read_output(&big, 100).unwrap().len(), 100);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn refuses_engine_others_can_modify() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = make_build_dir().unwrap();
        let bin = tmp.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let e = bin.join("tectonic");
        std::fs::write(&e, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&e, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(check_trustworthy(&e).is_ok());
        std::fs::set_permissions(&e, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(check_trustworthy(&e).is_err(), "world-writable binary accepted");
        std::fs::set_permissions(&e, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(check_trustworthy(&e).is_err(), "world-writable folder accepted");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn workspaces_are_private_and_replaced() {
        use std::os::unix::fs::PermissionsExt;
        let build = make_build_dir().unwrap();
        let a = new_workspace(&build, 1).unwrap();
        std::fs::write(a.cache.join("poisoned.sty"), "evil").unwrap();
        assert_eq!(std::fs::metadata(&a.dir).unwrap().permissions().mode() & 0o777, 0o700);
        let b = new_workspace(&build, 2).unwrap();
        assert!(!a.dir.exists(), "old workspace (and its poisoned cache) must be gone");
        assert!(!b.cache.join("poisoned.sty").exists());
        std::fs::remove_dir_all(&build).unwrap();
    }

    #[test]
    fn parses_bang_errors() {
        let e = parse_errors("error: !LaTeX Error: File `missing.sty' not found.\n");
        assert_eq!(e[0].message, "LaTeX Error: File `missing.sty' not found.");
    }

    #[test]
    fn build_dir_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = make_build_dir().unwrap();
        let mode = std::fs::metadata(&d).unwrap().permissions().mode() & 0o777;
        std::fs::remove_dir_all(&d).unwrap();
        assert_eq!(mode, 0o700);
    }

    /// End-to-end with a fake engine: checks stdin feeding, cwd, PDF pickup,
    /// error parsing, and cancellation of superseded compiles.
    #[test]
    fn worker_with_fake_engine() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = make_build_dir().unwrap();
        let bin = tmp.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("tectonic");
        // Echo stdin into the "PDF"; sleep if the doc says SLOW; report an error if it says BAD.
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             out=\"\"; while [ $# -gt 0 ]; do [ \"$1\" = --outdir ] && out=\"$2\"; shift; done\n\
             doc=$(cat)\n\
             case \"$doc\" in *SLOW*) sleep 5;; esac\n\
             case \"$doc\" in *ENV*) env > \"$out/env.txt\"; ulimit -c > \"$out/core.txt\"; ulimit -f > \"$out/fsize.txt\";; esac\n\
             case \"$doc\" in *BAD*) echo 'error: texput.tex:2: Undefined control sequence.' >&2;; esac\n\
             printf '%s|%s' \"$doc\" \"$(pwd)\" > \"$out/texput.pdf\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let build = tmp.join("build");
        std::fs::create_dir_all(&build).unwrap();
        let ws = new_workspace(&build, 1).unwrap();
        let proj = tmp.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let latest = Arc::new(AtomicU64::new(0));

        // Direct run: success with recovered error.
        latest.store(1, Ordering::SeqCst);
        let job = Job { generation: 1, source: "hello\nBAD ENV".into(), workdir: proj.clone(), doc_id: 1 };
        match run(&fake, &ws, &job, &latest, Tier::Strict) {
            Outcome::Pdf { pdf, errors, .. } => {
                let s = String::from_utf8(pdf).unwrap();
                assert!(s.starts_with("hello\nBAD ENV|"));
                assert!(s.ends_with(proj.canonicalize().unwrap().to_str().unwrap()));
                assert_eq!(errors[0].line, Some(2));
            }
            o => panic!("unexpected {o:?}"),
        }
        // The engine gets a scrubbed environment and resource limits.
        let env = std::fs::read_to_string(ws.out.join("env.txt")).unwrap();
        assert!(env.contains("TECTONIC_UNTRUSTED_MODE=1"));
        assert!(env.contains(&format!("HOME={}", ws.home.display())), "{env}");
        assert!(env.contains(&format!("TECTONIC_CACHE_DIR={}", ws.cache.display())));
        assert!(!env.contains("CARGO_PKG_NAME"), "parent environment leaked:\n{env}");
        assert_eq!(std::fs::read_to_string(ws.out.join("core.txt")).unwrap().trim(), "0");
        assert_ne!(std::fs::read_to_string(ws.out.join("fsize.txt")).unwrap().trim(), "unlimited");

        // Cancellation: a slow compile is killed once a newer generation exists.
        latest.store(2, Ordering::SeqCst);
        let slow = Job { generation: 2, source: "SLOW".into(), workdir: proj.clone(), doc_id: 1 };
        let l2 = latest.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            l2.store(3, Ordering::SeqCst);
        });
        let t = Instant::now();
        let o = run(&fake, &ws, &slow, &latest, Tier::Strict);
        canceller.join().unwrap();
        assert!(matches!(o, Outcome::Cancelled), "{o:?}");
        assert!(t.elapsed() < Duration::from_secs(2));

        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
