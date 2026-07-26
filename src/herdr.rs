//! herdr host integration: resolve the agent pane and send to it.
//!
//! See `specs/herdr-host.md`. Uses the herdr CLI via `$HERDR_BIN_PATH`. Only the
//! agent-send export depends on this module; browsing and clipboard do not.

use std::collections::HashMap;
use std::env;
use std::process::Command;

use crate::turn::Status;
use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct AgentListResponse {
    result: AgentList,
}

#[derive(Debug, Deserialize)]
struct AgentList {
    agents: Vec<AgentPane>,
}

/// One entry of `herdr agent list`. The picker-facing fields are optional: herdr 0.7.5 omits
/// `name`, `display_agent`, and `state_labels` entirely until something sets them, and
/// `herdr agent rename --clear` leaves `name` present and null. Both parse to `None`. The
/// identity fields stay required, so a payload missing `pane_id` fails the parse loudly
/// instead of minting an unaddressable send target.
#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
struct AgentPane {
    agent: Option<String>,
    agent_status: Status,
    pane_id: String,
    tab_id: String,
    workspace_id: String,
    name: Option<String>,
    display_agent: Option<String>,
    state_labels: Option<HashMap<String, String>>,
}

/// One picker row: the pane the send addresses, and the three parts the row shows
/// (`specs/herdr-host.md`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentChoice {
    pub pane_id: String,
    pub name: String,
    pub state: String,
    pub tab: String,
}

/// What `Send` does with the agents herdr reports (`specs/herdr-host.md`). A refusal is the
/// `Err` of [`send_target`], so zero agents and a failed enumeration land in one place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendTarget {
    /// Exactly one agent. The send goes straight to it, with no picker.
    One(AgentChoice),
    /// Several agents, in herdr's own order. The picker opens over them.
    Many(Vec<AgentChoice>),
}

fn herdr_bin() -> String {
    env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_string())
}

