//! Renders the dashboard with sample data into a folder, for reviewing the design and making
//! screenshots: `cargo run -p sheltie-api --example dashboard_preview -- <dir>`, then serve
//! `<dir>` and open `/dashboard/index.html`.

use std::path::PathBuf;

use chrono::{Duration, Utc};
use sheltie_api::dashboard::{
    CSS, FONT_MONO, FONT_SANS, LOGO, page, render_overview, render_repo, top_bar,
};
use sheltie_core::{
    ChangeState, CommitSha, GuaranteeTier, PrNumber, RepoFullName, RepoId, ResolutionTier,
};
use sheltie_store::{
    LandingItem, LeaseItem, QueueItem, RepoCard, RepoDetail, RepoMode, RepoOverview, ReturnedItem,
    Savings,
};

/// A plausible, repeatable landings-per-day series (weekends quieter).
fn daily(days: usize, scale: u32, seed: u32) -> Vec<u32> {
    (0..days)
        .map(|i| {
            let i = u32::try_from(i).unwrap_or(0);
            let weekday = (i + seed) % 7;
            let noise = (i.wrapping_mul(2_654_435_761) ^ seed.wrapping_mul(40_503)) >> 7;
            let base = if weekday >= 5 { scale / 5 } else { scale / 2 };
            base + noise % (scale + 1)
        })
        .collect()
}

/// `times`: CI, median with Sheltie, median one at a time (seconds); `counts`: conflicts,
/// replays, reverts.
fn savings(landings: usize, times: [i64; 3], saved: Option<i64>, counts: [u32; 3]) -> Savings {
    let [ci, actual, serial] = times;
    let [conflicts, replays, reverts] = counts;
    Savings {
        landings,
        ci_seconds: Some(ci),
        median_actual_seconds: Some(actual),
        median_serial_seconds: Some(serial),
        wait_saved_seconds: saved,
        conflicts_resolved: conflicts,
        replays,
        reverts,
        author_minutes_saved: u64::from(conflicts) * 15 + u64::from(replays) * 30,
        minutes_per_conflict: 15,
        minutes_per_replay: 30,
    }
}

