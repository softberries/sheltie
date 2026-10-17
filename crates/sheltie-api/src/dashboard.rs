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
use sheltie_github::check_run::{DOCS_URL, tier_words};
use sheltie_github::oauth::OAuthApp;
use sheltie_store::{DashboardSession, RepoCard, RepoDetail, RepoMode, Savings, Store};

const SESSION_COOKIE: &str = "sheltie_session";
const STATE_COOKIE: &str = "sheltie_oauth_state";
const SESSION_TTL: Duration = Duration::from_secs(8 * 3600);
/// The dashboard's stylesheet.
pub const CSS: &str = include_str!("../assets/dashboard.css");
/// The logo.
pub const LOGO: &[u8] = include_bytes!("../assets/logo.png");
/// Geist (SIL Open Font License 1.1, `assets/fonts/OFL.txt`).
pub const FONT_SANS: &[u8] = include_bytes!("../assets/fonts/Geist-Variable.woff2");
/// Geist Mono (SIL Open Font License 1.1).
pub const FONT_MONO: &[u8] = include_bytes!("../assets/fonts/GeistMono-Variable.woff2");

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
<meta name="color-scheme" content="light dark"><title>{} · Sheltie</title>
<link rel="icon" href="/dashboard/assets/logo.png" type="image/png">
<link rel="preload" href="/dashboard/assets/Geist-Variable.woff2" as="font" type="font/woff2" crossorigin>
<link rel="stylesheet" href="/dashboard/assets/dashboard.css"></head>
<body>{header}<main class="wrap">{body}</main>
<footer class="foot"><div class="wrap-in"><span>Sheltie · parallel merge engine</span><a href="{DOCS_URL}">Documentation</a></div></footer></body></html>"#,
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
        HeaderValue::from_static("default-src 'none'; style-src 'self'; img-src 'self'; font-src 'self'; form-action 'self'; frame-ancestors 'none'"),
    );
    res
}

/// The bar at the top: the brand, and who is signed in.
pub fn top_bar(login: Option<&str>) -> String {
    let who = match login {
        Some(login) => {
            let initial = login
                .chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_default();
            format!(
                r#"<div class="who"><span class="avatar" aria-hidden="true">{}</span><span class="login">{}</span><form method="post" action="/auth/logout"><button class="btn-ghost">Sign out</button></form></div>"#,
                esc(&initial),
                esc(login)
            )
        }
        None => String::new(),
    };
    format!(
        r#"<header class="top"><div class="wrap-in"><a class="brand" href="/dashboard"><img src="/dashboard/assets/logo.png" alt="" width="28" height="28"><span>Sheltie</span></a>{who}</div></header>"#
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

/// A duration for people: `45 s`, `12 min`, `4.2 h`, `3.5 d`.
pub fn duration(seconds: i64) -> String {
    let s = seconds.abs();
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", (s + 30) / 60),
        3600..172_800 => {
            let tenths = (s * 10 + 1800) / 3600;
            if tenths % 10 == 0 {
                format!("{} h", tenths / 10)
            } else {
                format!("{}.{} h", tenths / 10, tenths % 10)
            }
        }
        _ => {
            let tenths = (s * 10 + 43_200) / 86_400;
            format!("{}.{} d", tenths / 10, tenths % 10)
        }
    }
}

fn mode_badge(mode: RepoMode) -> &'static str {
    match mode {
        RepoMode::Merge => r#"<span class="badge b-green">Merge mode</span>"#,
        RepoMode::Shadow => r#"<span class="badge b-violet">Shadow mode</span>"#,
    }
}

