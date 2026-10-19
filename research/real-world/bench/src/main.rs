//! Measurement helper (scratch, not shipped): footprints for many commits, Sheltie queue
//! replays with real arrival times, and pairwise relations.
//!
//!   bench footprints <tool> <repo> <pairs.json> <cache-dir> [threads]
//!   bench graph      <tool> <repo> <rev>
//!   bench queue      <cache-dir> <queue.json>
//!   bench relate     <cache-dir> <pairs.json>

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, bail};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sheltie_adapter::policy::COMMON_PLAIN;
use sheltie_adapter::{Adapter, DEFAULT_IGNORE, Generic, Git, PathPolicy};
use sheltie_core::{Footprint, FootprintReport};
mod replay;

fn adapter(tool: &str) -> anyhow::Result<Box<dyn Adapter + Send + Sync>> {
    Ok(match tool {
        "cargo" => Box::new(sheltie_cargo::Cargo),
        "go" => Box::new(Generic::<sheltie_go::Go>::new()),
        "node" => Box::new(Generic::<sheltie_node::Node>::new()),
        "python" => Box::new(Generic::<sheltie_python::Python>::new()),
        "maven" => Box::new(Generic::<sheltie_maven::Maven>::new()),
        "gradle" => Box::new(Generic::<sheltie_gradle::Gradle>::new()),
        t => bail!("unknown tool {t}"),
    })
}

fn policy(a: &dyn Adapter) -> anyhow::Result<PathPolicy> {
    let plain: Vec<&str> = COMMON_PLAIN.iter().chain(a.plain()).copied().collect();
    Ok(PathPolicy::new(DEFAULT_IGNORE, &plain)?)
}

#[derive(Deserialize)]
struct Pair {
    sha: String,
    parent: String,
}

fn cached(cache: &Path, sha: &str) -> anyhow::Result<Option<Footprint>> {
    let v: serde_json::Value = serde_json::from_slice(
        &std::fs::read(cache.join(format!("{sha}.json"))).with_context(|| format!("no footprint for {sha}"))?,
    )?;
    if v.get("error").is_some() {
        return Ok(None);
    }
    let r: FootprintReport = serde_json::from_value(v)?;
    Ok(Some(r.footprint))
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("footprints") => {
            let (tool, repo, pairs, cache) = (&args[2], PathBuf::from(&args[3]), &args[4], PathBuf::from(&args[5]));
            let threads: usize = args.get(6).map_or(Ok(6), |t| t.parse())?;
            std::fs::create_dir_all(&cache)?;
            let pairs: Vec<Pair> = serde_json::from_slice(&std::fs::read(pairs)?)?;
            let todo: Vec<Pair> = pairs.into_iter().filter(|p| !cache.join(format!("{}.json", p.sha)).exists()).collect();
            let total = todo.len();
            let queue = Arc::new(Mutex::new(todo));
            let done = Arc::new(AtomicUsize::new(0));
            let started = std::time::Instant::now();
            let mut handles = Vec::new();
            for _ in 0..threads {
                let (queue, done, repo, cache, tool) = (queue.clone(), done.clone(), repo.clone(), cache.clone(), tool.clone());
                handles.push(std::thread::spawn(move || -> anyhow::Result<()> {
                    let a = adapter(&tool)?;
                    let pol = policy(a.as_ref())?;
                    let git = Git::open(&repo)?;
                    let scratch = std::env::temp_dir();
                    loop {
                        let Some(p) = queue.lock().map_err(|_| anyhow::anyhow!("poisoned"))?.pop() else { break };
                        let out = match a.compute(&git, &p.parent, &p.sha, &pol, &scratch) {
                            Ok(r) => serde_json::to_vec(&r)?,
                            Err(e) => serde_json::to_vec(&serde_json::json!({ "error": e.to_string() }))?,
                        };
                        std::fs::write(cache.join(format!("{}.json", p.sha)), out)?;
                        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                        if n % 100 == 0 || n == total {
                            eprintln!("{n}/{total} in {:.0}s", started.elapsed().as_secs_f64());
                        }
                    }
                    Ok(())
                }));
            }
            for h in handles {
                h.join().map_err(|_| anyhow::anyhow!("worker panicked"))??;
            }
        }
        Some("graph") => {
            let a = adapter(&args[2])?;
            let git = Git::open(&args[3])?;
            let t = std::time::Instant::now();
            let g = a.graph(&git, &args[4], &policy(a.as_ref())?, &std::env::temp_dir())?;
            let owned = g.owners.len();
            println!(
                "{}",
                serde_json::json!({ "targets": g.targets.len(), "owned_files": owned, "seconds": t.elapsed().as_secs_f64(),
                    "sample": g.targets.keys().take(8).map(|k| k.as_str().to_owned()).collect::<Vec<_>>() })
            );
        }
        Some("queue") => {
            #[derive(Deserialize)]
            struct Change {
                sha: String,
                ready_at: DateTime<Utc>,
                based_at: Option<DateTime<Utc>>,
            }
            #[derive(Deserialize)]
            struct Input {
                ci_seconds: i64,
                changes: Vec<Change>,
            }
            let cache = PathBuf::from(&args[2]);
            let input: Input = serde_json::from_slice(&std::fs::read(&args[3])?)?;
            let mut changes = Vec::new();
            let (mut missing, mut leaky) = (0, 0);
            for c in &input.changes {
                match cached(&cache, &c.sha)? {
                    Some(footprint) => {
                        leaky += usize::from(footprint.leaky);
                        changes.push(replay::Change { ready_at: c.ready_at, based_at: c.based_at.unwrap_or(c.ready_at).min(c.ready_at), footprint });
                    }
                    None => missing += 1,
                }
            }
            let (r, per) = replay::replay(&changes, input.ci_seconds).context("too few changes")?;
            // The copy only adds per-change results; its totals must match the shipped replay.
            let shipped: Vec<sheltie_scheduler::roi::HistoricalChange> = changes
                .iter()
                .map(|c| sheltie_scheduler::roi::HistoricalChange {
                    ready_at: c.ready_at,
                    based_at: Some(c.based_at),
                    footprint: c.footprint.clone(),
                })
                .collect();
            if let Some(lib) = sheltie_scheduler::roi::replay(&shipped, input.ci_seconds) {
                anyhow::ensure!(
                    lib.sheltie == r.sheltie && lib.serial == r.serial && lib.decisions == r.decisions,
                    "bench replay differs from sheltie_scheduler::roi::replay"
                );
            }
            println!("{}", serde_json::json!({ "replay": r, "missing": missing, "leaky": leaky, "per_change": per }));
        }
        Some("relate") => {
            #[derive(Deserialize)]
            struct Rel {
                a: String,
                b: String,
            }
            let cache = PathBuf::from(&args[2]);
            let pairs: Vec<Rel> = serde_json::from_slice(&std::fs::read(&args[3])?)?;
            let mut memo: BTreeMap<String, Option<Footprint>> = BTreeMap::new();
            let mut out = Vec::new();
            for p in pairs {
                for s in [&p.a, &p.b] {
                    if !memo.contains_key(s) {
                        memo.insert(s.clone(), cached(&cache, s)?);
                    }
                }
                let rel = match (&memo[&p.a], &memo[&p.b]) {
                    (Some(a), Some(b)) => format!("{:?}", a.relation(b)),
                    _ => "Unknown".to_owned(),
                };
                out.push(serde_json::json!({ "a": p.a, "b": p.b, "relation": rel }));
            }
            println!("{}", serde_json::Value::Array(out));
        }
        _ => bail!("usage: bench footprints|graph|queue|relate ..."),
    }
    Ok(())
}
