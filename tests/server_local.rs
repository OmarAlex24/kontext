//! The local kontext server the client tests run against (`server/dev.mjs`):
//! it starts, answers as a kontext server, and only the admin secret creates a
//! project. The protocol itself is tested in `server/test/` (`npm test`).

mod common;

use std::time::Duration;

use common::server::LocalServer;
use kontext::http::Http;
use serde_json::json;

#[test]
fn the_local_server_runs_the_worker() {
    let Some(s) = LocalServer::start() else {
        return;
    };
    let http = Http::new(Duration::from_secs(10));
    let r = http
        .request("GET", &format!("{}/v1/info", s.url), &[], None)
        .unwrap();
    assert_eq!(r.json().unwrap()["kontext_server"], 1);
    let body = json!({ "person": "omar", "writer": "omar@test" }).to_string();
    let url = format!("{}/v1/p/demo/create", s.url);
    let r = http
        .request(
            "POST",
            &url,
            &[("Authorization", "Bearer nope")],
            Some(body.as_bytes()),
        )
        .unwrap();
    assert_eq!(r.status, 401, "{}", r.text());
    let auth = format!("Bearer {}", s.admin);
    let r = http
        .request(
            "POST",
            &url,
            &[("Authorization", &auth)],
            Some(body.as_bytes()),
        )
        .unwrap();
    assert_eq!(r.status, 201, "{}", r.text());
    let token = r.json().unwrap()["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("kxt1."));
}