fn main() -> std::io::Result<()> {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "target/dashboard-preview".into()),
    );
    let dir = out.join("dashboard");
    std::fs::create_dir_all(dir.join("assets"))?;
    std::fs::create_dir_all(dir.join("acme"))?;
    std::fs::write(dir.join("assets/dashboard.css"), CSS)?;
    std::fs::write(dir.join("assets/logo.svg"), LOGO)?;
    std::fs::write(dir.join("assets/Geist-Variable.woff2"), FONT_SANS)?;
    std::fs::write(dir.join("assets/GeistMono-Variable.woff2"), FONT_MONO)?;
    let now = Utc::now();
    let ago = |m: i64| now - Duration::minutes(m);
    let repo = |id: u64, name: &str, mode: RepoMode, n: [i64; 4], last: Option<i64>| RepoOverview {
        id: RepoId(id),
        name: RepoFullName::parse(name).unwrap_or_else(|_| unreachable!()),
        mode,
        merge_engine: mode == RepoMode::Merge,
        pending: 2,
        ready: n[0],
        in_flight: n[1],
        landed_today: n[2],
        returned_today: n[3],
        last_landed: last.map(ago),
    };
    let repos = [
        repo(1, "acme/payments", RepoMode::Merge, [3, 2, 41, 1], Some(4)),
        repo(
            2,
            "acme/storefront",
            RepoMode::Merge,
            [1, 1, 17, 0],
            Some(22),
        ),
        repo(
            3,
            "acme/data-pipeline",
            RepoMode::Shadow,
            [5, 0, 0, 0],
            None,
        ),
    ];
    let sha =
        |c: char| CommitSha::parse(c.to_string().repeat(40)).unwrap_or_else(|_| unreachable!());
    let q = |pr: u64,
             title: &str,
             state: ChangeState,
             fp: Option<(i64, i64)>,
             plan: Option<&str>,
             m: i64| QueueItem {
        pr: PrNumber(pr),
        title: Some(title.into()),
        state,
        writes: fp.map(|f| f.0),
        reads: fp.map(|f| f.1),
        plan: plan.map(Into::into),
        since: ago(m),
    };
    let l = |pr: u64, title: &str, c: char, g: GuaranteeTier, r: Option<ResolutionTier>, m: i64| {
        LandingItem {
            pr: PrNumber(pr),
            title: Some(title.into()),
            commit: sha(c),
            guarantee: g,
            resolution: r,
            at: ago(m),
        }
    };
    let payments = savings(214, [540, 190, 1430], Some(246_100), [23, 4, 1]);
    let cards = vec![
        RepoCard {
            overview: repos[0].clone(),
            landed_daily: daily(14, 12, 1),
            savings: payments.clone(),
            predicted_wait_minutes: None,
        },
        RepoCard {
            overview: repos[1].clone(),
            landed_daily: daily(14, 6, 4),
            savings: savings(96, [420, 150, 610], Some(41_800), [9, 1, 0]),
            predicted_wait_minutes: None,
        },
        RepoCard {
            overview: repos[2].clone(),
            landed_daily: vec![0; 14],
            savings: savings(0, [300, 0, 0], None, [0, 0, 0]),
            predicted_wait_minutes: Some(312),
        },
    ];
    let detail = RepoDetail {
        overview: repos[0].clone(),
        landed_daily: daily(30, 12, 1),
        savings: payments,
        predicted_wait_minutes: None,
        queue: vec![
            q(318, "Retry webhooks with exponential backoff", ChangeState::Verifying, Some((2, 5)), Some("would reverify 1 target touched by changes landed since its base"), 3),
            q(321, "Add refund reasons to the ledger", ChangeState::Merging, Some((3, 7)), Some("writes the same targets as #317; would go to the merge engine"), 6),
            q(322, "Show currency in invoice emails", ChangeState::Ready, Some((1, 2)), Some("would land now, together with #323"), 1),
            q(323, "Bump tracing to 0.1.41", ChangeState::Ready, Some((1, 1)), Some("would land now, together with #322"), 1),
            q(324, "Draft: split the billing crate", ChangeState::Pending, None, None, 12),
        ],
        landed: vec![
            l(317, "Refund reasons: storage", 'a', GuaranteeTier::FootprintProven, None, 4),
            l(315, "Cache exchange rates for 5 minutes", 'b', GuaranteeTier::SpeculationVerified, None, 9),
            l(314, "Update Cargo.lock for serde 1.0.230", 'c', GuaranteeTier::FootprintProven, Some(ResolutionTier::Regenerate), 15),
            l(312, "Add the payouts feature flag", 'd', GuaranteeTier::FootprintProven, Some(ResolutionTier::Declared), 31),
            l(309, "Rename rate() to standard_rate()", 'e', GuaranteeTier::FootprintProven, Some(ResolutionTier::ReplayFull), 52),
        ],
        returned: vec![ReturnedItem {
            pr: PrNumber(311),
            title: Some("Round totals to cents".into()),
            reason: "CI failed with the change merged onto `main` at `9f2c1ab` (branch `sheltie/verify/311-9f2c1ab0e7d1`)".into(),
            at: ago(40),
        }],
        leases: vec![
            LeaseItem { issue: 88, title: Some("Support partial refunds".into()), holder: "claude-agent-1".into(), expires_at: now + Duration::minutes(190) },
            LeaseItem { issue: 91, title: Some("Faster invoice PDF rendering".into()), holder: "claude-agent-2".into(), expires_at: now + Duration::minutes(75) },
        ],
    };
    let bar = top_bar(Some("octo-dev"));
    std::fs::write(
        dir.join("index.html"),
        page("Repositories", &bar, &render_overview(&cards, now)).replace(
            r#"href="/dashboard/acme/payments""#,
            r#"href="/dashboard/acme/payments.html""#,
        ),
    )?;
    std::fs::write(
        dir.join("acme/payments.html"),
        page(
            "acme/payments",
            &bar,
            &render_repo(&detail, "https://github.com", now),
        ),
    )?;
    use std::io::Write as _;
    writeln!(std::io::stdout(), "{}", out.display())?;
    Ok(())
}
