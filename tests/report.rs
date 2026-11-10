//! Screenshots as evidence (`done --evidence screenshot.png`, `decide --attach`)
//! and the report for the human (`kontext report`).

mod common;

use std::fs;
use std::path::PathBuf;

use common::{Project, git, implementer, with_epic};
use serde_json::Value;

fn capture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples/screenshots")
        .join(name)
}

fn add_task(p: &Project, title: &str) -> String {
    let out = p.kontext_ok(&["--json", "task", "add", title, "--epic", "payments"]);
    serde_json::from_str::<Value>(&out).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The evidence of a task's `done`.
fn evidence(p: &Project, task: &str) -> Vec<Value> {
    let entries = p.entries("omar@test");
    let done = entries
        .iter()
        .rev()
        .find(|e| e["kind"] == "done" && e["refs"][0] == task)
        .unwrap();
    done["fields"]["evidence"].as_array().unwrap().clone()
}

#[test]
fn images_are_stored_by_content_with_their_screen() {
    let (p, _, _) = with_epic();
    let t = add_task(&p, "Payment screen");
    let (_, mark) = p.claim("impl-1", "payments");
    let png = capture("payment-v1.png");
    p.run_as_ok(
        &implementer("impl-1"),
        &[
            "done",
            &t,
            "--since",
            &mark,
            "--evidence",
            "cmd:\"pnpm e2e\"=0",
            "--evidence",
            png.to_str().unwrap(),
            "--screen",
            "payment",
        ],
    );
    let ev = evidence(&p, &t);
    let img = ev.iter().find(|e| e["type"] == "image").unwrap();
    let bytes = fs::read(&png).unwrap();
    let sha = img["sha256"].as_str().unwrap();
    assert_eq!(sha.len(), 64);
    assert_eq!(img["value"], format!("media/{sha}.png"));
    assert_eq!(img["name"], "payment-v1.png");
    assert_eq!(img["screen"], "payment");
    assert_eq!(img["bytes"], bytes.len());
    assert_eq!(
        fs::read(p.home.join(img["value"].as_str().unwrap())).unwrap(),
        bytes
    );
    let show = p.kontext_ok(&["show", &t]);
    assert!(
        show.contains("screenshot payment (payment-v1.png)"),
        "{show}"
    );

    // verify checks that it is there and that its hash matches.
    let out = p.kontext_ok(&["verify"]);
    assert!(out.contains("1 image"), "{out}");
    let path = p.home.join(img["value"].as_str().unwrap());
    fs::write(&path, b"another thing").unwrap();
    let out = p.kontext(&["verify"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no longer matches its hash"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    fs::remove_file(&path).unwrap();
    let out = p.kontext(&["verify"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("is not in the kontext-home"));
    fs::write(&path, &bytes).unwrap();
    p.kontext_ok(&["verify"]);

    // Without --screen, the screen is the file name; --screen without a screenshot is not valid.
    let t2 = add_task(&p, "Receipt");
    p.kontext_ok(&[
        "done",
        &t2,
        "--evidence",
        capture("email.png").to_str().unwrap(),
    ]);
    assert_eq!(evidence(&p, &t2)[0]["screen"], "email");
    let t3 = add_task(&p, "Other");
    let out = p.kontext(&["done", &t3, "--evidence", "cmd:\"x\"=0", "--screen", "x"]);
    assert!(!out.status.success());
}

#[test]
fn images_over_the_cap_are_refused_and_not_stored() {
    let (p, _, _) = with_epic();
    let toml = p.home.join("project.toml");
    let mut text = fs::read_to_string(&toml).unwrap();
    text.push_str("\n[media]\nmax_size = \"4KB\"\n");
    fs::write(&toml, text).unwrap();
    let t = add_task(&p, "Payment screen");
    let out = p.kontext(&[
        "done",
        &t,
        "--evidence",
        capture("payment-v2.png").to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("and the cap is 4 KB"), "{err}");
    assert!(!p.home.join("media").exists());
    assert_eq!(p.state()["tasks"][&t], "ready");
    // Neither is anything that is not an image.
    let fake = p.dir.path().join("no.png");
    fs::write(&fake, b"hello").unwrap();
    let out = p.kontext(&["done", &t, "--evidence", fake.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("is not a PNG, JPG or WebP image"));
}

#[test]
fn sync_commits_the_images() {
    let (p, _, _) = with_epic();
    let t = add_task(&p, "Payment screen");
    p.kontext_ok(&[
        "done",
        &t,
        "--evidence",
        capture("payment-v1.png").to_str().unwrap(),
    ]);
    // The first sync uploads everything; the second, what is new in media/.
    p.kontext_ok(&["sync"]);
    let t2 = add_task(&p, "Receipt");
    p.kontext_ok(&[
        "done",
        &t2,
        "--evidence",
        capture("email.png").to_str().unwrap(),
    ]);
    let out = p.kontext_ok(&["sync"]);
    assert!(!out.contains("no sube"), "{out}");
    let files = git(&p.home, &["ls-files", "media"]);
    assert_eq!(files.lines().count(), 2, "{files}");
}

#[test]
fn report_has_the_sections_in_order_and_no_history() {
    let (p, g, _) = with_epic();
    p.kontext_ok(&[
        "epic", "add", "Notices", "--goal", &g, "--name", "notices", "--after", "payments",
    ]);
    let t1 = add_task(&p, "Payment screen");
    p.kontext_ok(&[
        "done",
        &t1,
        "--evidence",
        capture("payment-v1.png").to_str().unwrap(),
        "--screen",
        "payment",
    ]);
    let t2 = add_task(&p, "Payment screen with 3DS");
    p.kontext_ok(&[
        "done",
        &t2,
        "--evidence",
        capture("payment-v2.png").to_str().unwrap(),
        "--screen",
        "payment",
    ]);
    p.kontext_ok(&["log", "Tried the Stripe SDK by hand"]);
    add_task(&p, "Email receipt");
    p.kontext_env_ok(
        &[("KONTEXT_ACTOR", "agent:pm"), ("KONTEXT_ROLE", "pm")],
        &["goal", "add", "Refunds"],
    );
    p.kontext_env_ok(
        &[("KONTEXT_ACTOR", "agent:pm"), ("KONTEXT_ROLE", "pm")],
        &[
            "log",
            "--status",
            "Card payments are ready. Your signature on refunds is still missing.",
        ],
    );
    let out_file = p.dir.path().join("report.html");
    let out = p.kontext_ok(&["report", "--html", "-o", out_file.to_str().unwrap()]);
    assert!(out.contains("2 screenshots"), "{out}");
    let html = fs::read_to_string(&out_file).unwrap();

    // The sections, in this order.
    let mut last = 0;
    for needle in [
        "Card payments are ready. Your signature on refunds is still missing.",
        "id=\"your-turn\"",
        ">Your turn<",
        "id=\"problems\"",
        ">Problems<",
        "id=\"what-it-looks-like\"",
        ">What it looks like<",
        "id=\"progress\"",
        ">Progress<",
    ] {
        // After the sidebar, which names them too.
        let at = html[last..]
            .find(needle)
            .map(|i| i + last)
            .unwrap_or_else(|| panic!("missing {needle:?}"));
        assert!(at >= last, "{needle:?} out of order");
        last = at;
    }
    // The light, what is up to you with its command, the before and after.
    assert!(
        html.contains("class=\"state s-amber\""),
        "nothing is stopped"
    );
    assert!(html.contains("Goal: Refunds"));
    assert!(html.contains("kontext sign G-"));
    assert!(html.contains("<b>Before</b>") && html.contains("<b>Now</b>"));
    assert_eq!(html.matches("data:image/png;base64,").count(), 2);
    assert!(
        html.contains("1 of 3") || html.contains("2 of 3"),
        "progress bar of the goal"
    );
    assert!(html.contains("Wave 1"));
    // The page carries its script and still has no internet.
    assert!(html.contains("<script>") && !html.contains("<script src"));
    // No history: neither closed tasks, nor the journal, nor the internet.
    for gone in [
        "Payment screen with 3DS",
        "Tried the Stripe SDK",
        "Email receipt",
        "http://",
        "https://",
    ] {
        assert!(!html.contains(gone), "the report does not carry {gone:?}");
    }
    // Generating it writes nothing.
    let before = p.entries("omar@test").len();
    p.kontext_ok(&["report", "--html"]);
    p.kontext_ok(&["report"]);
    assert_eq!(p.entries("omar@test").len(), before);

    // With a pending item that stops work, the light turns red.
    let t3 = add_task(&p, "Webhooks");
    p.kontext_env_ok(
        &[("KONTEXT_ACTOR", "agent:impl-1")],
        &["ask", "Which key do I use?", "--task", &t3, "--to", "owner"],
    );
    let html = p.kontext_ok(&["report", "--html"]);
    assert!(html.contains("class=\"state s-red\""));
    assert!(html.contains("Work is stopped, waiting for you"));
    // In text, the same.
    let md = p.kontext_ok(&["report"]);
    assert!(md.starts_with("# Where we are in demo"), "{md}");
    assert!(
        md.contains("## Your turn") && md.contains("## Progress"),
        "{md}"
    );
}

#[test]
fn a_decision_can_carry_the_approved_mockup() {
    let (p, _, _) = with_epic();
    let page = p.dir.path().join("maqueta.html");
    fs::write(&page, "<!doctype html><p>Maqueta</p>").unwrap();
    let d = p.id_of(
        &[],
        &[
            "decide",
            "The payment screen follows the mockup",
            "--epic",
            "payments",
            "--attach",
            capture("payment-v2.png").to_str().unwrap(),
            "--attach",
            page.to_str().unwrap(),
            "--screen",
            "payment",
        ],
    );
    let e = p
        .entries("omar@test")
        .into_iter()
        .find(|e| e["id"] == d.as_str())
        .unwrap();
    let att = e["fields"]["attachments"].as_array().unwrap();
    assert_eq!(att[0]["type"], "image");
    assert_eq!(att[0]["screen"], "payment");
    assert_eq!(att[1]["type"], "page");
    assert!(att[1]["value"].as_str().unwrap().ends_with(".html"));
    p.kontext_ok(&["verify"]);
    let html = p.kontext_ok(&["report", "--html"]);
    assert!(
        html.contains(&format!("approved reference ({d})")),
        "mockup in the gallery"
    );
    // An agent that tries to decide leaves no attachment saved.
    let other = p.dir.path().join("other.png");
    fs::copy(capture("email.png"), &other).unwrap();
    let n = fs::read_dir(p.home.join("media")).unwrap().count();
    let out = p.kontext_env(
        &[("KONTEXT_ACTOR", "agent:x")],
        &["decide", "Other", "--attach", other.to_str().unwrap()],
    );
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(fs::read_dir(p.home.join("media")).unwrap().count(), n);
}

#[test]
fn report_carries_every_pending_with_the_rest_folded_and_its_script() {
    let (p, _, _) = with_epic();
    let goals = [
        "Refunds from the panel",
        "PDF invoices",
        "SMS reminders",
        "Charge in dollars",
        "Reconciliation panel",
        "OXXO payments",
        "Exportar a contabilidad",
    ];
    for g in goals {
        p.kontext_env_ok(
            &[("KONTEXT_ACTOR", "agent:pm"), ("KONTEXT_ROLE", "pm")],
            &["goal", "add", g],
        );
    }
    let html = p.kontext_ok(&["report", "--html"]);

    // Every pending item comes: the first 5 in view and the rest folded under
    // "And N more", which opens without script (<details>).
    for g in goals {
        assert!(html.contains(&format!("Goal: {g}")), "missing {g:?}");
    }
    let fold = html
        .find("<details class=\"fold fold-todos\">")
        .expect("the rest is folded");
    assert_eq!(html[..fold].matches("<li class=\"todo").count(), 5);
    assert_eq!(html[fold..].matches("<li class=\"todo").count(), 2);
    assert!(html[fold..].contains("And 2 more"));
    assert!(html.contains("class=\"count\">7<"), "how many are theirs");
    // Each command, ready to copy.
    assert_eq!(
        html.matches("<span class=\"cmdbox main\"><code class=\"cmd\">kontext sign G-")
            .count(),
        7
    );

    // The script is inline, and nothing goes out to the network: no scripts, no sheets, no
    // fonts (the font is embedded), no images.
    assert_eq!(html.matches("<script>").count(), 1);
    assert!(html.contains(kontext::report::inline_script()));
    assert!(html.contains("navigator.clipboard"));
    assert!(html.contains("data:font/woff2;base64,"));
    assert!(html.contains(&format!(
        "<style>{}</style>",
        kontext::report::inline_style()
    )));
    let lower = html.to_lowercase();
    for gone in [
        "http",
        "src=\"//",
        "url(//",
        "<link",
        "<script src",
        "@import",
    ] {
        assert!(!lower.contains(gone), "the report does not cite {gone:?}");
    }
    // It writes and sends nothing: no forms or requests.
    for gone in [
        "<form",
        "fetch(",
        "xmlhttprequest",
        "sendbeacon",
        "method=\"post\"",
    ] {
        assert!(!lower.contains(gone), "the report does not carry {gone:?}");
    }
    // In text, the rest is counted.
    let md = p.kontext_ok(&["report"]);
    assert!(md.contains("and 2 more: `kontext inbox --all`"), "{md}");
}

#[test]
fn report_has_a_sidebar_and_marks_each_card_with_its_epics() {
    let (p, g, payments) = with_epic();
    let avisos = p.id_of(
        &[],
        &[
            "epic",
            "add",
            "Payment notices",
            "--goal",
            &g,
            "--name",
            "notices",
            "--after",
            "payments",
        ],
    );
    let t = add_task(&p, "Payment screen");
    p.kontext_ok(&[
        "done",
        &t,
        "--evidence",
        capture("payment-v1.png").to_str().unwrap(),
        "--screen",
        "payment",
    ]);
    add_task(&p, "Webhooks");
    p.kontext_env_ok(
        &[("KONTEXT_ACTOR", "agent:avisos-impl")],
        &[
            "ask",
            "Send SMS besides email?",
            "--epic",
            "notices",
            "--to",
            "owner",
        ],
    );
    p.kontext_env_ok(
        &[("KONTEXT_ACTOR", "agent:pm"), ("KONTEXT_ROLE", "pm")],
        &["propose", "Save cards in Stripe", "--epic", "payments"],
    );
    let html = p.kontext_ok(&["report", "--html"]);

    // The sidebar: project and status, sections with how many, epics and how
    // to regenerate it; on a phone, a bar with "Sections".
    let side = &html[html
        .find("<aside class=\"side\" id=\"lateral\"")
        .expect("barra lateral")..html.find("</aside>").unwrap()];
    assert!(side.contains("class=\"side-project\">demo<"), "{side}");
    assert!(side.contains("class=\"mini-signal\" role=\"img\" aria-label=\"Status light: "));
    assert!(side.contains("Your last action: "), "{side}");
    for (id, label) in [
        ("your-turn", "Your turn"),
        ("problems", "Problems"),
        ("what-it-looks-like", "What it looks like"),
        ("progress", "Progress"),
    ] {
        assert!(
            side.contains(&format!(
                "<a href=\"#{id}\" data-nav=\"{id}\"><span>{label}</span>"
            )),
            "{label}: {side}"
        );
        assert!(
            html.contains(&format!("id=\"{id}\"")),
            "section {id} exists"
        );
    }
    assert!(
        side.contains("<span>Your turn</span><span class=\"n\">2</span>"),
        "{side}"
    );
    assert!(
        side.contains("<span>What it looks like</span><span class=\"n\">1</span>"),
        "{side}"
    );
    assert!(
        side.contains("<span>Progress</span></a>"),
        "Progress has no counter"
    );
    for (id, title, wave) in [
        (&payments, "Card payments", "Wave 1"),
        (&avisos, "Payment notices", "Wave 2"),
    ] {
        let at = side
            .find(&format!("data-epic-row=\"{id}\""))
            .unwrap_or_else(|| panic!("epic {title} is missing"));
        let row = &side[at..at + side[at..].find("</li>").unwrap()];
        assert!(row.contains(title) && row.contains(wave), "{row}");
        assert!(row.contains("<progress"), "mini barra: {row}");
        assert!(
            row.contains("class=\"wait\""),
            "something of {title} waits for you: {row}"
        );
    }
    assert!(side.contains("class=\"epic-all js-only\" aria-pressed=\"true\">All<"));
    assert!(side.contains("<code class=\"cmd\">kontext report --html</code>"));
    assert!(html.contains("<header class=\"bar-top\">"));
    assert!(html.contains("aria-controls=\"lateral\">Sections</button>"));

    // Each card names its epics, for the filter: the question, the avisos one;
    // the proposal, the payments one; the screenshot, its task's.
    let card = |needle: &str| {
        let at = html
            .find(needle)
            .unwrap_or_else(|| panic!("missing {needle:?}"));
        let start = html[..at].rfind("<li class=\"todo").unwrap();
        html[start..at].to_string()
    };
    assert!(
        card("Send SMS besides email?").contains(&format!("data-epic=\"{avisos}\"")),
        "the question is from notices"
    );
    assert!(
        card("Save cards in Stripe").contains(&format!("data-epic=\"{payments}\"")),
        "the proposal is from payments"
    );
    assert!(html.contains(&format!(
        "<article class=\"shot\" data-epic=\"{payments}\">"
    )));
    // Everything that can be filtered has data-epic; the ids are of epics that exist.
    for li in html.split("<li class=\"todo").skip(1) {
        assert!(li[..li.find('>').unwrap()].contains("data-epic=\""), "{li}");
    }
    for chunk in html.split("data-epic=\"").skip(1) {
        let ids = &chunk[..chunk.find('"').unwrap()];
        for id in ids.split_whitespace() {
            assert!(id == payments || id == avisos, "{id} is not an epic");
        }
    }

    // The main command goes in its box; the other way out, on a discreet line
    // (the script gives them "Copy" or an icon).
    let c = card("Save cards in Stripe");
    let full = &html[html.find(&c).unwrap()..];
    let full = &full[..full.find("</li>").unwrap()];
    assert!(
        full.contains("<span class=\"cmdbox main\"><code class=\"cmd\">kontext accept P-"),
        "{full}"
    );
    assert!(
        full.contains("<p class=\"alt\"><span class=\"or\">or</span><span class=\"cmdbox mini\"><code class=\"cmd\">kontext reject P-"),
        "{full}"
    );
}

// ------------------------------------------------------------ your ideas

const IDEAS_HEADER: &str =
    "| id | idea | source | state | where | evidence |\n|---|---|---|---|---|---|\n";

/// `kontext <args>` run from `dir`, at a fixed instant (so two runs compare).
fn report_in(p: &Project, dir: &std::path::Path, args: &[&str]) -> String {
    let out = p
        .cmd(args)
        .current_dir(dir)
        .env("KONTEXT_NOW", "2030-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "kontext {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A git repo `<tmp>/<name>`, with `.pm/items.md` if `items` is given.
fn repo_with_pm(p: &Project, name: &str, items: Option<&str>) -> PathBuf {
    let repo = p.dir.path().join(name);
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    if let Some(items) = items {
        fs::create_dir_all(repo.join(".pm/shots")).unwrap();
        fs::write(repo.join(".pm/items.md"), items).unwrap();
    }
    repo
}

/// The "Your ideas" section of a page.
fn ideas_section(html: &str) -> &str {
    let at = html
        .find("<section class=\"block ideas-block\"")
        .expect("the section is there");
    &html[at..at + html[at..].find("</section>").unwrap()]
}

#[test]
fn without_ideas_the_report_does_not_change() {
    let (p, _, _) = with_epic();
    p.kontext_ok(&["log", "--status", "Payments work in test."]);
    let bare = repo_with_pm(&p, "bare", None);
    let empty = repo_with_pm(&p, "empty", Some(&format!("# Ideas\n\n{IDEAS_HEADER}")));
    let one = repo_with_pm(
        &p,
        "one",
        Some(&format!(
            "{IDEAS_HEADER}| P1 | CSV export | | pending | | |\n"
        )),
    );
    for args in [&["report", "--html"][..], &["report"][..]] {
        let base = report_in(&p, &bare, args);
        // A .pm/items.md with no ideas, and a folder that is not a repo: the same page.
        assert_eq!(report_in(&p, &empty, args), base, "{args:?}");
        assert_eq!(report_in(&p, p.dir.path(), args), base, "{args:?}");
        // (The shared script names the section id; the markup does not have it.)
        assert!(!base.contains("id=\"your-ideas\"") && !base.contains("Your ideas"));
        // With one idea, the section comes.
        let with = report_in(&p, &one, args);
        assert_ne!(with, base);
        assert!(with.contains("Your ideas"), "{with}");
    }
    let base = report_in(&p, &bare, &["report", "--html"]);
    assert_eq!(base.matches("<style>").count(), 1);
    assert_eq!(base.matches("<script>").count(), 1);
    assert!(!base.contains(kontext::report::ideas_style()));
    assert!(!base.contains(kontext::report::ideas_script()));
    assert!(!base.contains("nav-ideas"));
}

#[test]
fn report_shows_your_ideas_where_they_went_and_their_evidence() {
    let (p, _, _) = with_epic();
    let q = p.id_of(&[], &["ask", "Which CSV columns?", "--to", "owner"]);
    let d = p.id_of(&[], &["decide", "Invoices export as CSV", "--why", "asked"]);
    let csv = fs::read(capture("email.png")).unwrap();
    let secret = fs::read(capture("payment-v1.png")).unwrap();
    let repo = repo_with_pm(
        &p,
        "app",
        Some(&format!(
            "# Ideas\n\n{IDEAS_HEADER}\
             | P1 | Know <b>\"what\"</b> is done | dumps/2026-11-07.md:3 | pending | | |\n\
             | P2 | CSV columns | dumps/2026-11-07.md:4 | pending | {q} | |\n\
             | P3 | CSV export | dumps/2026-11-02.md:1 | done | {d} | PR #12, shots/csv.png |\n\
             | P4 | Yearly numbering | | decided | {d} | shots/../secret.png, shots/missing.png, shots/notes.txt |\n\
             | P5 | Fax the invoices | dumps/2026-11-07.md:4 | discarded | | |\n"
        )),
    );
    let pm = repo.join(".pm");
    fs::create_dir_all(pm.join("dumps")).unwrap();
    fs::write(
        pm.join("dumps/2026-11-07.md"),
        "## 2026-11-07 10:42\n\n<script>alert(\"x\")</script> I need to know what is done\nwhich columns go in the CSV\n",
    )
    .unwrap();
    fs::write(
        pm.join("dumps/2026-11-02.md"),
        "export the invoices as CSV\n",
    )
    .unwrap();
    fs::write(pm.join("shots/csv.png"), &csv).unwrap();
    // A real image outside shots/: it never goes in the page.
    fs::write(pm.join("secret.png"), &secret).unwrap();
    fs::write(pm.join("shots/notes.txt"), "not an image").unwrap();

    let html = report_in(&p, &repo, &["report", "--html"]);
    // The section goes after "Problems" and before "What it looks like"; its
    // link, right after "Your turn".
    let ideas_at = html.find("id=\"your-ideas\"").unwrap();
    assert!(html.find("id=\"problems\"").unwrap() < ideas_at);
    assert!(ideas_at < html.find("id=\"what-it-looks-like\"").unwrap());
    let turn = html.find("data-nav=\"your-turn\"").unwrap();
    let next = &html[turn..];
    let next = &next[next.find("</li>\n").unwrap() + 6..];
    assert!(
        next.starts_with(
            "<li class=\"nav-ideas\"><a href=\"#your-ideas\" data-nav=\"your-ideas\"><span>Your ideas</span><span class=\"n\">5</span></a></li>"
        ),
        "{next}"
    );
    // Its own style and script, in their own blocks.
    assert_eq!(html.matches("<style>").count(), 2);
    assert_eq!(html.matches("<script>").count(), 2);
    assert!(html.contains(kontext::report::ideas_script()));

    let sec = ideas_section(&html);
    assert!(sec.contains("<span class=\"count\">5</span>"), "{sec}");
    // A filter per state that has ideas, and "All"; the groups in order, the
    // discarded last; the proof strip in the same order.
    let states: Vec<&str> = sec
        .split("class=\"st-btn\" data-state=\"")
        .skip(1)
        .map(|s| &s[..s.find('"').unwrap()])
        .collect();
    assert_eq!(
        states,
        ["", "nowhere", "waiting", "decided", "done", "discarded"]
    );
    let groups: Vec<&str> = sec
        .split("<div class=\"idea-group\" data-state=\"")
        .skip(1)
        .map(|s| &s[..s.find('"').unwrap()])
        .collect();
    assert_eq!(
        groups,
        ["nowhere", "waiting", "decided", "done", "discarded"]
    );
    let cells: Vec<&str> = sec
        .split("<a class=\"cell st-")
        .skip(1)
        .map(|s| &s[..s.find('"').unwrap()])
        .collect();
    assert_eq!(
        cells,
        ["nowhere", "waiting", "decided", "done", "discarded"]
    );
    assert!(sec.contains("href=\"#idea-P1\"") && sec.contains("id=\"idea-P1\""));
    assert!(sec.contains("You dropped it."));
    assert!(
        sec.contains("<time datetime=\"2026-11-07\">7 Nov</time>"),
        "{sec}"
    );
    assert!(
        sec.contains(&format!("<p class=\"went\">{q} · Which CSV columns?</p>")),
        "{sec}"
    );

    // User text is escaped: the title, the quote.
    assert!(!html.contains("<script>alert"), "the quote runs nothing");
    assert!(
        sec.contains("&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;"),
        "{sec}"
    );
    assert!(
        sec.contains("<h4>Know &lt;b&gt;&quot;what&quot;&lt;/b&gt; is done</h4>"),
        "{sec}"
    );

    // The screenshot in shots/ is embedded; the one that leaves shots/ with
    // `..`, the missing one and what is not an image stay as text.
    assert_eq!(sec.matches("data:image/png;base64,").count(), 1);
    assert!(sec.contains(&kontext::media::base64(&csv)));
    assert!(!html.contains(&kontext::media::base64(&secret)));
    for chip in [
        "PR #12",
        "shots/../secret.png",
        "shots/missing.png",
        "shots/notes.txt",
    ] {
        assert!(
            sec.contains(&format!("<span class=\"ev\">{chip}</span>")),
            "{chip}: {sec}"
        );
    }

    // In text: the summary, the open ones, and only how many are settled.
    let md = report_in(&p, &repo, &["report"]);
    let at = md.find("## Your ideas").expect("the section");
    assert!(md.find("## Problems").unwrap() < at);
    assert!(at < md.find("## What it looks like").unwrap());
    let part = &md[at..md.find("## What it looks like").unwrap()];
    assert!(
        part.contains(
            "5 ideas · no destination 1 · waiting for you 1 · decided 1 · done 1 · discarded 1."
        ),
        "{part}"
    );
    assert!(
        part.contains("- No destination: P1 · Know <b>\"what\"</b> is done\n"),
        "{part}"
    );
    assert!(
        part.contains(&format!(
            "- Waiting for you: P2 · CSV columns → {q} · Which CSV columns?\n"
        )),
        "{part}"
    );
    assert!(
        !part.contains("P3") && !part.contains("P4") && !part.contains("P5"),
        "{part}"
    );
    assert!(
        part.contains("Decided 1 · done 1 · discarded 1: `kontext ideas` lists them all"),
        "{part}"
    );
}

#[cfg(unix)]
#[test]
fn a_screenshot_that_links_out_of_shots_is_not_embedded() {
    let (p, _, _) = with_epic();
    let repo = repo_with_pm(
        &p,
        "app",
        Some(&format!(
            "{IDEAS_HEADER}| P1 | a | | pending | | shots/link.png |\n"
        )),
    );
    let outside = p.dir.path().join("outside.png");
    fs::copy(capture("email.png"), &outside).unwrap();
    std::os::unix::fs::symlink(&outside, repo.join(".pm/shots/link.png")).unwrap();
    let html = report_in(&p, &repo, &["report", "--html"]);
    let sec = ideas_section(&html);
    assert!(!sec.contains("data:image/"), "{sec}");
    assert!(sec.contains("<span class=\"ev\">shots/link.png</span>"));
    // Nor does a `shots/` that is itself a link out of `.pm/`.
    let elsewhere = p.dir.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::copy(capture("email.png"), elsewhere.join("a.png")).unwrap();
    fs::remove_dir_all(repo.join(".pm/shots")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, repo.join(".pm/shots")).unwrap();
    fs::write(
        repo.join(".pm/items.md"),
        format!("{IDEAS_HEADER}| P1 | a | | pending | | shots/a.png |\n"),
    )
    .unwrap();
    let html = report_in(&p, &repo, &["report", "--html"]);
    let sec = ideas_section(&html);
    assert!(!sec.contains("data:image/"), "{sec}");
    assert!(sec.contains("<span class=\"ev\">shots/a.png</span>"));
}

#[test]
fn a_screenshot_over_the_media_cap_is_not_embedded() {
    let (p, _, _) = with_epic();
    p.add_repo_toml("[media]\nmax_size = \"4KB\"\n");
    let repo = repo_with_pm(
        &p,
        "app",
        Some(&format!(
            "{IDEAS_HEADER}| P1 | a | | pending | | shots/big.png |\n"
        )),
    );
    fs::copy(capture("payment-v2.png"), repo.join(".pm/shots/big.png")).unwrap();
    let html = report_in(&p, &repo, &["report", "--html"]);
    assert!(!ideas_section(&html).contains("data:image/"));
}

#[test]
fn a_table_without_where_warns_in_the_report() {
    let (p, _, _) = with_epic();
    let repo = repo_with_pm(
        &p,
        "app",
        Some("| id | idea | state |\n|---|---|---|\n| P1 | CSV export | done |\n"),
    );
    let warn = "items.md has no `where` column: every idea shows as no destination.";
    let html = report_in(&p, &repo, &["report", "--html"]);
    let sec = ideas_section(&html);
    assert!(
        sec.contains("<p class=\"ideas-warn\">items.md has no `where` column: every idea shows as no destination.</p>"),
        "{sec}"
    );
    let md = report_in(&p, &repo, &["report"]);
    assert!(md.contains(&format!("Warning: {warn}")), "{md}");
    let json: Value = serde_json::from_str(&report_in(&p, &repo, &["--json", "ideas"])).unwrap();
    assert_eq!(json["warnings"][0], warn);
    assert_eq!(json["ideas"][0]["state"], "no destination");
    assert!(report_in(&p, &repo, &["ideas"]).contains(&format!("warning: {warn}")));
}
