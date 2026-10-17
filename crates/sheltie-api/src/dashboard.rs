//! The read-only dashboard (`/dashboard`): queues, landings and leases per repository.
//!
//! Users sign in with GitHub through the App's own OAuth credentials and see only the
//! repositories of this App's installations they can access. Sessions are kept server-side
//! (the cookie holds a random id; the database only its SHA-256). Every value rendered into
//! HTML is escaped.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sheltie_core::{ChangeState, RepoFullName};
use sheltie_github::check_run::tier_words;
use sheltie_github::oauth::OAuthApp;
use sheltie_store::{DashboardSession, RepoDetail, RepoMode, RepoOverview, Store};

const SESSION_COOKIE: &str = "sheltie_session";
const STATE_COOKIE: &str = "sheltie_oauth_state";
const SESSION_TTL: Duration = Duration::from_secs(8 * 3600);
/// The dashboard's stylesheet.
pub const CSS: &str = include_str!("../assets/dashboard.css");
/// The logo.
pub const LOGO: &str = include_str!("../assets/logo.svg");

/// Settings for the dashboard.
#[derive(Debug, Clone)]
pub struct Dashboard {
    /// Sign-in with the App's OAuth credentials.
    pub oauth: Arc<OAuthApp>,
    /// The server's public URL, e.g. `https://sheltie.example.com` (the OAuth callback is
    /// `<public_url>/auth/callback`).
    pub public_url: String,
    /// GitHub's web URL, for links to pull requests and issues.
    pub github_web_url: String,
}

#[derive(Clone)]
pub(crate) struct DashState {
    pub(crate) store: Store,
    pub(crate) config: Dashboard,
}

/// Escapes text for HTML element content and attribute values.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Escapes `text`, then shows `backticked` parts as inline code.
pub fn inline_code(text: &str) -> String {
    esc(text)
        .split('`')
        .enumerate()
        .map(|(i, part)| {
            if i % 2 == 1 {
                format!("<code>{part}</code>")
            } else {
                part.to_owned()
            }
        })
        .collect()
}

fn guarantee_label(tier: sheltie_core::GuaranteeTier) -> &'static str {
    match tier {
        sheltie_core::GuaranteeTier::FootprintProven => "footprint-proven",
        sheltie_core::GuaranteeTier::SpeculationVerified => "speculative build",
        sheltie_core::GuaranteeTier::SweepVerified => "sweep",
    }
}

fn random_token() -> Result<String, StatusCode> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(hex::encode(bytes))
}

fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
}

impl DashState {
    fn set_cookie(&self, name: &str, value: &str, max_age: u64) -> HeaderValue {
        let secure = if self.config.public_url.starts_with("https://") {
            "; Secure"
        } else {
            ""
        };
        HeaderValue::from_str(&format!(
            "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
        ))
        .unwrap_or_else(|_| HeaderValue::from_static(""))
    }

    fn redirect_uri(&self) -> String {
        format!(
            "{}/auth/callback",
            self.config.public_url.trim_end_matches('/')
        )
    }

    async fn session(&self, headers: &HeaderMap) -> Option<DashboardSession> {
        let id = cookie(headers, SESSION_COOKIE)?;
        match self.store.dashboard_session(&hash(&id)).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "dashboard session lookup failed");
                None
            }
        }
    }
}

/// A complete dashboard page: `header` from [`top_bar`], `body` from the renderers.
pub fn page(title: &str, header: &str, body: &str) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{} · Sheltie</title><link rel="icon" href="/dashboard/assets/logo.svg" type="image/svg+xml"><link rel="stylesheet" href="/dashboard/assets/dashboard.css"></head>
<body>{header}<main class="wrap">{body}</main></body></html>"#,
        esc(title)
    )
}

fn html(status: StatusCode, title: &str, header: &str, body: &str) -> Response {
    let page = page(title, header, body);
    let mut res = (status, page).into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; style-src 'self'; img-src 'self'; form-action 'self'; frame-ancestors 'none'"),
    );
    res
}

/// The navy bar at the top: the brand, and who is signed in.
pub fn top_bar(login: Option<&str>) -> String {
    let who = match login {
        Some(login) => format!(
            r#"<div class="who">Signed in as <b>{}</b><form method="post" action="/auth/logout"><button>Sign out</button></form></div>"#,
            esc(login)
        ),
        None => String::new(),
    };
    format!(
        r#"<header class="top"><div class="in"><a class="brand" href="/dashboard"><img src="/dashboard/assets/logo.svg" alt="">Sheltie <span>dashboard</span></a>{who}</div></header>"#
    )
}

fn ago(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let s = (now - t).num_seconds().max(0);
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", s / 60),
        3600..86_400 => format!("{} h ago", s / 3600),
        _ => format!("{} d ago", s / 86_400),
    }
}

