//! Read a **vibe** session store (#962, story #960; measured on vibe 2.25.8 with
//! `--legacy-harness`): `<root>/<prefix>_<yyyymmdd>_<hhmmss>_<id8>/{meta.json,messages.jsonl}`.
//!
//! `meta.json` carries the session identity and everything PDO reads per **session**
//! (vibe reports nothing per message): `session_id` (the full UUID), `start_time`
//! (RFC 3339, UTC), `environment.working_directory`, `config.active_model` (the alias
//! vibe ran on) and `config.models` (the catalogue snapshot, which names each alias's
//! provider), `stats.session_cost` with the two prices per million,
//! `stats.session_prompt_tokens` / `session_completion_tokens`, `stats.context_tokens`,
//! `stats.steps`. `messages.jsonl` is the transcript: one `{role, content, injected}` a line.
//!
//! **Learned identity (ADR-0080).** vibe names its own sessions; PDO learns the id once —
//! the *first* session of the node's working directory created *after* the spawn — and
//! freezes it in the event log. [`learn_session`] is that rule, pure over a listing of
//! `meta.json` texts. Every read then goes by id ([`resolve_by_id`]), never "the newest".
//!
//! Pure, like `pi_session`: text in, facts out. A torn or unreadable file is "no
//! reading", never an error.

use std::path::{Path, PathBuf};

/// A reported cost, already in dollars (ADR-0052 §2 amended): constant 1.0.
pub(crate) const REPORTED_USD_CONSTANT: f64 = 1.0;

/// Why a session's cost is unavailable when `stats` carries no prices.
pub(crate) const PRICES_ABSENT_REASON: &str =
    "vibe reported no prices for this session (stats without price per million)";
/// Why a session's cost is unavailable when `meta.json` carries no `stats` at all.
pub(crate) const STATS_ABSENT_REASON: &str =
    "vibe reported no usage for this session yet (meta.json without stats)";

/// The reported cost of one vibe session, as `run_cost` folds it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ReportedCost {
    /// `stats` present with **both** prices per million: `session_cost` × 1.0. A price
    /// **declared** zero by vibe's catalogue (a local model) is a genuine `Usd(0.0)` —
    /// a declared zero, not a deduced one (CONTEXT.md § "Coût rapporté en dollars").
    Usd(f64),
    /// `stats` absent, or prices absent: « — » with the reason, never `$0`.
    Unavailable { reason: String },
}

/// The model one session ran on, with the provider its catalogue snapshot names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedModel {
    pub model: String,
    pub provider: Option<String>,
}

/// The identity a `meta.json` declares, as [`learn_session`] reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionMeta {
    pub session_id: String,
    /// `start_time`, parsed to UTC.
    pub start_time: chrono::DateTime<chrono::Utc>,
    pub working_directory: PathBuf,
}

fn parse(text: &str) -> Option<serde_json::Value> {
    serde_json::from_str(text).ok()
}

/// The identity facts of a `meta.json`, or `None` when any is missing or unparsable.
pub(crate) fn session_meta(text: &str) -> Option<SessionMeta> {
    let m = parse(text)?;
    let session_id = m.get("session_id")?.as_str()?.to_string();
    let start_time = chrono::DateTime::parse_from_rfc3339(m.get("start_time")?.as_str()?)
        .ok()?
        .with_timezone(&chrono::Utc);
    let working_directory = PathBuf::from(m.pointer("/environment/working_directory")?.as_str()?);
    Some(SessionMeta {
        session_id,
        start_time,
        working_directory,
    })
}

/// The session's reported cost (see [`ReportedCost`]).
pub(crate) fn reported_cost(text: &str) -> ReportedCost {
    let Some(m) = parse(text) else {
        return ReportedCost::Unavailable {
            reason: STATS_ABSENT_REASON.to_string(),
        };
    };
    let Some(stats) = m.get("stats").filter(|s| s.is_object()) else {
        return ReportedCost::Unavailable {
            reason: STATS_ABSENT_REASON.to_string(),
        };
    };
    let price = |k: &str| stats.get(k).and_then(|v| v.as_f64());
    match (
        price("input_price_per_million"),
        price("output_price_per_million"),
        price("session_cost"),
    ) {
        (Some(_), Some(_), Some(cost)) => ReportedCost::Usd(cost * REPORTED_USD_CONSTANT),
        _ => ReportedCost::Unavailable {
            reason: PRICES_ABSENT_REASON.to_string(),
        },
    }
}