fn herdr(args: &[&str]) -> Result<String> {
    let out = Command::new(herdr_bin())
        .args(args)
        .output()
        .with_context(|| format!("running herdr {args:?}"))?;
    if !out.status.success() {
        bail!("herdr {args:?} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The (tab, workspace, pane) id trio identifying this sidebar in the herdr environment.
fn agent_env() -> (Option<String>, Option<String>, Option<String>) {
    (
        env::var("HERDR_TAB_ID").ok(),
        env::var("HERDR_WORKSPACE_ID").ok(),
        env::var("HERDR_PANE_ID").ok(),
    )
}

/// The agents herdr currently lists. The one place the `agent list` call and its envelope
/// parsing live, shared by pane and status resolution.
fn agent_list() -> Result<Vec<AgentPane>> {
    parse_agents(&herdr(&["agent", "list"])?)
}

/// What `Send` does: one workspace agent sends directly, several open the picker, and no
/// agent refuses (`specs/herdr-host.md`). A failed enumeration refuses the same way, so the
/// reviewer is never told a count herdr did not report. The status line renders the refusal
/// as `agent failed: <this message>` (HH-REFUSE-SAYS-CLIPBOARD).
pub fn send_target() -> Result<SendTarget> {
    let (_, ws, me) = agent_env();
    let agents = agent_list().unwrap_or_default();
    // Candidacy is decided once, here: an `agent` field, our workspace, not our own pane
    // (HH-AGENT-PANES, HH-NOT-SELF). Rows keep `agent list` order, which is herdr's own
    // (`specs/herdr-host.md`).
    let picked = candidates(&agents, ws.as_deref(), me.as_deref(), |agent| &agent.workspace_id);
    match picked.len() {
        0 => bail!("no agent here — copy to the clipboard instead"),
        // The sole-agent send shows no row, so only the picker pays for the tab-label call.
        1 => Ok(SendTarget::One(picked[0].choice(&HashMap::new()))),
        _ => {
            let tabs = tab_labels(ws.as_deref());
            Ok(SendTarget::Many(picked.into_iter().map(|agent| agent.choice(&tabs)).collect()))
        }
    }
}

impl AgentPane {
    /// This pane as a picker row (`specs/herdr-host.md`).
    fn choice(&self, tabs: &HashMap<String, String>) -> AgentChoice {
        AgentChoice {
            pane_id: self.pane_id.clone(),
            name: self.row_name(),
            state: self.row_state(),
            tab: tabs.get(&self.tab_id).cloned().unwrap_or_default(),
        }
    }

    /// The agent's `name`, else its `display_agent`, else its kind (`specs/herdr-host.md`).
    /// A cleared name arrives as null and falls through like an absent one. The pane id is a
    /// last resort no live agent reaches, so the row and the success line always name something.
    fn row_name(&self) -> String {
        [&self.name, &self.display_agent, &self.agent]
            .into_iter()
            .flatten()
            .find(|part| !part.is_empty())
            .cloned()
            .unwrap_or_else(|| self.pane_id.clone())
    }

    /// The agent's `state_labels` entry for its state, else the state itself.
    fn row_state(&self) -> String {
        let state = self.agent_status.as_str();
        self.state_labels
            .as_ref()
            .and_then(|labels| labels.get(state))
            .filter(|label| !label.is_empty())
            .cloned()
            .unwrap_or_else(|| state.to_string())
    }
}

/// The pane the sidebar was opened beside, from the plugin context herdr puts in the pane's
/// own environment. It can be absent, and it can name a pane that has since closed, so the
/// caller treats it as one highlight candidate among others (`specs/herdr-host.md`).
pub fn opened_beside() -> Option<String> {
    let context = env::var("HERDR_PLUGIN_CONTEXT_JSON").ok()?;
    let parsed: PluginContext = serde_json::from_str(&context).ok()?;
    parsed.focused_pane_id
}

#[derive(Debug, Deserialize)]
struct PluginContext {
    #[serde(default)]
    focused_pane_id: Option<String>,
}

/// Tab id to tab label for one workspace. Labelling is best effort: a failed call or a
/// missing tab leaves the row's tab part empty rather than failing the send.
fn tab_labels(ws: Option<&str>) -> HashMap<String, String> {
    let Some(ws) = ws else { return HashMap::new() };
    let Ok(json) = herdr(&["tab", "list", "--workspace", ws]) else {
        return HashMap::new();
    };
    parse_tab_labels(&json).unwrap_or_default()
}

/// The documented `result.tabs` array from `herdr tab list`, as tab id → label. A tab
/// without a label is dropped, so its rows show no tab part.
fn parse_tab_labels(json: &str) -> Result<HashMap<String, String>> {
    let response: TabListResponse = serde_json::from_str(json).context("parsing tab list")?;
    Ok(response
        .result
        .tabs
        .into_iter()
        .filter_map(|tab| tab.label.map(|label| (tab.tab_id, label)))
        .collect())
}

#[derive(Debug, Deserialize)]
struct TabListResponse {
    result: TabList,
}

#[derive(Debug, Deserialize)]
struct TabList {
    tabs: Vec<TabInfo>,
}

#[derive(Debug, Deserialize)]
struct TabInfo {
    tab_id: String,
    #[serde(default)]
    label: Option<String>,
}

/// The documented `result.agents` array from `herdr agent list`.
fn parse_agents(json: &str) -> Result<Vec<AgentPane>> {
    let response: AgentListResponse = serde_json::from_str(json).context("parsing agent list")?;
    Ok(response.result.agents)
}

/// The resolved agent's `agent_status` (`idle`/`working`/`blocked`/`done`/`unknown`), for
/// turn tracking (`specs/herdr-host.md`). `Ok(None)` when no agent resolves, so the caller
/// treats an absent or ambiguous agent the same as a missing herdr — turn tracking pauses.
pub fn resolved_agent_status() -> Result<Option<Status>> {
    let (tab, ws, me) = agent_env();
    Ok(pick_agent(&agent_list()?, tab.as_deref(), ws.as_deref(), me.as_deref())
        .map(|agent| agent.agent_status))
}

/// The sole agent in this tab, else the sole workspace agent, for turn tracking
/// (`specs/herdr-host.md`, HH-AGENT-PANES, HH-NOT-SELF, HH-TAB-WINS). `None` when nothing
/// resolves — zero candidates or an ambiguous set alike pause tracking. The send never
/// resolves this way; an ambiguous workspace opens the picker instead.
fn pick_agent<'a>(
    agents: &'a [AgentPane],
    tab: Option<&str>,
    ws: Option<&str>,
    me: Option<&str>,
) -> Option<&'a AgentPane> {
    if let &[agent] = candidates(agents, tab, me, |agent| &agent.tab_id).as_slice() {
        return Some(agent);
    }
    match candidates(agents, ws, me, |agent| &agent.workspace_id).as_slice() {
        &[agent] => Some(agent),
        _ => None,
    }
}

/// The real agents whose projected ID equals `want`, ignoring our own pane `me`. Only entries
/// carrying an `agent` field count (HH-AGENT-PANES). herdr 0.7.5 already keeps non-agent panes
/// out of `agent list`, so both filters are defensive: a plugin sidebar or a plain shell shows
/// up in `pane list` with `agent: null` and never here (`../docs/herdr-api-notes.md`).
fn candidates<'a>(
    agents: &'a [AgentPane],
    want: Option<&str>,
    me: Option<&str>,
    id: impl Fn(&'a AgentPane) -> &'a str,
) -> Vec<&'a AgentPane> {
    let Some(want) = want else { return Vec::new() };
    agents
        .iter()
        .filter(|agent| agent.agent.is_some())
        .filter(|agent| id(agent) == want)
        .filter(|agent| Some(agent.pane_id.as_str()) != me)
        .collect()
}