fn mode_pill(mode: RepoMode) -> &'static str {
    match mode {
        RepoMode::Merge => r#"<span class="pill mode-merge">merge mode</span>"#,
        RepoMode::Shadow => r#"<span class="pill mode-shadow">shadow mode</span>"#,
    }
}

fn state_pill(state: ChangeState) -> String {
    let label = match state {
        ChangeState::Committable => "landing",
        s => s.as_str(),
    };
    format!(r#"<span class="pill st-{}">{label}</span>"#, state.as_str())
}

/// The overview: one card per repository.
pub fn render_overview(repos: &[RepoOverview], now: DateTime<Utc>) -> String {
    let mut s = String::from(
        r#"<h1>Repositories</h1><p class="sub">Where Sheltie is installed and you have access.</p>"#,
    );
    if repos.is_empty() {
        s.push_str(r#"<div class="table"><div class="empty">No repositories yet. Install the GitHub App on a repository you can access.</div></div>"#);
        return s;
    }
    s.push_str(r#"<div class="repos">"#);
    for r in repos {
        let last = r.last_landed.map_or_else(
            || "Nothing landed yet".to_owned(),
            |t| format!("Last landed {}", ago(t, now)),
        );
        let _ = write!(
            s,
            r#"<a class="repo" href="/dashboard/{name}"><div class="name">{name}{mode}</div>
<div class="nums"><span><b>{}</b>ready</span><span><b>{}</b>in flight</span><span><b>{}</b>landed today</span><span><b>{}</b>returned</span></div>
<div class="last">{last}</div></a>"#,
            r.ready,
            r.in_flight,
            r.landed_today,
            r.returned_today,
            name = esc(r.name.as_str()),
            mode = mode_pill(r.mode),
        );
    }
    s.push_str("</div>");
    s
}

/// One repository: headline numbers, the queue, landings, returned changes and leases.
pub fn render_repo(d: &RepoDetail, web: &str, now: DateTime<Utc>) -> String {
    let o = &d.overview;
    let name = esc(o.name.as_str());
    let pr_link = |n: u64| format!(r#"<a href="{}/{name}/pull/{n}">#{n}</a>"#, esc(web));
    let title = |t: &Option<String>| t.as_deref().map(esc).unwrap_or_default();
    let mut s = format!(
        r#"<div class="crumbs"><a href="/dashboard">Repositories</a> /</div><h1>{name}{}{}</h1><p class="sub"><a href="{}/{name}">Open on GitHub</a></p>"#,
        mode_pill(o.mode),
        if o.merge_engine {
            r#"<span class="pill engine">merge engine</span>"#
        } else {
            ""
        },
        esc(web)
    );
    let _ = write!(
        s,
        r#"<div class="tiles"><div class="tile"><b>{}</b><span>Ready</span></div><div class="tile"><b>{}</b><span>In flight</span></div><div class="tile"><b>{}</b><span>Landed in 24 h</span></div><div class="tile"><b>{}</b><span>Returned in 24 h</span></div></div>"#,
        o.ready, o.in_flight, o.landed_today, o.returned_today
    );

    let _ = write!(
        s,
        r#"<h2>Queue <small>{} open</small></h2><div class="table">"#,
        d.queue.len()
    );
    if d.queue.is_empty() {
        s.push_str(r#"<div class="empty">No open pull requests.</div>"#);
    } else {
        s.push_str("<table><tr><th>Pull request</th><th>State</th><th>Footprint</th><th>Next</th><th>Updated</th></tr>");
        for q in &d.queue {
            let footprint = match (q.writes, q.reads) {
                (Some(w), Some(r)) => format!("writes {w} · affects {r}"),
                _ => r#"<span class="muted">not uploaded</span>"#.into(),
            };
            let _ = write!(
                s,
                r#"<tr><td class="pr">{} <span class="title">{}</span></td><td>{}</td><td class="num">{footprint}</td><td>{}</td><td class="muted num">{}</td></tr>"#,
                pr_link(q.pr.0),
                title(&q.title),
                state_pill(q.state),
                q.plan
                    .as_deref()
                    .map(|p| esc(&if o.mode == RepoMode::Merge {
                        p.replacen("would ", "will ", 1)
                    } else {
                        p.to_owned()
                    }))
                    .unwrap_or_else(|| "—".into()),
                ago(q.since, now)
            );
        }
        s.push_str("</table>");
    }
    s.push_str("</div>");

    s.push_str(r#"<h2>Recently landed</h2><div class="table">"#);
    if d.landed.is_empty() {
        s.push_str(r#"<div class="empty">Nothing landed through Sheltie yet.</div>"#);
    } else {
        s.push_str("<table><tr><th>Pull request</th><th>Commit</th><th>Proven by</th><th>Conflict</th><th>When</th></tr>");
        for l in &d.landed {
            let short = l.commit.as_str().get(..7).unwrap_or_default();
            let _ = write!(
                s,
                r#"<tr><td class="pr">{} <span class="title">{}</span></td><td><a href="{}/{name}/commit/{}"><code>{short}</code></a></td><td>{}</td><td class="muted">{}</td><td class="muted num">{}</td></tr>"#,
                pr_link(l.pr.0),
                title(&l.title),
                esc(web),
                l.commit.as_str(),
                guarantee_label(l.guarantee),
                l.resolution.map_or("—", tier_words),
                ago(l.at, now)
            );
        }
        s.push_str("</table>");
    }
    s.push_str("</div>");

    if !d.returned.is_empty() {
        s.push_str(r#"<h2>Returned to their authors</h2><div class="table"><table><tr><th>Pull request</th><th>Why</th><th>When</th></tr>"#);
        for r in &d.returned {
            let _ = write!(
                s,
                r#"<tr><td class="pr">{} <span class="title">{}</span></td><td>{}</td><td class="muted num">{}</td></tr>"#,
                pr_link(r.pr.0),
                title(&r.title),
                inline_code(&r.reason),
                ago(r.at, now)
            );
        }
        s.push_str("</table></div>");
    }

    if !d.leases.is_empty() {
        s.push_str(r#"<h2>Agents at work <small>claimed issues</small></h2><div class="table"><table><tr><th>Issue</th><th>Agent</th><th>Claim ends</th></tr>"#);
        for l in &d.leases {
            let mins = (l.expires_at - now).num_minutes().max(0);
            let _ = write!(
                s,
                r#"<tr><td class="pr"><a href="{}/{name}/issues/{n}">#{n}</a> <span class="title">{}</span></td><td><code>{}</code></td><td class="muted num">in {} h {} min</td></tr>"#,
                esc(web),
                title(&l.title),
                esc(&l.holder),
                mins / 60,
                mins % 60,
                n = l.issue,
            );
        }
        s.push_str("</table></div>");
    }
    s
}

fn signin_page() -> Response {
    html(
        StatusCode::OK,
        "Sign in",
        &top_bar(None),
        r#"<div class="signin"><img src="/dashboard/assets/logo.svg" alt=""><h1>Sheltie dashboard</h1><p class="sub">See the merge queue, landings and agents of the repositories you can access on GitHub.</p><a class="btn" href="/auth/login">Sign in with GitHub</a></div>"#,
    )
}

pub(crate) async fn overview(State(st): State<DashState>, headers: HeaderMap) -> Response {
    let Some(session) = st.session(&headers).await else {
        return signin_page();
    };
    match st.store.repo_overviews(&session.repos).await {
        Ok(repos) => html(
            StatusCode::OK,
            "Repositories",
            &top_bar(Some(&session.login)),
            &render_overview(&repos, Utc::now()),
        ),
        Err(e) => {
            tracing::error!(error = %e, "dashboard overview failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub(crate) async fn repo(
    State(st): State<DashState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> Response {
    let Some(session) = st.session(&headers).await else {
        return Redirect::to("/dashboard").into_response();
    };
    let not_found = || {
        html(
            StatusCode::NOT_FOUND,
            "Not found",
            &top_bar(Some(&session.login)),
            r#"<h1>Not found</h1><p class="sub">This repository does not exist, Sheltie is not installed on it, or you cannot access it. <a href="/dashboard">All repositories</a></p>"#,
        )
    };
    let Ok(full) = RepoFullName::parse(format!("{owner}/{name}")) else {
        return not_found();
    };
    let overviews = match st.store.repo_overviews(&session.repos).await {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(error = %e, "dashboard repo failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    // Only repositories from the user's own session are reachable.
    let Some(id) = overviews
        .iter()
        .find(|o| o.name.as_str().eq_ignore_ascii_case(full.as_str()))
        .map(|o| o.id)
    else {
        return not_found();
    };
    match st.store.repo_detail(id).await {
        Ok(Some(d)) => html(
            StatusCode::OK,
            d.overview.name.as_str(),
            &top_bar(Some(&session.login)),
            &render_repo(&d, &st.config.github_web_url, Utc::now()),
        ),
        Ok(None) => not_found(),
        Err(e) => {
            tracing::error!(error = %e, "dashboard repo failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub(crate) async fn login(State(st): State<DashState>) -> Response {
    let Ok(state) = random_token() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let mut res =
        Redirect::to(&st.config.oauth.authorize_url(&st.redirect_uri(), &state)).into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, st.set_cookie(STATE_COOKIE, &state, 600));
    res
}

#[derive(Debug, Deserialize)]
pub(crate) struct Callback {
    code: Option<String>,
    state: Option<String>,
}

pub(crate) async fn callback(
    State(st): State<DashState>,
    headers: HeaderMap,
    Query(q): Query<Callback>,
) -> Response {
    let fail = |why: &str| {
        html(
            StatusCode::BAD_REQUEST,
            "Sign-in failed",
            &top_bar(None),
            &format!(
                r#"<div class="signin"><h1>Sign-in failed</h1><p class="sub">{}</p><a class="btn" href="/auth/login">Try again</a></div>"#,
                esc(why)
            ),
        )
    };
    let expected = cookie(&headers, STATE_COOKIE);
    let (Some(code), Some(state)) = (q.code, q.state) else {
        return fail("GitHub did not send a sign-in code.");
    };
    if expected.as_deref() != Some(state.as_str()) {
        return fail("The sign-in link expired or was opened in another browser.");
    }
    let signed_in = match st.config.oauth.sign_in(&code, &st.redirect_uri()).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "dashboard sign-in failed");
            return fail("GitHub refused the sign-in. Please try again.");
        }
    };
    let Ok(session) = random_token() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    if let Err(e) = st
        .store
        .create_dashboard_session(
            &hash(&session),
            &signed_in.login,
            &signed_in.repos,
            SESSION_TTL,
        )
        .await
    {
        tracing::error!(error = %e, "creating a dashboard session failed");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    tracing::info!(login = %signed_in.login, repos = signed_in.repos.len(), "dashboard sign-in");
    let mut res = Redirect::to("/dashboard").into_response();
    let h = res.headers_mut();
    h.append(
        header::SET_COOKIE,
        st.set_cookie(SESSION_COOKIE, &session, SESSION_TTL.as_secs()),
    );
    h.append(header::SET_COOKIE, st.set_cookie(STATE_COOKIE, "", 0));
    res
}

pub(crate) async fn logout(State(st): State<DashState>, headers: HeaderMap) -> Response {
    if let Some(id) = cookie(&headers, SESSION_COOKIE)
        && let Err(e) = st.store.end_dashboard_session(&hash(&id)).await
    {
        tracing::error!(error = %e, "ending a dashboard session failed");
    }
    let mut res = Redirect::to("/dashboard").into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, st.set_cookie(SESSION_COOKIE, "", 0));
    res
}

pub(crate) async fn asset(Path(file): Path<String>) -> Response {
    let (body, kind) = match file.as_str() {
        "dashboard.css" => (CSS, "text/css; charset=utf-8"),
        "logo.svg" => (LOGO, "image/svg+xml"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let mut res = body.into_response();
    res.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping() {
        assert_eq!(
            esc(r#"<script>"a" & 'b'</script>"#),
            "&lt;script&gt;&quot;a&quot; &amp; &#39;b&#39;&lt;/script&gt;"
        );
    }

    #[test]
    fn inline_code_is_escaped_first() {
        assert_eq!(
            inline_code("onto `main` at <x>"),
            "onto <code>main</code> at &lt;x&gt;"
        );
        assert_eq!(inline_code("`<b>`"), "<code>&lt;b&gt;</code>");
    }

    #[test]
    fn cookies_are_parsed_strictly() {
        let mut h = HeaderMap::new();
        let id = "a".repeat(64);
        h.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("x=1; {SESSION_COOKIE}={id}")).unwrap(),
        );
        assert_eq!(cookie(&h, SESSION_COOKIE), Some(id));
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("sheltie_session=../../etc"),
        );
        assert_eq!(cookie(&h, SESSION_COOKIE), None);
    }

    #[test]
    fn titles_are_escaped() {
        use sheltie_core::{PrNumber, RepoId};
        use sheltie_store::QueueItem;
        let now = Utc::now();
        let d = RepoDetail {
            overview: RepoOverview {
                id: RepoId(1),
                name: RepoFullName::parse("acme/widgets").unwrap(),
                mode: RepoMode::Merge,
                merge_engine: true,
                pending: 0,
                ready: 1,
                in_flight: 0,
                landed_today: 0,
                returned_today: 0,
                last_landed: None,
            },
            queue: vec![QueueItem {
                pr: PrNumber(7),
                title: Some("<img src=x onerror=alert(1)>".into()),
                state: ChangeState::Ready,
                writes: Some(2),
                reads: Some(3),
                plan: Some("would land now".into()),
                since: now,
            }],
            landed: vec![],
            returned: vec![],
            leases: vec![],
        };
        let page = render_repo(&d, "https://github.com", now);
        assert!(!page.contains("<img src=x"), "{page}");
        assert!(page.contains("&lt;img src=x onerror=alert(1)&gt;"));
        assert!(page.contains("writes 2 · affects 3"));
    }
}