/// The model the session ran on (`config.active_model`, the alias vibe accepts) and
/// its provider, looked up in the session's own catalogue snapshot by alias, then name.
pub(crate) fn observed_model(text: &str) -> Option<ObservedModel> {
    let m = parse(text)?;
    let model = m
        .pointer("/config/active_model")?
        .as_str()?
        .trim()
        .to_string();
    if model.is_empty() {
        return None;
    }
    // `config.models` is a map keyed by alias (the session snapshot), unlike the
    // `[[models]]` array of `config.toml`; fall back to a scan by `name`.
    let models = m.pointer("/config/models").and_then(|v| v.as_object());
    let entry = models.and_then(|ms| {
        ms.get(&model).or_else(|| {
            ms.values()
                .find(|e| e.get("name").and_then(|v| v.as_str()) == Some(model.as_str()))
        })
    });
    let provider = entry
        .and_then(|e| e.get("provider").and_then(|v| v.as_str()))
        .map(str::to_string);
    Some(ObservedModel { model, provider })
}

/// The session's context peak in tokens: `stats.context_tokens`. `None` rather than
/// `Some(0)` when absent or zero, so a missing reading is never "used no context".
pub(crate) fn session_peak(text: &str) -> Option<u64> {
    parse(text)?
        .pointer("/stats/context_tokens")?
        .as_u64()
        .filter(|t| *t > 0)
}

/// The steering messages a human typed: `role: user` rows with `injected: false`,
/// minus the first (the launch prompt). `injected: true` rows are vibe's own
/// insertions (an invoked skill, a mentioned file, a hook retry), never a human.
/// `None` when the transcript carries no user row at all (no reading).
pub(crate) fn steering_count(messages: &str) -> Option<u32> {
    let typed = messages
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .filter(|m| m.get("injected").and_then(|i| i.as_bool()) != Some(true))
        .count();
    if typed == 0 {
        None
    } else {
        Some((typed - 1) as u32)
    }
}

/// ADR-0080 §2 — the session PDO learns for a node: among `metas` (the `meta.json`
/// text of every session under the store, with its directory), the one whose
/// `working_directory` is the node's `working_dir` and whose `start_time` is **at or
/// after** `spawn`, the **earliest** such. `None` until vibe has created it.
pub(crate) fn learn_session<'a>(
    metas: impl IntoIterator<Item = (&'a Path, &'a str)>,
    working_dir: &Path,
    spawn: chrono::DateTime<chrono::Utc>,
) -> Option<(PathBuf, SessionMeta)> {
    metas
        .into_iter()
        .filter_map(|(dir, text)| session_meta(text).map(|m| (dir.to_path_buf(), m)))
        .filter(|(_, m)| m.working_directory == working_dir && m.start_time >= spawn)
        .min_by_key(|(_, m)| m.start_time)
}

/// Whether a session directory is **resumable** by vibe: both files present and valid
/// JSON (vibe's own `_is_valid_session` rule, measured). A node killed between two
/// writes leaves a directory that is not.
pub(crate) fn session_readable(dir: &Path) -> bool {
    let meta = std::fs::read_to_string(dir.join("meta.json"))
        .ok()
        .and_then(|t| session_meta(&t))
        .is_some();
    let messages = std::fs::read_to_string(dir.join("messages.jsonl"))
        .ok()
        .map(|t| {
            t.lines()
                .filter(|l| !l.trim().is_empty())
                .all(|l| serde_json::from_str::<serde_json::Value>(l).is_ok())
        })
        .unwrap_or(false);
    meta && messages
}

/// The session directory named after `session_id` under `root`: the directory whose
/// name ends with the id's first eight characters **and** whose `meta.json` carries the
/// full id (the directory name alone is a prefix, never the identity).
pub(crate) fn resolve_dir_by_id(root: &Path, session_id: &str) -> Option<PathBuf> {
    let short = session_id.get(..8)?;
    let entries = std::fs::read_dir(root).ok()?;
    entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with(&format!("_{short}")))
                .unwrap_or(false)
        })
        .find(|p| {
            std::fs::read_to_string(p.join("meta.json"))
                .ok()
                .and_then(|t| session_meta(&t))
                .map(|m| m.session_id == session_id)
                .unwrap_or(false)
        })
}

/// The file PDO reads a session **by** (cost, model, context, steps): its `meta.json`.
/// The transcript proper is its sibling, [`messages_sibling`].
pub(crate) fn resolve_by_id(root: &Path, session_id: &str) -> Option<PathBuf> {
    resolve_dir_by_id(root, session_id).map(|d| d.join("meta.json"))
}

