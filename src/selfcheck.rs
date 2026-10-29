//! `Typeset --security-check`: attack the installed app on this Mac.
//!
//! Runs hostile documents through the real compiler path (same sandbox, same
//! workspace isolation, same Tectonic) and probes the sandbox rules directly
//! with a shell confined by the exact same profile. Uses throwaway decoy
//! files with random contents — never your real secrets — and cleans up.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::compile::{self, Job, Outcome, Tier, Workspace};

struct Report {
    pass: usize,
    fail: usize,
}

impl Report {
    fn check(&mut self, ok: bool, what: &str) {
        if ok {
            self.pass += 1;
            println!("  PASS  {what}");
        } else {
            self.fail += 1;
            println!("  FAIL  {what}");
        }
    }
}

fn token() -> String {
    let mut b = [0u8; 12];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// TeX that tries to read `path` and reports what it got into result.txt.
fn read_attempt(path: &Path) -> String {
    format!(
        "\\documentclass{{article}}\\newread\\r\\newwrite\\w\\begin{{document}}\n\
         \\immediate\\openout\\w=result.txt\n\
         \\openin\\r=\"{}\"\n\
         \\ifeof\\r\\immediate\\write\\w{{BLOCKED}}\\else\\read\\r to\\x\\immediate\\write\\w{{READ:\\detokenize\\expandafter{{\\x}}}}\\fi\n\
         \\immediate\\closeout\\w\nx\n\\end{{document}}\n",
        path.display()
    )
}

/// TeX that tries to create `path` (absolute or relative) via \openout.
fn write_attempt(path: &str) -> String {
    format!(
        "\\documentclass{{article}}\\newwrite\\w\\begin{{document}}\n\
         \\immediate\\openout\\w=\"{path}\"\\immediate\\write\\w{{pwned}}\\immediate\\closeout\\w\nx\n\\end{{document}}\n"
    )
}

fn compile(engine: &Path, ws: &Workspace, tier: Tier, project: &Path, src: &str, generation: &AtomicU64) -> Outcome {
    let g = generation.fetch_add(1, Ordering::SeqCst) + 1;
    let job = Job { generation: g, source: src.to_string(), workdir: project.to_path_buf(), doc_id: 0 };
    compile::run(engine, ws, &job, generation, tier)
}

fn result_of(ws: &Workspace) -> String {
    std::fs::read_to_string(ws.out.join("result.txt")).unwrap_or_default()
}

/// Run a shell command confined by the document sandbox; true if it succeeded.
fn confined(tier: Tier, ws: &Workspace, project: &Path, script: &str) -> bool {
    let mut cmd = compile::probe_command(tier, Path::new("/bin/bash"), ws, project);
    cmd.arg("-c").arg(script).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else { return false };
    let start = Instant::now();
    loop {
        if let Ok(Some(s)) = child.try_wait() {
            return s.success();
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Is the internet reachable from an ordinary (unsandboxed) shell?
fn online() -> bool {
    let Ok(mut child) = std::process::Command::new("/bin/bash")
        .arg("-c")
        .arg("exec 3<>/dev/tcp/1.1.1.1/443")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let start = Instant::now();
    loop {
        if let Ok(Some(s)) = child.try_wait() {
            return s.success();
        }
        if start.elapsed() > Duration::from_secs(6) {
            let _ = child.kill();
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn run() -> i32 {
    println!("Typeset security check\n");
    let mut r = Report { pass: 0, fail: 0 };
    let engine = match compile::find_tectonic() {
        Ok(e) => e,
        Err(msg) => {
            println!("  FAIL  {msg}");
            return 2;
        }
    };
    println!("Engine: {}", engine.display());
    let Ok(build) = compile::make_build_dir() else {
        println!("  FAIL  couldn't create a private build folder");
        return 2;
    };
    print!("Preparing LaTeX packages (first run downloads them)… ");
    let warmed = compile::warm_up(&engine, &build);
    println!("{}", if warmed { "ready" } else { "not ready (offline?) — document tests may fail" });

    // Decoys: random tokens in places a document must not reach.
    let tok = token();
    let test_root = home().join(format!("TypesetSecurityCheck-{}", &tok[..8]));
    let project = test_root.join("project");
    let _ = std::fs::create_dir_all(&project);
    let sibling = project.join("chapter.tex");
    let _ = std::fs::write(&sibling, format!("SIBLING{tok}\n"));
    let image = project.join("Figure.PNG");
    let _ = std::fs::write(&image, format!("IMAGE{tok}\n"));
    let dotenv = project.join(".env");
    let _ = std::fs::write(&dotenv, format!("DOTENV{tok}\n"));
    let keyfile = project.join("server.key");
    let _ = std::fs::write(&keyfile, format!("KEY{tok}\n"));
    let home_decoy = test_root.join("secret.txt"); // in $HOME, outside the project
    let _ = std::fs::write(&home_decoy, format!("HOMESECRET{tok}\n"));
    let support_decoy = home().join(format!("Library/Application Support/TypesetSecurityCheck-{}.txt", &tok[..8]));
    let _ = std::fs::write(&support_decoy, format!("APPSUPPORT{tok}\n"));
    let tmp_decoy = std::env::temp_dir().join(format!("TypesetSecurityCheck-{}.txt", &tok[..8]));
    let _ = std::fs::write(&tmp_decoy, format!("TMPSECRET{tok}\n"));
    let ssh_known = home().join(".ssh/known_hosts");

    let generation = Arc::new(AtomicU64::new(0));
    let ws = match compile::new_workspace(&build, 1) {
        Ok(w) => w,
        Err(e) => {
            println!("  FAIL  couldn't create a workspace: {e}");
            return 2;
        }
    };
    let tier = compile::choose_tier(&engine, &ws);
    println!("Sandbox level chosen for this Mac: {}\n", tier.label());
    r.check(tier == Tier::Strict, "strictest sandbox level works on this Mac");

    println!("\nSandbox rules (a shell confined exactly like a document):");
    r.check(confined(tier, &ws, &project, &format!("read -r l < '{}'", sibling.display())), "can read files in the document's own folder (needed for \\input)");
    r.check(confined(tier, &ws, &project, &format!("read -r l < '{}'", image.display())), "can read images beside the document (any capitalisation, e.g. .PNG)");
    r.check(!confined(tier, &ws, &project, &format!("read -r l < '{}'", dotenv.display())), "cannot read hidden files beside the document (.env, .git…)");
    r.check(!confined(tier, &ws, &project, &format!("read -r l < '{}'", keyfile.display())), "cannot read keys or other non-LaTeX files beside the document");
    r.check(!confined(tier, &ws, &project, &format!("read -r l < '{}'", home_decoy.display())), "cannot read other files in your home folder");
    r.check(!confined(tier, &ws, &project, &format!("read -r l < '{}'", support_decoy.display())), "cannot read ~/Library/Application Support");
    r.check(!confined(tier, &ws, &project, &format!("read -r l < '{}'", tmp_decoy.display())), "cannot read other apps' temporary files");
    if ssh_known.exists() {
        r.check(!confined(tier, &ws, &project, &format!("read -r l < '{}'", ssh_known.display())), "cannot read ~/.ssh");
    }
    r.check(!confined(tier, &ws, &project, &format!("echo x > '{}'", test_root.join("written.txt").display())), "cannot write to your home folder");
    r.check(!confined(tier, &ws, &project, &format!("echo x > '{}'", compile::cache_dir().join("pwned.sty").display())), "cannot write to the shared package cache");
    r.check(confined(tier, &ws, &project, &format!("echo x > '{}'", ws.tmp.join("ok.txt").display())), "can write inside its own private workspace");
    if tier != Tier::Basic {
        r.check(!confined(tier, &ws, &project, "exec /usr/bin/true"), "cannot start other programs");
        r.check(!confined(tier, &ws, &project, "exec /usr/bin/osascript -e 'return 1'"), "cannot run AppleScript");
    }
    r.check(!confined(tier, &ws, &project, "( exit 0 ) && /usr/bin/true"), "cannot fork new processes");
    if online() {
        r.check(!confined(tier, &ws, &project, "exec 3<>/dev/tcp/1.1.1.1/443"), "cannot connect to the internet");
        r.check(!confined(tier, &ws, &project, "exec 3<>/dev/tcp/apple.com/443"), "cannot look up or reach websites (DNS)");
    } else {
        println!("  SKIP  network tests (this Mac seems to be offline)");
    }

    println!("\nHostile documents (through the real compiler):");
    let ok = matches!(compile(&engine, &ws, tier, &project, "\\documentclass{article}\\begin{document}Hello\\end{document}", &generation), Outcome::Pdf { .. });
    r.check(ok, "a normal document compiles");

    let _ = compile(&engine, &ws, tier, &project, &read_attempt(&sibling), &generation);
    r.check(result_of(&ws).contains(&format!("SIBLING{tok}")), "\\openin of a file beside the document works");
    let _ = compile(&engine, &ws, tier, &project, &read_attempt(&dotenv), &generation);
    r.check(!result_of(&ws).contains(&tok), "\\openin of a hidden .env beside the document is blocked");
    let _ = compile(&engine, &ws, tier, &project, &read_attempt(&keyfile), &generation);
    r.check(!result_of(&ws).contains(&tok), "\\openin of a .key file beside the document is blocked");
    for (path, what) in [
        (&home_decoy, "\\openin of a file elsewhere in your home folder is blocked"),
        (&support_decoy, "\\openin of ~/Library/Application Support is blocked"),
        (&tmp_decoy, "\\openin of another app's temp file is blocked"),
    ] {
        let _ = compile(&engine, &ws, tier, &project, &read_attempt(path), &generation);
        r.check(!result_of(&ws).contains(&tok), what);
    }
    // Saved directly in $HOME: no folder access at all.
    let _ = compile(&engine, &ws, tier, &home(), &read_attempt(&home_decoy), &generation);
    r.check(!result_of(&ws).contains(&tok), "a document saved directly in your home folder gets no access to it");

    let targets = [
        (test_root.join("pwned.txt"), "write a file into your home folder"),
        (compile::cache_dir().join("pwned.sty"), "plant a file in the shared package cache"),
        (std::env::temp_dir().join(format!("TypesetPwned-{}.txt", &tok[..8])), "write into the system temp folder"),
    ];
    for (path, what) in &targets {
        let _ = compile(&engine, &ws, tier, &project, &write_attempt(&path.display().to_string()), &generation);
        r.check(!path.exists(), &format!("\\openout cannot {what}"));
    }
    let rel = format!("../../../../{}/escaped.txt", test_root.strip_prefix(home()).map(|p| p.display().to_string()).unwrap_or_default());
    let _ = compile(&engine, &ws, tier, &project, &write_attempt(&rel), &generation);
    r.check(!test_root.join("escaped.txt").exists(), "\\openout with ../ cannot escape the output folder");

    let marker = test_root.join("shell-escape.txt");
    let _ = compile(&engine, &ws, tier, &project, &format!("\\documentclass{{article}}\\begin{{document}}\\immediate\\write18{{touch '{}'}}x\\end{{document}}", marker.display()), &generation);
    r.check(!marker.exists(), "\\write18 shell escape is disabled");

    // A package that isn't downloaded yet: fetched by the trusted step, then
    // the document compiles offline.
    if online() {
        let mut ws3 = compile::new_workspace(&build, 3).expect("workspace");
        let mut attempted = Vec::new();
        let g = generation.fetch_add(1, Ordering::SeqCst) + 1;
        let job = Job {
            generation: g,
            source: "\\documentclass{article}\\usepackage{lipsum}\\begin{document}\\lipsum[1]\\end{document}".into(),
            workdir: PathBuf::new(),
            doc_id: 3,
        };
        let o = compile::compile_fetching(&engine, &build, &mut ws3, 3, &job, &generation, tier, &mut attempted);
        let ok = matches!(&o, Outcome::Pdf { errors, .. } if errors.is_empty());
        r.check(ok, "a new package is fetched safely and the document compiles offline");
        if !ok {
            let first = match &o {
                Outcome::Pdf { errors, .. } | Outcome::Failed { errors, .. } => errors.first().map(|e| e.message.clone()).unwrap_or_default(),
                other => format!("{other:?}").chars().take(120).collect(),
            };
            println!("        (detail: {first}; fetched: {})", attempted.join(", "));
        }
        let _ = std::fs::remove_dir_all(&ws3.dir);
    }

    // Workspace isolation: poison this document's cache, then open "another document".
    let poison = ws.cache.join("typeset-poison.sty");
    let _ = compile(&engine, &ws, tier, &project, &write_attempt(&poison.display().to_string()), &generation);
    let ws2 = compile::new_workspace(&build, 2).expect("workspace");
    r.check(!ws.dir.exists() && !ws2.cache.join("typeset-poison.sty").exists(), "anything a document plants is gone when you open another document");

    // A runaway document is stopped when you keep typing.
    let gen2 = Arc::new(AtomicU64::new(10));
    let g = gen2.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        g.store(11, Ordering::SeqCst);
    });
    let start = Instant::now();
    let job = Job { generation: 10, source: "\\def\\x{\\x}\\x".into(), workdir: PathBuf::new(), doc_id: 2 };
    let o = compile::run(&engine, &ws2, &job, &gen2, tier);
    let _ = canceller.join();
    r.check(matches!(o, Outcome::Cancelled) && start.elapsed() < Duration::from_secs(10), "an infinite loop is stopped");

    // Clean up decoys.
    let _ = std::fs::remove_dir_all(&test_root);
    let _ = std::fs::remove_file(&support_decoy);
    let _ = std::fs::remove_file(&tmp_decoy);
    let _ = std::fs::remove_file(compile::cache_dir().join("pwned.sty"));
    let _ = std::fs::remove_file(&targets[2].0);
    let _ = std::fs::remove_dir_all(&build);

    println!("\n{} passed, {} failed", r.pass, r.fail);
    if r.fail == 0 { 0 } else { 1 }
}
