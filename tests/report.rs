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
    let out = p.ctx_ok(&["--json", "task", "add", title, "--epic", "payments"]);
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
    let show = p.ctx_ok(&["show", &t]);
    assert!(
        show.contains("screenshot payment (payment-v1.png)"),
        "{show}"
    );

    // verify checks that it is there and that its hash matches.
    let out = p.ctx_ok(&["verify"]);
    assert!(out.contains("1 image"), "{out}");
    let path = p.home.join(img["value"].as_str().unwrap());
    fs::write(&path, b"another thing").unwrap();
    let out = p.ctx(&["verify"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no longer matches its hash"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    fs::remove_file(&path).unwrap();
    let out = p.ctx(&["verify"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("is not in the kontext-home"));
    fs::write(&path, &bytes).unwrap();
    p.ctx_ok(&["verify"]);

    // Without --screen, the screen is the file name; --screen without a screenshot is not valid.
    let t2 = add_task(&p, "Receipt");
    p.ctx_ok(&[
        "done",
        &t2,
        "--evidence",
        capture("email.png").to_str().unwrap(),
    ]);
    assert_eq!(evidence(&p, &t2)[0]["screen"], "email");
    let t3 = add_task(&p, "Other");
    let out = p.ctx(&["done", &t3, "--evidence", "cmd:\"x\"=0", "--screen", "x"]);
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
    let out = p.ctx(&[
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
    let out = p.ctx(&["done", &t, "--evidence", fake.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("is not a PNG, JPG or WebP image"));
}

#[test]
fn sync_commits_the_images() {
    let (p, _, _) = with_epic();
    let t = add_task(&p, "Payment screen");
    p.ctx_ok(&[
        "done",
        &t,
        "--evidence",
        capture("payment-v1.png").to_str().unwrap(),
    ]);
    // The first sync uploads everything; the second, what is new in media/.
    p.ctx_ok(&["sync"]);
    let t2 = add_task(&p, "Receipt");
    p.ctx_ok(&[
        "done",
        &t2,
        "--evidence",
        capture("email.png").to_str().unwrap(),
    ]);
    let out = p.ctx_ok(&["sync"]);
    assert!(!out.contains("no sube"), "{out}");
    let files = git(&p.home, &["ls-files", "media"]);
    assert_eq!(files.lines().count(), 2, "{files}");
}

#[test]
fn report_has_the_sections_in_order_and_no_history() {
    let (p, g, _) = with_epic();
    p.ctx_ok(&[
        "epic", "add", "Notices", "--goal", &g, "--name", "notices", "--after", "payments",
    ]);
    let t1 = add_task(&p, "Payment screen");
    p.ctx_ok(&[
        "done",
        &t1,
        "--evidence",
        capture("payment-v1.png").to_str().unwrap(),
        "--screen",
        "payment",
    ]);
    let t2 = add_task(&p, "Payment screen with 3DS");
    p.ctx_ok(&[
        "done",
        &t2,
        "--evidence",
        capture("payment-v2.png").to_str().unwrap(),
        "--screen",
        "payment",
    ]);
    p.ctx_ok(&["log", "Tried the Stripe SDK by hand"]);
    add_task(&p, "Email receipt");
    p.ctx_env_ok(
        &[("KONTEXT_ACTOR", "agent:pm"), ("KONTEXT_ROLE", "pm")],
        &["goal", "add", "Refunds"],
    );
    p.ctx_env_ok(
        &[("KONTEXT_ACTOR", "agent:pm"), ("KONTEXT_ROLE", "pm")],
        &[
            "log",
            "--status",
            "Card payments are ready. Your signature on refunds is still missing.",
        ],
    );
    let out_file = p.dir.path().join("report.html");
    let out = p.ctx_ok(&["report", "--html", "-o", out_file.to_str().unwrap()]);
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
    p.ctx_ok(&["report", "--html"]);
    p.ctx_ok(&["report"]);
    assert_eq!(p.entries("omar@test").len(), before);

    // With a pending item that stops work, the light turns red.
    let t3 = add_task(&p, "Webhooks");
    p.ctx_env_ok(
        &[("KONTEXT_ACTOR", "agent:impl-1")],
        &["ask", "Which key do I use?", "--task", &t3, "--to", "owner"],
    );
    let html = p.ctx_ok(&["report", "--html"]);
    assert!(html.contains("class=\"state s-red\""));
    assert!(html.contains("Work is stopped, waiting for you"));
    // In text, the same.
    let md = p.ctx_ok(&["report"]);
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
    p.ctx_ok(&["verify"]);
    let html = p.ctx_ok(&["report", "--html"]);
    assert!(
        html.contains(&format!("approved reference ({d})")),
        "mockup in the gallery"
    );
    // An agent that tries to decide leaves no attachment saved.
    let other = p.dir.path().join("other.png");
    fs::copy(capture("email.png"), &other).unwrap();
    let n = fs::read_dir(p.home.join("media")).unwrap().count();
    let out = p.ctx_env(
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
        p.ctx_env_ok(
            &[("KONTEXT_ACTOR", "agent:pm"), ("KONTEXT_ROLE", "pm")],
            &["goal", "add", g],
        );
    }
    let html = p.ctx_ok(&["report", "--html"]);

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
    let md = p.ctx_ok(&["report"]);
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
    p.ctx_ok(&[
        "done",
        &t,
        "--evidence",
        capture("payment-v1.png").to_str().unwrap(),
        "--screen",
        "payment",
    ]);
    add_task(&p, "Webhooks");
    p.ctx_env_ok(
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
    p.ctx_env_ok(
        &[("KONTEXT_ACTOR", "agent:pm"), ("KONTEXT_ROLE", "pm")],
        &["propose", "Save cards in Stripe", "--epic", "payments"],
    );
    let html = p.ctx_ok(&["report", "--html"]);

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