/// `messages.jsonl` beside a resolved `meta.json` (the steering and turn-end file).
pub(crate) fn messages_sibling(meta_path: &Path) -> PathBuf {
    meta_path.with_file_name("messages.jsonl")
}

/// What a resume of a vibe node re-enters (ADR-0080 §4): the learned session **by
/// id** when it is still resumable, else a **fresh** session — with the reason said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResumeIdentity {
    /// `Some(id)` ⇒ `--resume <id>`; `None` ⇒ no resume flag, a fresh session.
    pub session_id: Option<String>,
    /// Why the resume is fresh, when it is — recorded on the resurrection event.
    pub fresh_reason: Option<String>,
}

/// Decide [`ResumeIdentity`] for `learned` under `store_root`: a learned id whose
/// folder is [`session_readable`] resumes by id; a missing or torn folder, or no learned
/// id at all, relaunches fresh. The sweep then learns the new session (a relaunch
/// newer than the last learned id re-arms learning).
pub(crate) fn resume_identity(store_root: &Path, learned: Option<&str>) -> ResumeIdentity {
    match learned {
        None => ResumeIdentity {
            session_id: None,
            fresh_reason: Some(
                "no vibe session was learned for this node yet — relaunching a fresh session"
                    .to_string(),
            ),
        },
        Some(id) => match resolve_dir_by_id(store_root, id) {
            Some(dir) if session_readable(&dir) => ResumeIdentity {
                session_id: Some(id.to_string()),
                fresh_reason: None,
            },
            _ => ResumeIdentity {
                session_id: None,
                fresh_reason: Some(format!(
                    "vibe session {id} is not resumable (folder missing, or meta.json / \
                     messages.jsonl absent or torn) — relaunching a fresh session"
                )),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRICED: &str = include_str!("../tests/fixtures/vibe-2.25.8/meta_priced.json");
    const ZERO: &str = include_str!("../tests/fixtures/vibe-2.25.8/meta_zero_price.json");
    const NO_PRICES: &str = include_str!("../tests/fixtures/vibe-2.25.8/meta_no_prices.json");
    const NO_STATS: &str = include_str!("../tests/fixtures/vibe-2.25.8/meta_no_stats.json");
    const STEERING: &str = include_str!("../tests/fixtures/vibe-2.25.8/messages_steering.jsonl");

    #[test]
    fn a_priced_session_reports_its_cost_in_dollars_and_a_declared_zero_is_zero() {
        assert_eq!(reported_cost(PRICED), ReportedCost::Usd(0.0005168));
        // A local model priced 0 by the catalogue: a declared zero, `$0.00`, not « — ».
        assert_eq!(reported_cost(ZERO), ReportedCost::Usd(0.0));
    }

    #[test]
    fn a_session_without_prices_or_without_stats_is_unavailable_never_zero() {
        assert_eq!(
            reported_cost(NO_PRICES),
            ReportedCost::Unavailable {
                reason: PRICES_ABSENT_REASON.to_string()
            }
        );
        assert_eq!(
            reported_cost(NO_STATS),
            ReportedCost::Unavailable {
                reason: STATS_ABSENT_REASON.to_string()
            }
        );
        assert_eq!(
            reported_cost("not json"),
            ReportedCost::Unavailable {
                reason: STATS_ABSENT_REASON.to_string()
            }
        );
    }

    #[test]
    fn the_observed_model_is_the_sessions_active_model_with_its_provider() {
        assert_eq!(
            observed_model(PRICED),
            Some(ObservedModel {
                model: "devstral-small".into(),
                provider: Some("mistral".into())
            })
        );
        assert_eq!(
            observed_model(ZERO),
            Some(ObservedModel {
                model: "local".into(),
                provider: Some("llamacpp".into())
            })
        );
        assert_eq!(
            observed_model(NO_STATS).map(|m| m.model),
            Some("devstral-small".into())
        );
        assert_eq!(observed_model("{}"), None);
    }

    #[test]
    fn context_peak_and_steering_read_their_own_fields() {
        assert_eq!(session_peak(PRICED), Some(5078));
        assert_eq!(session_peak(NO_STATS), None);
        // 4 typed user rows (one injected skipped) minus the launch prompt = 2.
        assert_eq!(steering_count(STEERING), Some(2));
        assert_eq!(steering_count(""), None);
        assert_eq!(
            steering_count("{\"role\":\"user\",\"content\":\"only prompt\",\"injected\":false}\n"),
            Some(0)
        );
    }

    fn meta(sid: &str, start: &str, cwd: &str) -> String {
        format!(
            "{{\"session_id\":\"{sid}\",\"start_time\":\"{start}\",\"environment\":{{\"working_directory\":\"{cwd}\"}}}}"
        )
    }

    #[test]
    fn learning_picks_the_first_session_of_the_cwd_created_after_the_spawn() {
        let spawn = chrono::DateTime::parse_from_rfc3339("2026-09-30T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let before = meta(
            "aaaa1111-0000-0000-0000-000000000000",
            "2026-09-30T11:59:59+00:00",
            "/w",
        );
        let first = meta(
            "bbbb2222-0000-0000-0000-000000000000",
            "2026-09-30T12:00:03.5+00:00",
            "/w",
        );
        let later = meta(
            "cccc3333-0000-0000-0000-000000000000",
            "2026-09-30T12:00:09+00:00",
            "/w",
        );
        let other = meta(
            "dddd4444-0000-0000-0000-000000000000",
            "2026-09-30T12:00:01+00:00",
            "/elsewhere",
        );
        let listing: Vec<(&Path, &str)> = vec![
            (Path::new("/s/later"), later.as_str()),
            (Path::new("/s/other"), other.as_str()),
            (Path::new("/s/before"), before.as_str()),
            (Path::new("/s/torn"), "{not json"),
            (Path::new("/s/first"), first.as_str()),
        ];
        let (dir, m) = learn_session(listing.clone(), Path::new("/w"), spawn).unwrap();
        assert_eq!(dir, PathBuf::from("/s/first"));
        assert_eq!(m.session_id, "bbbb2222-0000-0000-0000-000000000000");
        // Nothing eligible yet ⇒ None (learning is deferred, ADR-0080 limits).
        assert_eq!(learn_session(listing, Path::new("/nowhere"), spawn), None);
        // The real fixture parses too (UTC offset form `+00:00`).
        assert_eq!(
            session_meta(PRICED).unwrap().working_directory,
            PathBuf::from("/work/repo")
        );
    }

    #[test]
    fn a_resume_re_enters_a_readable_learned_session_by_id_else_relaunches_fresh_and_says_why() {
        let root = tempfile::tempdir().unwrap();
        let sid = "7a76feb5-f3a0-af82-ae5c-481ab27fdafd";
        // Nothing learned yet: fresh, said.
        let none = resume_identity(root.path(), None);
        assert_eq!(none.session_id, None);
        assert!(none
            .fresh_reason
            .unwrap()
            .contains("no vibe session was learned"));
        // Learned but the folder is gone: fresh, said with the id.
        let gone = resume_identity(root.path(), Some(sid));
        assert_eq!(gone.session_id, None);
        assert!(gone.fresh_reason.unwrap().contains(sid));
        // Learned and readable: by id, no reason.
        let dir = root.path().join("session_20260930_123902_7a76feb5");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), PRICED).unwrap();
        std::fs::write(dir.join("messages.jsonl"), STEERING).unwrap();
        assert_eq!(
            resume_identity(root.path(), Some(sid)),
            ResumeIdentity {
                session_id: Some(sid.to_string()),
                fresh_reason: None
            }
        );
        // Torn transcript (killed between two writes): fresh again.
        std::fs::write(dir.join("messages.jsonl"), "{torn").unwrap();
        assert_eq!(resume_identity(root.path(), Some(sid)).session_id, None);
    }

    #[test]
    fn a_session_resolves_by_full_id_and_is_readable_only_with_both_valid_files() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("session_20260930_123902_7a76feb5");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), PRICED).unwrap();
        // A decoy sharing the 8-char prefix but not the full id.
        let decoy = root.path().join("session_20260101_000000_7a76feb5");
        std::fs::create_dir_all(&decoy).unwrap();
        std::fs::write(decoy.join("meta.json"), ZERO).unwrap();

        let sid = "7a76feb5-f3a0-af82-ae5c-481ab27fdafd";
        assert_eq!(resolve_by_id(root.path(), sid), Some(dir.join("meta.json")));
        assert_eq!(
            messages_sibling(&dir.join("meta.json")),
            dir.join("messages.jsonl")
        );
        assert_eq!(
            resolve_by_id(root.path(), "00000000-aaaa-bbbb-cccc-dddddddddddd"),
            None
        );

        // No messages.jsonl yet: not resumable (vibe's own rule).
        assert!(!session_readable(&dir));
        std::fs::write(dir.join("messages.jsonl"), STEERING).unwrap();
        assert!(session_readable(&dir));
        std::fs::write(dir.join("messages.jsonl"), "{torn").unwrap();
        assert!(!session_readable(&dir));
    }
}