fn state_badge(state: ChangeState) -> String {
    let (label, tone) = match state {
        ChangeState::Pending => ("Waiting", "b-gray"),
        ChangeState::Ready => ("Ready", "b-blue"),
        ChangeState::Verifying => ("Verifying", "b-amber"),
        ChangeState::Merging => ("Resolving", "b-violet"),
        ChangeState::Committable => ("Landing", "b-green"),
        ChangeState::Landed => ("Landed", "b-green"),
        ChangeState::Returned => ("Returned", "b-red"),
        ChangeState::Closed => ("Closed", "b-gray"),
    };
    format!(r#"<span class="badge {tone}">{label}</span>"#)
}

/// Bars of `values` (one per day, oldest first, the last one today), as an inline SVG with a
/// tooltip per bar. `class` sizes it.
fn day_bars(values: &[u32], now: DateTime<Utc>, class: &str, id: &str) -> String {
    let n = values.len().max(1);
    let max = values.iter().copied().max().unwrap_or(0).max(1);
    let (w, h, gap) = (n * 10, 48, 3);
    let mut s = format!(
        r#"<svg class="{class}" viewBox="0 0 {w} {h}" preserveAspectRatio="none" role="img" aria-label="Landings per day"><defs><linearGradient id="{id}" x1="0" y1="1" x2="0" y2="0"><stop class="stop-a" offset="0"/><stop class="stop-b" offset="1"/></linearGradient></defs>"#
    );
    for (i, &v) in values.iter().enumerate() {
        let bar = if v == 0 {
            2
        } else {
            (f64::from(v) / f64::from(max) * f64::from(h - 4))
                .round()
                .max(4.0) as u32
        };
        let back = i64::try_from(n - 1 - i).unwrap_or(0);
        let day = (now - chrono::Duration::days(back)).format("%b %-d");
        let plural = if v == 1 { "" } else { "s" };
        let _ = write!(
            s,
            r#"<rect class="{}" x="{}" y="{}" width="{}" height="{bar}" rx="1.5"{}><title>{day}: {v} landing{plural}</title></rect>"#,
            if v == 0 { "bar-zero" } else { "bar" },
            i * 10,
            h - bar,
            10 - gap,
            if v == 0 {
                String::new()
            } else {
                format!(r#" fill="url(#{id})""#)
            },
        );
    }
    s.push_str("</svg>");
    s
}

/// Two horizontal bars: Sheltie's median time to land against the one-at-a-time queue's.
fn compare_bars(sheltie: i64, serial: i64) -> String {
    let max = sheltie.max(serial).max(1) as f64;
    let value = |v: i64| ((v as f64 / max) * 100.0).max(2.0);
    format!(
        r#"<div class="compare"><div class="crow"><span class="clabel">Sheltie</span><progress class="pbar pbar-a" max="100" value="{:.1}" aria-hidden="true"></progress><span class="cval">{}</span></div><div class="crow"><span class="clabel">One at a time</span><progress class="pbar pbar-b" max="100" value="{:.1}" aria-hidden="true"></progress><span class="cval">{}</span></div></div>"#,
        value(sheltie),
        duration(sheltie),
        value(serial),
        duration(serial)
    )
}

/// The "Time saved" panel of a repository page.
fn savings_panel(sv: &Savings, mode: RepoMode, predicted_wait_minutes: Option<i64>) -> String {
    let days = sheltie_store::DETAIL_DAYS;
    let (headline, caption) = match (mode, sv.wait_saved_seconds, predicted_wait_minutes) {
        (RepoMode::Shadow, _, Some(m)) => (
            duration(m * 60),
            "less waiting predicted · shadow mode, Sheltie only predicts".to_owned(),
        ),
        (_, Some(w), _) if w >= 0 => (
            duration(w),
            "less waiting than a one-at-a-time merge queue".to_owned(),
        ),
        (_, Some(w), _) => (
            duration(w),
            "more waiting than a one-at-a-time merge queue".to_owned(),
        ),
        (_, None, _) if !sv.enough_data() => (
            "—".to_owned(),
            format!(
                "Not enough data yet: {} of {} landings needed",
                sv.landings,
                sheltie_store::MIN_LANDINGS
            ),
        ),
        (_, None, _) => (
            "—".to_owned(),
            "No CI time measured on the default branch yet".to_owned(),
        ),
    };
    let compare = match (sv.median_actual_seconds, sv.median_serial_seconds) {
        (Some(a), Some(b)) if sv.enough_data() => format!(
            r#"<div class="hero-side"><div class="kicker">Median time from ready to landed</div>{}<p class="fine">CI on the default branch takes {} (median, measured).</p></div>"#,
            compare_bars(a, b),
            sv.ci_seconds.map(duration).unwrap_or_default()
        ),
        _ => String::new(),
    };
    let author = sv.author_minutes_saved * 60;
    format!(
        r#"<section class="hero"><div class="hero-main"><div class="kicker">Time saved · last {days} days</div><div class="big">{headline}</div><p class="caption">{caption}</p></div>{compare}</section>
<div class="metrics">
<div class="metric"><span class="mlabel">Author time saved</span><b>{}</b><span class="mnote">{} min per conflict, {} per replay (assumed)</span></div>
<div class="metric"><span class="mlabel">Conflicts resolved</span><b>{}</b><span class="mnote">{} by the merge engine · {} re-derived</span></div>
<div class="metric"><span class="mlabel">Landed</span><b>{}</b><span class="mnote">through Sheltie</span></div>
<div class="metric"><span class="mlabel">Bad commits reverted</span><b>{}</b><span class="mnote">found and undone automatically</span></div>
</div>
<details class="how"><summary>How this is estimated</summary><p>Waiting time compares Sheltie's real landing times with a one-at-a-time merge queue simulated from the same ready times: each pull request lands one CI run after the later of its ready time and the previous landing. The CI time is the median measured on the default branch. Author time counts conflicts Sheltie resolved without the author, at the minutes per fix set in <code>[estimates]</code> in <code>.sheltie/merge.toml</code>. <a href="{DOCS_URL}/dashboard.html#time-saved">More</a></p></details>"#,
        duration(i64::try_from(author).unwrap_or(i64::MAX)),
        sv.minutes_per_conflict,
        sv.minutes_per_replay,
        sv.conflicts_resolved + sv.replays,
        sv.conflicts_resolved,
        sv.replays,
        sv.landings,
        sv.reverts,
    )
}

fn empty(text: &str) -> String {
    format!(r#"<div class="empty">{text}</div>"#)
}

/// The overview: totals, then one card per repository.
pub fn render_overview(cards: &[RepoCard], now: DateTime<Utc>) -> String {
    let mut s = String::from(
        r#"<div class="head"><div><div class="kicker">Overview</div><h1>Repositories</h1><p class="sub">Where Sheltie is installed and you have access.</p></div></div>"#,
    );
    if cards.is_empty() {
        s.push_str(&empty(r#"<b>No repositories yet</b>Install the GitHub App on a repository you can access. Installed it after signing in? <a href="/auth/login">Sign in again</a> to refresh your repositories."#));
        return s;
    }
    let waited: i64 = cards
        .iter()
        .filter_map(|c| c.savings.wait_saved_seconds)
        .filter(|w| *w > 0)
        .sum();
    let author: u64 = cards.iter().map(|c| c.savings.author_minutes_saved).sum();
    let landed: usize = cards.iter().map(|c| c.savings.landings).sum();
    let conflicts: u32 = cards
        .iter()
        .map(|c| c.savings.conflicts_resolved + c.savings.replays)
        .sum();
    let _ = write!(
        s,
        r#"<div class="totals"><div><span>Waiting saved</span><b>{}</b></div><div><span>Author time saved</span><b>{}</b></div><div><span>Landed</span><b>{landed}</b></div><div><span>Conflicts resolved</span><b>{conflicts}</b></div><p class="fine">Last {} days, all repositories.</p></div><div class="cards">"#,
        if waited > 0 {
            duration(waited)
        } else {
            "—".into()
        },
        duration(i64::try_from(author * 60).unwrap_or(i64::MAX)),
        sheltie_store::DETAIL_DAYS,
    );
    for (i, c) in cards.iter().enumerate() {
        let o = &c.overview;
        let (owner, repo) = o
            .name
            .as_str()
            .split_once('/')
            .unwrap_or(("", o.name.as_str()));
        let last = o.last_landed.map_or_else(
            || "Nothing landed yet".to_owned(),
            |t| format!("Last landed {}", ago(t, now)),
        );
        let saved = match (
            o.mode,
            c.savings.wait_saved_seconds,
            c.predicted_wait_minutes,
        ) {
            (RepoMode::Shadow, _, Some(m)) if m > 0 => {
                format!("{} less waiting predicted", duration(m * 60))
            }
            (_, Some(w), _) if w > 0 => format!("{} less waiting", duration(w)),
            _ => format!(
                "{} landed in {} days",
                c.savings.landings,
                sheltie_store::DETAIL_DAYS
            ),
        };
        let _ = write!(
            s,
            r#"<a class="card" href="/dashboard/{name}"><div class="card-top"><div class="rname"><span>{}/</span>{}</div>{}</div>
{}<div class="card-stats"><div><b>{}</b><span>Ready</span></div><div><b>{}</b><span>In flight</span></div><div><b>{}</b><span>Landed 24h</span></div><div><b>{}</b><span>Returned 24h</span></div></div>
<div class="card-foot"><span class="saved">{}</span><span>{last}</span></div></a>"#,
            esc(owner),
            esc(repo),
            mode_badge(o.mode),
            day_bars(&c.landed_daily, now, "spark", &format!("g{i}")),
            o.ready,
            o.in_flight,
            o.landed_today,
            o.returned_today,
            esc(&saved),
            name = esc(o.name.as_str()),
        );
    }
    s.push_str("</div>");
    s
}

/// One repository: time saved, activity, the queue, landings, returned changes and leases.
pub fn render_repo(d: &RepoDetail, web: &str, now: DateTime<Utc>) -> String {
    let o = &d.overview;
    let name = esc(o.name.as_str());
    let web = esc(web);
    let pr_link = |n: u64| format!(r#"<a class="num-chip" href="{web}/{name}/pull/{n}">#{n}</a>"#);
    let title = |t: &Option<String>| t.as_deref().map(esc).unwrap_or_default();
    let (owner, repo) = o
        .name
        .as_str()
        .split_once('/')
        .unwrap_or(("", o.name.as_str()));
    let mut s = format!(
        r#"<nav class="crumbs"><a href="/dashboard">Repositories</a><span>/</span>{}</nav><div class="head"><div><h1><span class="owner">{}/</span>{}</h1><div class="badges">{}{}</div></div><a class="btn-ghost" href="{web}/{name}">Open on GitHub ↗</a></div>"#,
        esc(owner),
        esc(owner),
        esc(repo),
        mode_badge(o.mode),
        if o.merge_engine {
            r#"<span class="badge b-sable">Merge engine</span>"#
        } else {
            ""
        },
    );
    s.push_str(&savings_panel(&d.savings, o.mode, d.predicted_wait_minutes));

    let _ = write!(
        s,
        r#"<section class="panel activity"><div class="panel-head"><h2>Activity</h2><span class="fine">Landings per day, last {} days</span></div>{}<div class="axis"><span>{}</span><span>Today</span></div>
<div class="now"><div><b>{}</b><span>Ready</span></div><div><b>{}</b><span>In flight</span></div><div><b>{}</b><span>Landed in 24 h</span></div><div><b>{}</b><span>Returned in 24 h</span></div></div></section>"#,
        sheltie_store::DETAIL_DAYS,
        day_bars(&d.landed_daily, now, "chart", "gchart"),
        (now - chrono::Duration::days(i64::from(sheltie_store::DETAIL_DAYS) - 1)).format("%b %-d"),
        o.ready,
        o.in_flight,
        o.landed_today,
        o.returned_today
    );

    let _ = write!(
        s,
        r#"<section class="panel"><div class="panel-head"><h2>Queue</h2><span class="count">{}</span></div>"#,
        d.queue.len()
    );
    if d.queue.is_empty() {
        s.push_str(&empty("<b>Queue is empty</b>No open pull requests."));
    } else {
        s.push_str(r#"<div class="scroll"><table><thead><tr><th>Pull request</th><th>State</th><th>Footprint</th><th>Next</th><th class="r">Updated</th></tr></thead><tbody>"#);
        for q in &d.queue {
            let footprint = match (q.writes, q.reads) {
                (Some(w), Some(r)) => format!(
                    r#"<span class="fp"><b>{w}</b> write{} · <b>{r}</b> affected</span>"#,
                    if w == 1 { "" } else { "s" }
                ),
                _ => r#"<span class="muted">not uploaded</span>"#.into(),
            };
            let plan = q
                .plan
                .as_deref()
                .map(|p| {
                    esc(&if o.mode == RepoMode::Merge {
                        p.replacen("would ", "will ", 1)
                    } else {
                        p.to_owned()
                    })
                })
                .unwrap_or_else(|| "—".into());
            let _ = write!(
                s,
                r#"<tr><td><div class="prcell">{}<span class="ttl">{}</span></div></td><td>{}</td><td>{footprint}</td><td class="plan">{plan}</td><td class="r muted nowrap">{}</td></tr>"#,
                pr_link(q.pr.0),
                title(&q.title),
                state_badge(q.state),
                ago(q.since, now)
            );
        }
        s.push_str("</tbody></table></div>");
    }
    s.push_str("</section>");

    s.push_str(r#"<section class="panel"><div class="panel-head"><h2>Recently landed</h2></div>"#);
    if d.landed.is_empty() {
        s.push_str(&empty(
            "<b>Nothing landed yet</b>Landings through Sheltie appear here.",
        ));
    } else {
        s.push_str(r#"<ol class="timeline">"#);
        for l in &d.landed {
            let short = l.commit.as_str().get(..7).unwrap_or_default();
            let conflict = l.resolution.map_or(String::new(), |t| {
                format!(
                    r#"<span class="badge b-sable">{}</span>"#,
                    esc(tier_words(t))
                )
            });
            let _ = write!(
                s,
                r#"<li><span class="dot" aria-hidden="true"></span><div class="tl-main">{}<span class="ttl">{}</span></div><div class="tl-meta"><a class="sha" href="{web}/{name}/commit/{}">{short}</a><span class="badge b-green">{}</span>{conflict}<span class="muted nowrap">{}</span></div></li>"#,
                pr_link(l.pr.0),
                title(&l.title),
                l.commit.as_str(),
                guarantee_label(l.guarantee),
                ago(l.at, now)
            );
        }
        s.push_str("</ol>");
    }
    s.push_str("</section>");

    if !d.returned.is_empty() {
        s.push_str(r#"<section class="panel"><div class="panel-head"><h2>Returned to their authors</h2></div><ul class="rows">"#);
        for r in &d.returned {
            let _ = write!(
                s,
                r#"<li><div class="prcell">{}<span class="ttl">{}</span><span class="muted nowrap push">{}</span></div><p class="why">{}</p></li>"#,
                pr_link(r.pr.0),
                title(&r.title),
                ago(r.at, now),
                inline_code(&r.reason),
            );
        }
        s.push_str("</ul></section>");
    }

    if !d.leases.is_empty() {
        s.push_str(r#"<section class="panel"><div class="panel-head"><h2>Agents at work</h2><span class="fine">Claimed issues</span></div><ul class="rows">"#);
        for l in &d.leases {
            let mins = (l.expires_at - now).num_minutes().max(0);
            let _ = write!(
                s,
                r#"<li><div class="prcell"><a class="num-chip" href="{web}/{name}/issues/{n}">#{n}</a><span class="ttl">{}</span><code class="push">{}</code><span class="muted nowrap">ends in {} h {} min</span></div></li>"#,
                title(&l.title),
                esc(&l.holder),
                mins / 60,
                mins % 60,
                n = l.issue,
            );
        }
        s.push_str("</ul></section>");
    }
    s
}

fn signin_page() -> Response {
    html(
        StatusCode::OK,
        "Sign in",
        &top_bar(None),
        r#"<div class="signin"><img src="/dashboard/assets/logo.png" alt="" width="64" height="64"><h1>Sheltie dashboard</h1><p class="sub">The merge queue, landings, time saved and agents of the repositories you can access on GitHub.</p><a class="btn" href="/auth/login"><svg viewBox="0 0 16 16" width="18" height="18" aria-hidden="true"><path fill="currentColor" d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z"/></svg>Sign in with GitHub</a><p class="fine">Read-only. Sheltie sees only repositories where its App is installed.</p></div>"#,
    )
}

pub(crate) async fn overview(State(st): State<DashState>, headers: HeaderMap) -> Response {
    let Some(session) = st.session(&headers).await else {
        return signin_page();
    };
    match st.store.repo_cards(&session.repos).await {
        Ok(cards) => html(
            StatusCode::OK,
            "Repositories",
            &top_bar(Some(&session.login)),
            &render_overview(&cards, Utc::now()),
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
    let (body, kind): (&'static [u8], &'static str) = match file.as_str() {
        "dashboard.css" => (CSS.as_bytes(), "text/css; charset=utf-8"),
        "logo.png" => (LOGO, "image/png"),
        "Geist-Variable.woff2" => (FONT_SANS, "font/woff2"),
        "GeistMono-Variable.woff2" => (FONT_MONO, "font/woff2"),
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

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-17T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn overview(mode: RepoMode) -> sheltie_store::RepoOverview {
        sheltie_store::RepoOverview {
            id: sheltie_core::RepoId(1),
            name: RepoFullName::parse("acme/widgets").unwrap(),
            mode,
            merge_engine: true,
            pending: 0,
            ready: 1,
            in_flight: 2,
            landed_today: 7,
            returned_today: 1,
            last_landed: Some(now() - chrono::Duration::minutes(5)),
        }
    }

    fn savings(landings: usize, saved: Option<i64>) -> Savings {
        Savings {
            landings,
            ci_seconds: Some(540),
            median_actual_seconds: Some(180),
            median_serial_seconds: Some(1500),
            wait_saved_seconds: saved,
            conflicts_resolved: 6,
            replays: 1,
            reverts: 1,
            author_minutes_saved: 6 * 15 + 30,
            minutes_per_conflict: 15,
            minutes_per_replay: 30,
        }
    }

    fn detail(mode: RepoMode, sv: Savings) -> RepoDetail {
        use sheltie_core::{CommitSha, GuaranteeTier, PrNumber, ResolutionTier};
        use sheltie_store::{LandingItem, QueueItem};
        RepoDetail {
            overview: overview(mode),
            landed_daily: vec![0, 2, 5, 1, 0, 0, 3],
            savings: sv,
            predicted_wait_minutes: (mode == RepoMode::Shadow).then_some(95),
            queue: vec![QueueItem {
                pr: PrNumber(7),
                title: Some("<img src=x onerror=alert(1)>".into()),
                state: ChangeState::Ready,
                writes: Some(2),
                reads: Some(3),
                plan: Some("would land now".into()),
                since: now(),
            }],
            landed: vec![LandingItem {
                pr: PrNumber(6),
                title: Some("Rename rate()".into()),
                commit: CommitSha::parse("a".repeat(40)).unwrap(),
                guarantee: GuaranteeTier::SpeculationVerified,
                resolution: Some(ResolutionTier::ReplayFull),
                at: now() - chrono::Duration::minutes(3),
            }],
            returned: vec![],
            leases: vec![],
        }
    }

    #[test]
    fn titles_are_escaped() {
        let page = render_repo(
            &detail(RepoMode::Merge, savings(12, Some(9000))),
            "https://github.com",
            now(),
        );
        assert!(!page.contains("<img src=x"), "{page}");
        assert!(page.contains("&lt;img src=x onerror=alert(1)&gt;"));
        assert!(page.contains("<b>2</b> writes · <b>3</b> affected"));
    }

    #[test]
    fn durations() {
        assert_eq!(duration(45), "45 s");
        assert_eq!(duration(12 * 60 + 10), "12 min");
        assert_eq!(duration(3600), "1 h");
        assert_eq!(duration(4 * 3600 + 720), "4.2 h");
        assert_eq!(duration(3 * 86_400 + 43_200), "3.5 d");
        assert_eq!(duration(-600), "10 min");
    }

    #[test]
    fn repository_page() {
        let html = render_repo(
            &detail(RepoMode::Merge, savings(12, Some(9000))),
            "https://github.com",
            now(),
        );
        insta::assert_snapshot!(html);
    }

    #[test]
    fn time_saved_states() {
        let panel = |mode, sv| savings_panel(&sv, mode, (mode == RepoMode::Shadow).then_some(95));
        assert!(
            panel(RepoMode::Merge, savings(12, Some(9000)))
                .contains("2.5 h</div><p class=\"caption\">less waiting")
        );
        assert!(panel(RepoMode::Merge, savings(12, Some(-600))).contains("more waiting"));
        assert!(panel(RepoMode::Merge, savings(4, None)).contains("4 of 10 landings needed"));
        assert!(
            panel(RepoMode::Shadow, savings(0, None))
                .contains("1.6 h</div><p class=\"caption\">less waiting predicted")
        );
    }

    #[test]
    fn overview_page() {
        let card = RepoCard {
            overview: overview(RepoMode::Merge),
            landed_daily: vec![1, 0, 4],
            savings: savings(12, Some(9000)),
            predicted_wait_minutes: None,
        };
        insta::assert_snapshot!(render_overview(&[card], now()));
    }
}