/// Write literal text into the agent pane's input, without submitting.
///
/// Uses `pane send-text`, not the agent-level send: herdr 0.7.5 replaced `agent send` with
/// the logical-key `agent send-keys`, while `pane send-text` has carried the literal-text,
/// no-Enter semantics unchanged since 0.7.0 (`docs/herdr-api-notes.md`).
pub fn send_text(pane: &str, text: &str) -> Result<()> {
    herdr(&["pane", "send-text", pane, text])?;
    Ok(())
}

/// Focus the agent pane so the reviewer can add context and submit.
pub fn focus(pane: &str) -> Result<()> {
    herdr(&["agent", "focus", pane])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AgentChoice, AgentPane, HashMap, Status, parse_agents, parse_tab_labels, pick_agent,
    };

    /// One agent entry shaped like the real `herdr agent list` output (api notes).
    fn agent(pane: &str, tab: &str, ws: &str) -> AgentPane {
        AgentPane {
            agent: Some("claude".to_string()),
            agent_status: Status::Working,
            pane_id: pane.to_string(),
            tab_id: tab.to_string(),
            workspace_id: ws.to_string(),
            ..AgentPane::default()
        }
    }

    /// One non-agent pane as herdr 0.7.1 lists it live: `agent_status: unknown`, no `agent`
    /// field — a plugin sidebar or a plain shell.
    fn non_agent_pane(pane: &str, tab: &str, ws: &str) -> AgentPane {
        AgentPane {
            agent: None,
            agent_status: Status::Unknown,
            pane_id: pane.to_string(),
            tab_id: tab.to_string(),
            workspace_id: ws.to_string(),
            ..AgentPane::default()
        }
    }

    /// [`pick_agent`] reduced to the picked `pane_id`, for terse assertions.
    fn pick(
        agents: &[AgentPane],
        tab: Option<&str>,
        ws: Option<&str>,
        me: Option<&str>,
    ) -> Option<String> {
        pick_agent(agents, tab, ws, me).map(|agent| agent.pane_id.clone())
    }

    /// The picker-row mapping `send_target`'s Many arm applies to the workspace candidates.
    fn rows(
        agents: &[AgentPane],
        ws: Option<&str>,
        me: Option<&str>,
        tabs: &HashMap<String, String>,
    ) -> Vec<AgentChoice> {
        super::candidates(agents, ws, me, |agent| &agent.workspace_id)
            .into_iter()
            .map(|agent| agent.choice(tabs))
            .collect()
    }

    #[test]
    fn pick_prefers_the_tab_agent_over_the_workspace() {
        let agents = vec![agent("w8:p1", "w8:t1", "w8"), agent("w8:p2", "w8:t2", "w8")];
        // Both share workspace w8; our tab is w8:t2, so its pane wins (HH-TAB-WINS).
        assert_eq!(pick(&agents, Some("w8:t2"), Some("w8"), None), Some("w8:p2".to_string()));
    }

    #[test]
    fn pick_falls_back_to_the_sole_workspace_agent() {
        let agents = vec![agent("w8:p1", "w8:t1", "w8")];
        // No agent shares our tab, but exactly one is in the workspace.
        assert_eq!(pick(&agents, Some("w8:tX"), Some("w8"), None), Some("w8:p1".to_string()));
    }

    #[test]
    fn the_reviewr_pane_excludes_itself_so_the_real_agent_resolves() {
        // Even if herdr listed our own sidebar pane (w8:p5) as an agent alongside the real
        // one (w8:p1), excluding our pane leaves the real agent unambiguous (HH-NOT-SELF).
        let agents = vec![agent("w8:p1", "w8:t1", "w8"), agent("w8:p5", "w8:t1", "w8")];
        assert_eq!(
            pick(&agents, Some("w8:t1"), Some("w8"), Some("w8:p5")),
            Some("w8:p1".to_string())
        );
    }

    #[test]
    fn non_agent_panes_do_not_make_the_tab_ambiguous() {
        // A tab holding one real agent plus a non-agent pane (another plugin's sidebar, a
        // plain shell) resolves to the agent, not an ambiguity refusal (HH-AGENT-PANES, #6).
        let agents = vec![agent("w3:p1", "w3:t1", "w3"), non_agent_pane("w3:p4", "w3:t1", "w3")];
        assert_eq!(
            pick(&agents, Some("w3:t1"), Some("w3"), Some("w3:p5")),
            Some("w3:p1".to_string())
        );
    }

    #[test]
    fn only_non_agent_panes_resolve_no_one() {
        // A tab and workspace holding nothing but non-agent panes resolves no one (HH-AGENT-PANES).
        let agents =
            vec![non_agent_pane("w3:p2", "w3:t1", "w3"), non_agent_pane("w3:p4", "w3:t1", "w3")];
        assert_eq!(pick(&agents, Some("w3:t1"), Some("w3"), None), None);
    }

    #[test]
    fn no_matching_agent_resolves_no_one() {
        let agents = vec![agent("w9:p1", "w9:t1", "w9")];
        // An agent exists, but in another workspace entirely — nothing resolves.
        assert_eq!(pick(&agents, Some("w8:t1"), Some("w8"), None), None);
    }

    #[test]
    fn two_workspace_agents_resolve_no_one() {
        let agents = vec![agent("w8:p1", "w8:t1", "w8"), agent("w8:p2", "w8:t2", "w8")];
        // Neither shares our tab and the workspace has two — tracking refuses to guess.
        assert_eq!(pick(&agents, Some("w8:tZ"), Some("w8"), None), None);
    }

    #[test]
    fn two_tab_agents_resolve_no_one_even_without_a_workspace_id() {
        let agents = vec![agent("w8:p1", "w8:t1", "w8"), agent("w8:p2", "w8:t1", "w8")];
        // Two agents share our tab and no workspace id is available to widen the scope —
        // tracking still refuses to guess between them.
        assert_eq!(pick(&agents, Some("w8:t1"), None, None), None);
    }

    /// One agent carrying the picker-facing fields herdr omits until something sets them.
    fn named(pane: &str, tab: &str, ws: &str, name: Option<&str>) -> AgentPane {
        AgentPane { name: name.map(str::to_string), ..agent(pane, tab, ws) }
    }

    #[test]
    fn a_row_name_prefers_the_rename_then_the_display_agent_then_the_kind() {
        // `herdr agent rename` sets `name`, which wins (specs/herdr-host.md).
        assert_eq!(named("w8:p1", "w8:t1", "w8", Some("release-bot")).row_name(), "release-bot");
        // `--clear` leaves the key present and null, which falls through like an absent one.
        let cleared = named("w8:p1", "w8:t1", "w8", None);
        assert_eq!(cleared.row_name(), "claude");
        // With no kind either, the pane id keeps the row and the success line from going blank.
        let anonymous = AgentPane { agent: None, ..agent("w8:p1", "w8:t1", "w8") };
        assert_eq!(anonymous.row_name(), "w8:p1");
        let displayed = AgentPane {
            agent: None,
            display_agent: Some("Claude".into()),
            ..agent("w8:p1", "w8:t1", "w8")
        };
        assert_eq!(displayed.row_name(), "Claude");
    }

    #[test]
    fn a_row_state_prefers_the_state_label_over_the_wire_spelling() {
        let mut labels = HashMap::new();
        labels.insert("working".to_string(), "thinking".to_string());
        let labelled = AgentPane { state_labels: Some(labels), ..agent("w8:p1", "w8:t1", "w8") };
        assert_eq!(labelled.row_state(), "thinking");
        // herdr 0.7.5 sends no `state_labels`, so every live row falls back to the state itself.
        assert_eq!(agent("w8:p1", "w8:t1", "w8").row_state(), "working");
    }

    #[test]
    fn picker_rows_are_every_workspace_agent_in_herdr_order_with_its_tab_label() {
        let agents = vec![
            agent("w8:p1", "w8:t1", "w8"),
            non_agent_pane("w8:p4", "w8:t1", "w8"),
            named("w8:p2", "w8:t2", "w8", Some("release-bot")),
            agent("w9:p1", "w9:t1", "w9"),
        ];
        let mut tabs = HashMap::new();
        tabs.insert("w8:t1".to_string(), "Grip Outreach".to_string());
        // w8:t2 has no label, so that row shows its state alone.
        let rows = rows(&agents, Some("w8"), Some("w8:p9"), &tabs);
        assert_eq!(
            rows,
            vec![
                AgentChoice {
                    pane_id: "w8:p1".into(),
                    name: "claude".into(),
                    state: "working".into(),
                    tab: "Grip Outreach".into(),
                },
                AgentChoice {
                    pane_id: "w8:p2".into(),
                    name: "release-bot".into(),
                    state: "working".into(),
                    tab: String::new(),
                },
            ]
        );
    }

    #[test]
    fn picker_rows_exclude_the_sidebar_and_every_non_agent_pane() {
        // HH-AGENT-PANES and HH-NOT-SELF, so a shell and our own pane never become rows.
        let agents = vec![
            agent("w3:p1", "w3:t1", "w3"),
            non_agent_pane("w3:p4", "w3:t1", "w3"),
            agent("w3:p5", "w3:t1", "w3"),
        ];
        let rows = rows(&agents, Some("w3"), Some("w3:p5"), &HashMap::new());
        assert_eq!(rows.iter().map(|r| r.pane_id.as_str()).collect::<Vec<_>>(), ["w3:p1"]);
    }

    #[test]
    fn an_agent_list_entry_parses_without_any_of_the_picker_fields() {
        // Exactly what herdr 0.7.5 emits: no `name`, no `display_agent`, no `state_labels`.
        let json = r#"{"result":{"agents":[{"agent":"claude","agent_status":"idle","pane_id":"w8:p1","tab_id":"w8:t1","workspace_id":"w8"}]}}"#;
        let parsed = parse_agents(json).unwrap();
        assert_eq!(parsed[0].row_name(), "claude");
        assert_eq!(parsed[0].row_state(), "idle");
        // And with `name` explicitly null, as `herdr agent rename --clear` leaves it.
        let cleared = r#"{"result":{"agents":[{"agent":"codex","agent_status":"idle","pane_id":"w8:p2","tab_id":"w8:t1","workspace_id":"w8","name":null}]}}"#;
        assert_eq!(parse_agents(cleared).unwrap()[0].row_name(), "codex");
    }

    #[test]
    fn a_tab_list_parses_to_labels_and_an_unlabelled_tab_is_dropped() {
        // The documented envelope (docs/herdr-api-notes.md): `label` can be absent.
        let json = r#"{"result":{"tabs":[{"tab_id":"w8:t1","label":"Grip Outreach","number":1,"pane_count":2},{"tab_id":"w8:t2","number":2,"pane_count":1}]}}"#;
        let labels = parse_tab_labels(json).unwrap();
        assert_eq!(labels.get("w8:t1").map(String::as_str), Some("Grip Outreach"));
        assert!(!labels.contains_key("w8:t2"));
        assert!(parse_tab_labels("[]").is_err());
    }

    #[test]
    fn parse_agents_accepts_only_the_documented_envelope() {
        let wrapped = r#"{"result":{"agents":[{"agent":"claude","agent_status":"working","pane_id":"w8:p1","tab_id":"w8:t1","workspace_id":"w8"}]}}"#;
        assert_eq!(parse_agents(wrapped).unwrap(), [agent("w8:p1", "w8:t1", "w8")]);
        assert!(parse_agents("[]").is_err());
        assert_eq!(serde_json::from_str::<Status>(r#""starting""#).unwrap(), Status::Unknown);
    }
}
