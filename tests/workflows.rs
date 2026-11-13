//! The GitHub workflows pin every action by a full commit SHA, with the tag it
//! was taken from in a comment: a tag can be moved to other code, a SHA cannot.

use std::fs;
use std::path::Path;

#[test]
fn every_action_is_pinned_by_commit_sha() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut seen = 0;
    let mut bad = Vec::new();
    for f in fs::read_dir(&dir).unwrap() {
        let path = f.unwrap().path();
        if !matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("yml" | "yaml")
        ) {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap();
        for (i, line) in text.lines().enumerate() {
            let Some(rest) = line
                .trim_start()
                .trim_start_matches("- ")
                .strip_prefix("uses:")
            else {
                continue;
            };
            seen += 1;
            let rest = rest.trim();
            // A local action (`./…`) or a container has no tag to move.
            if rest.starts_with("./") || rest.starts_with("docker://") {
                continue;
            }
            let (action, comment) = match rest.split_once('#') {
                Some((a, c)) => (a.trim(), c.trim()),
                None => (rest, ""),
            };
            let sha = action.rsplit_once('@').map(|(_, r)| r).unwrap_or("");
            let pinned = sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit());
            if !pinned || comment.is_empty() {
                bad.push(format!(
                    "{}:{}: {}",
                    path.file_name().unwrap().to_string_lossy(),
                    i + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(seen > 0, "no `uses:` found in {}", dir.display());
    assert!(
        bad.is_empty(),
        "actions not pinned by a full commit SHA with the tag in a comment:\n{}",
        bad.join("\n")
    );
}
