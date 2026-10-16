//! A fake GitHub for integration tests: wiremock plus a throwaway App key.

use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rsa::{KeyPair, KeySize};
use base64::Engine as _;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub(crate) const INSTALLATION: u64 = 77;

/// A fresh RSA key in PKCS#8 PEM, generated per test run so no key is ever committed.
pub(crate) fn app_key_pem() -> Vec<u8> {
    let key = KeyPair::generate(KeySize::Rsa2048).unwrap();
    let der = AsDer::<aws_lc_rs::encoding::Pkcs8V1Der>::as_der(&key).unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(der.as_ref());
    let mut pem = String::from("-----BEGIN PRIVATE KEY-----\n");
    for line in b64.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(line).unwrap());
        pem.push('\n');
    }
    pem.push_str("-----END PRIVATE KEY-----\n");
    pem.into_bytes()
}

/// Serves installation tokens for [`INSTALLATION`]; expects exactly `times` token requests.
pub(crate) async fn mock_token(server: &MockServer, times: u64) {
    Mock::given(method("POST"))
        .and(path(format!(
            "/app/installations/{INSTALLATION}/access_tokens"
        )))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "token": "ghs_test_installation_token",
            "expires_at": "2099-01-01T00:00:00Z",
            "permissions": { "checks": "write" }
        })))
        .expect(times)
        .mount(server)
        .await;
}

/// A check run response body as GitHub returns it.
pub(crate) fn check_run_body(id: u64, head: &str) -> serde_json::Value {
    json!({
        "id": id,
        "node_id": "CR_test",
        "details_url": null,
        "head_sha": head,
        "url": format!("https://api.github.com/repos/acme/widgets/check-runs/{id}"),
        "html_url": null,
        "conclusion": null,
        "output": {
            "title": "t", "summary": "s", "text": null,
            "annotations_count": 0, "annotations_url": "https://example.invalid"
        },
        "started_at": null,
        "completed_at": null,
        "name": "Sheltie",
        "pull_requests": []
    })
}
