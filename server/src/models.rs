use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const SHARED_MEMBER: &str = "@SHARED";

/// Synthetic `MemberMetricData::name` for the raid-completions chart's aggregate "Total" line -
/// no real member is ever named this, so the frontend can special-case it for styling without a
/// dedicated wire field. Kept in sync with `skill-graph.js`'s matching literal.
pub const RAID_GROUP_TOTAL_LABEL: &str = "Group Total";

/// Helmet/accent colour keys a member row's `color` column may hold. Shared vocabulary with the
/// site's `member-data.js` palette - the hue each key maps to is a frontend-only concern, so
/// only the keys themselves need to agree between the two.
pub const MEMBER_COLOR_PALETTE: [&str; 12] = [
    "yellow", "green", "blue", "red", "purple", "orange", "cyan", "pink", "lime", "teal", "indigo",
    "brown",
];

/// Hue (degrees) each [`MEMBER_COLOR_PALETTE`] key renders as, mirroring the site's
/// `MEMBER_COLOR_HUES` (member-data.js) exactly - `hsl(hue, 70%, 45%)`. Lets server code (the
/// party overlay websocket) hand the RuneLite plugin a decoded hex colour instead of the raw key,
/// so the overlay accent matches the site's colour picker without the plugin needing its own hue
/// table.
const MEMBER_COLOR_HUES: [(&str, f64); 12] = [
    ("yellow", 41.0),
    ("green", 151.0),
    ("blue", 210.0),
    ("red", 355.0),
    ("purple", 288.0),
    ("orange", 25.0),
    ("cyan", 185.0),
    ("pink", 330.0),
    ("lime", 95.0),
    ("teal", 170.0),
    ("indigo", 245.0),
    ("brown", 30.0),
];

/// Resolves a [`MEMBER_COLOR_PALETTE`] key to the hex string its `hsl(hue, 70%, 45%)` rendering
/// works out to. `None` for a key not in [`MEMBER_COLOR_HUES`].
pub fn named_color_to_hex(key: &str) -> Option<String> {
    MEMBER_COLOR_HUES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, hue)| hsl_to_hex(*hue, 0.70, 0.45))
}

fn hsl_to_hex(h: f64, s: f64, l: f64) -> String {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = match h {
        h if h < 60.0 => (c, x, 0.0),
        h if h < 120.0 => (x, c, 0.0),
        h if h < 180.0 => (0.0, c, x),
        h if h < 240.0 => (0.0, x, c),
        h if h < 300.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to_byte = |v: f64| ((v + m) * 255.0).round() as u8;
    format!("#{:02X}{:02X}{:02X}", to_byte(r1), to_byte(g1), to_byte(b1))
}

#[cfg(test)]
mod member_color_tests {
    use super::*;

    #[test]
    fn named_color_to_hex_matches_known_hsl_conversion() {
        // hsl(151, 70%, 45%) -> a mid-tone green.
        assert_eq!(named_color_to_hex("green").as_deref(), Some("#22C375"));
        assert_eq!(named_color_to_hex("not-a-color"), None);
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Coordinates {
    x: i32,
    y: i32,
    plane: i32,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Interacting {
    pub name: String,
    pub scale: i32,
    pub ratio: i32,
    location: Coordinates,
    #[serde(default = "default_last_updated")]
    last_updated: DateTime<Utc>,
}
fn default_last_updated() -> DateTime<Utc> {
    Utc::now()
}

impl Interacting {
    /// The plugin has no other way to say "the player stopped interacting with anything" through
    /// this `Option<Interacting>` field - an absent field already means "no update this batch",
    /// so it sends this empty-name placeholder instead to force a real, storable value change
    /// (needed so `merge_group_member`'s `is_some()` check and the `interacting_last_update`
    /// change-trigger both fire). Every read site must collapse this back to `None` before
    /// exposing it to a client - see `normalize_interacting`.
    pub fn is_cleared(&self) -> bool {
        self.name.is_empty()
    }
}

/// Collapses the plugin's "explicitly not interacting" placeholder (see
/// [`Interacting::is_cleared`]) into a real `None`. Every place that reads a stored/broadcast
/// `interacting` value for external consumption (REST responses, the websocket overlay feed)
/// must pass it through this first.
pub fn normalize_interacting(interacting: Option<Interacting>) -> Option<Interacting> {
    interacting.filter(|i| !i.is_cleared())
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CombatAchievements {
    pub tiers: std::collections::HashMap<String, bool>,
    pub tasks: std::collections::HashMap<String, bool>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SlayerTask {
    pub has_task: bool,
    pub master_name: Option<String>,
    pub task_name: Option<String>,
    pub task_location: Option<String>,
    pub amount_remaining: Option<i32>,
    pub initial_amount: Option<i32>,
    pub points: i32,
    pub streak: i32,
    /// The 3 independently-tracked "tasks completed" counters game-side (Krystilia's wilderness
    /// tasks and Mortimer's Managing Miscellania tasks each keep their own, everything else feeds
    /// the shared "normal" counter - see `SlayerTaskState`'s streak switch). The plugin only ever
    /// sends the single counter matching whatever master is current in `streak`; these 3 fields
    /// are derived and persisted server-side in `update_batcher::merge_slayer_task` by bucketing
    /// each incoming `streak` under `master_name`, carrying the other 2 buckets forward from the
    /// stored row so they don't blank out when the player switches masters. `None` means that
    /// bucket has never been observed yet, not that it's zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streak_normal: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streak_mortimer: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streak_wildy: Option<i32>,
    /// Mortimer's task modifier - one of `"points"`, `"quantity"`, `"clue_rate"`,
    /// `"superior_rate"`, `"xp"` (see `SlayerTaskState::resolveModifierType`). `modifier_value` is
    /// the magnitude (a percent for everything but `"points"`, which is a flat point bonus);
    /// `modifier_negative` only ever applies to `"quantity"` (task size can go up or down, the
    /// other 4 are always positive boosts). `None` for every non-Mortimer task - enforced in
    /// `update_batcher::merge_slayer_task` regardless of what the plugin sends, since the game's
    /// own modifier varbits aren't reliably reset once you leave Mortimer.
    ///
    /// Deliberately NOT `skip_serializing_if`, unlike the `streak_*` buckets above: those are
    /// meant to survive the DB's shallow jsonb merge (`a.slayer_task || b.slayer_task`) when a
    /// flush window doesn't touch them, but a modifier that no longer applies must actively
    /// overwrite the stored row as an explicit JSON `null`, or that same shallow merge would keep
    /// the stale value forever.
    #[serde(default)]
    pub modifier_type: Option<String>,
    #[serde(default)]
    pub modifier_value: Option<i32>,
    #[serde(default)]
    pub modifier_negative: Option<bool>,
}

/// One slayer task's lifecycle, from the plugin's `SlayerTaskCloseEvents` accumulator, under its
/// own "slayer_task_events" upload key (kept separate from `events` - see `GroupMember`). The
/// plugin sends one of these at assignment (`status` "not_started"/"in_progress") and again
/// whenever that same task closes out (`status` "completed"/"cancelled"/"blocked"/"unknown") -
/// both keyed by the same `client_event_id` so the server upserts in place rather than storing
/// two rows per task. See `db::upsert_slayer_task_history_event`.
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SlayerTaskHistoryEvent {
    /// Stable id the plugin generates once per task assignment (not per event) - lets the
    /// assignment and close events for the same task be recognized as the same history row.
    pub client_event_id: String,
    pub task_name: String,
    pub master_name: String,
    /// "not_started" | "in_progress" | "completed" | "cancelled" | "blocked" | "reset" | "unknown"
    pub status: String,
    pub amount_done: i32,
    pub amount_total: i32,
    /// Signed: positive is the points reward for a completed task, negative is the points spent
    /// cancelling/blocking, `None` while the task is still open (not_started/in_progress).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<i32>,
    pub assigned_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    /// Set only by the assignment event, never the close event - see this field's counterpart on
    /// [`SlayerTask`] and `db::upsert_slayer_task_history_event`'s doc comment for why the close
    /// event's upsert doesn't touch these columns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modifier_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modifier_value: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modifier_negative: Option<bool>,
}

/// One row of `get-slayer-task-history`, newest-first.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SlayerTaskHistoryEntry {
    pub task_name: String,
    pub master_name: String,
    pub status: String,
    pub amount_done: i32,
    pub amount_total: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub points: Option<i32>,
    pub assigned_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modifier_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modifier_value: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modifier_negative: Option<bool>,
}

/// One page of `get-slayer-task-history` - offset pagination (page/page_size) rather than a
/// keyset cursor, since the History tab needs first/prev/next/last controls over a known total
/// rather than an infinite-scroll "load more".
#[derive(Serialize, Deserialize)]
pub struct SlayerTaskHistoryPage {
    pub entries: Vec<SlayerTaskHistoryEntry>,
    pub page: i64,
    pub page_size: i64,
    pub total_entries: i64,
    pub total_pages: i64,
}

/// A "most X" tile on the Stats tab - `None` when the member has no history to rank yet.
#[derive(Serialize, Deserialize, Default)]
pub struct SlayerTaskLeader {
    pub name: String,
    pub count: i64,
}

/// The "Most common modifier" tile - same idea as [`SlayerTaskLeader`] but keyed by the
/// `(modifier_type, modifier_negative)` pair rather than a task/master name, since `"quantity"`'s
/// up/down directions are opposite effects and must be counted (and displayed) separately.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlayerModifierLeader {
    pub modifier_type: String,
    pub modifier_negative: bool,
    pub count: i64,
}

/// The "Fastest completion" tile - the completed task with the shortest `closed_at - assigned_at`
/// span. `seconds` rather than a pre-formatted string so the client picks the duration format
/// (matches how every other tile leaves display formatting to the client).
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlayerTaskDurationLeader {
    pub name: String,
    pub seconds: i64,
}

/// All-time slayer task stats for one member, `get-slayer-task-stats`.
#[derive(Serialize, Deserialize)]
pub struct SlayerTaskStats {
    pub tasks_completed: i64,
    pub total_kills: i64,
    pub total_points_earned: i64,
    /// Percentage (0-100) of closed tasks (completed+cancelled+blocked) that were completed.
    pub completion_rate: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub most_killed_task: Option<SlayerTaskLeader>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub most_common_task: Option<SlayerTaskLeader>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub most_common_master: Option<SlayerTaskLeader>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub most_cancelled_task: Option<SlayerTaskLeader>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub most_common_modifier: Option<SlayerModifierLeader>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fastest_completed_task: Option<SlayerTaskDurationLeader>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupMemberName {
    pub name: String,
}

#[derive(Serialize)]
pub struct BlockedMember {
    pub member_name: String,
    pub blocked_at: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenameGroup {
    pub new_name: String,
}

#[derive(Serialize)]
pub struct GroupCredentials {
    pub name: String,
    pub token: String,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct GroupMember {
    #[serde(skip)]
    pub group_id: Option<i64>,
    pub name: String,
    /// Plugin-submitted `client.getAccountHash()`, used to derive `name` server-side from the
    /// linked character's `display_rsn` instead of requiring it to be typed at group setup.
    /// `None` for legacy plugin builds or unlinked characters, which fall back to matching an
    /// existing member row by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_hash: Option<String>,
    /// Admin-assigned helmet/accent colour (one of [`MEMBER_COLOR_PALETTE`]), unconditionally
    /// included rather than gated by the heartbeat's `$1` staleness cutoff like the fields below
    /// - it changes far less often than telemetry, so there's no "since timestamp" to check.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordinates: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quests: Option<Vec<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inventory: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equipment: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_bank: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rune_pouch: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interacting: Option<Interacting>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed_vault: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deposited: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diary_vars: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection_log_v2: Option<Vec<i32>>,
    // Items the plugin only saw via a bulk collection log widget scan (e.g. the player just
    // opened their log to browse), with no matching live drop signal - see
    // `CollectionLogV2Manager` in the plugin. Never diffed for Discord/activity-feed events;
    // `update_group_member` merges it into `collection_log_v2`'s stored baseline before the
    // batcher writes it, then clears it so it never reaches storage under its own name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_log_sync: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub potion_storage: Option<Vec<i32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub special_attack: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_prayers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rich_presence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub combat_achievements: Option<CombatAchievements>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slayer_task: Option<SlayerTask>,
    /// Slayer task assignment/close events from the plugin's `SlayerTaskCloseEvents` accumulator,
    /// under its own "slayer_task_events" upload key. Consumed once per event (upserted into
    /// `groupscape.slayer_task_history` by `client_event_id`), never stored on `GroupMember`
    /// itself - same ephemeral handling as `notable_drops`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slayer_task_events: Option<Vec<SlayerTaskHistoryEvent>>,
    /// Timestamp of the character's most recent portrait mesh upload (`character_mesh.mesh_last_update`),
    /// gated by the same "since timestamp" cutoff as the telemetry fields above. Never sent by the
    /// plugin - it's server-computed in `get_group_data` so an already-open side panel knows to
    /// refetch `/portrait/{member_name}` after a gear/appearance change instead of showing a stale mesh
    /// until the page is reloaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portrait_last_update: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<DateTime<Utc>>,
    /// Discrete kill/death events piggybacked on the same heartbeat, matching
    /// `groupscape-old`'s "ride the tick" ingestion pattern rather than a separate endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<GameEvent>>,
    /// NPC dialogue events from the plugin's `InteractionEvents` accumulator, under its own
    /// "interactions" upload key (kept separate from `events` and `object_interactions` -
    /// the plugin never merges these event streams).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interactions: Option<Vec<DialogueEvent>>,
    /// Object-interaction events from the plugin's `ObjectInteractionEvents` accumulator,
    /// under its own "object_interactions" upload key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_interactions: Option<Vec<ObjectInteractionEvent>>,
    /// Low-HP/wilderness-entry alert events from the plugin's `AlertEvents` accumulator, under
    /// its own "alerts" upload key. Consumed once to trigger a web push, never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alerts: Option<Vec<AlertEvent>>,
    /// Drops crossing the plugin-configured value threshold, from the plugin's
    /// `NotableDropEvents` accumulator, under its own "notableDrops" upload key. Consumed once
    /// to broadcast a chat message (and optionally a Discord post), never stored - same
    /// ephemeral handling as `alerts`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notable_drops: Option<Vec<NotableDropEvent>>,
    /// True for a character that's linked to the group (`character_group_links`) but has no
    /// `members` row yet - i.e. an admin added them (`admin_add_account_to_group`) but their
    /// RuneLite plugin hasn't sent a first telemetry update. Server-computed only, in
    /// `get_group_data`; never sent by the plugin.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
}

/// One item entry in a [`GameEvent::Kill`]'s loot, field names matching the plugin's
/// `PendingKill.toMap()` output verbatim (`itemId`/`quantity`).
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LootItem {
    pub item_id: i32,
    pub quantity: i32,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KillEvent {
    pub npc_id: i32,
    pub npc_name: String,
    pub world_x: i32,
    pub world_y: i32,
    pub plane: i32,
    pub world: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<DateTime<Utc>>,
    /// Stable id the plugin generates once, at the moment this kill is captured (not at send
    /// time) - lets the server recognize a resend of the same buffered event (e.g. after a
    /// connection drop during a server restart whose response never reached the client) as a
    /// duplicate rather than a second kill. Absent from older plugin builds, in which case this
    /// event isn't deduplicated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participants: Option<Vec<String>>,
    /// Absent when the plugin's best-effort loot correlation (`onLoot`) never matched a
    /// pending kill before the next drain - the kill still ships without loot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loot: Option<Vec<LootItem>>,
    /// The account's real in-game kill count, parsed client-side from the "Your X kill count
    /// is: N." chat line. Absent when that line never arrived before the next upload drain -
    /// callers fall back to a server-tracked count in that case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_kc: Option<i32>,
    /// Set only for a "combo" kill the plugin's `ComboKillEvents` synthesizes on a shared reward
    /// chest opening (`npc_name` "Barrows"/"Moons of Peril") - the short display labels of
    /// whichever sub-bosses were actually killed that run, in kill order (e.g. `["Eclipse",
    /// "Blood"]`). Absent for every ordinary single-NPC kill. Reused as-is (not a dedicated event
    /// type) so any future combo-reward boss only needs a plugin-side definition, not a new
    /// server model/message path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_kills: Option<Vec<String>>,
}
impl KillEvent {
    /// `" (Eclipse, Blood, Blue)"`-style suffix for a combo kill's `sub_kills`, empty string for
    /// an ordinary kill - mirrors `RaidDifficulty`'s suffix role in the raid completion message,
    /// but built here rather than as a `RaidDifficulty`-style enum since a combo's sub-boss set is
    /// open-ended (1-6+ entries) rather than one of a few fixed difficulty tiers.
    pub fn sub_kills_suffix(&self) -> String {
        match &self.sub_kills {
            Some(labels) if !labels.is_empty() => format!(" ({})", labels.join(", ")),
            _ => String::new(),
        }
    }
}

/// Distinguishes a chest/instance reward (e.g. Chambers of Xeric, Barrows) from a clue scroll
/// casket - both arrive from the plugin as RuneLite's `LootRecordType.EVENT`, uncorrelated to
/// any kill, unlike [`KillEvent::loot`].
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LootSourceType {
    Chest,
    Clue,
}

/// A chest/instance reward or clue scroll casket opening - has no `npc_id`/participant-split
/// semantics the way [`KillEvent`] does, since there's no "kill" to attach to.
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LootEvent {
    pub source_type: LootSourceType,
    pub source_name: String,
    /// "beginner".."master" - only set when `source_type` is `Clue`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clue_tier: Option<String>,
    pub world_x: i32,
    pub world_y: i32,
    pub plane: i32,
    pub world: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<DateTime<Utc>>,
    pub loot: Vec<LootItem>,
    /// See [`KillEvent::event_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DeathEvent {
    pub world_x: i32,
    pub world_y: i32,
    pub plane: i32,
    pub world: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<DateTime<Utc>>,
    /// Best-effort, from `Actor.getInteracting()` at time of death - never a guaranteed
    /// "killed by" fact, so this is `None` rather than defaulted to any placeholder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub killer_name: Option<String>,
    /// See [`KillEvent::event_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
}

/// The three raid instances tracked for completion events. `Display` renders the wiki-style
/// full name used in both the activity feed sentence and the Discord relay.
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum RaidType {
    Cox,
    Tob,
    Toa,
}
impl RaidType {
    pub fn display_name(self) -> &'static str {
        match self {
            RaidType::Cox => "Chambers of Xeric",
            RaidType::Tob => "Theatre of Blood",
            RaidType::Toa => "Tombs of Amascut",
        }
    }

    /// The `payload.raidType` string the frontend keys its per-raid icon/description lookup on
    /// (`"cox"`/`"tob"`/`"toa"`) - same as the serde tag, spelled out so callers building JSON by
    /// hand (the merge path in `authed.rs`) don't have to round-trip through serde to get it.
    pub fn as_str(self) -> &'static str {
        match self {
            RaidType::Cox => "cox",
            RaidType::Tob => "tob",
            RaidType::Toa => "toa",
        }
    }

    /// Inverse of [`as_str`](Self::as_str) - parses the `raid_type` query param on the
    /// leaderboard/metric-data endpoints. `None` for anything else, including `"all"` (the
    /// caller treats that as "don't filter" rather than an unknown raid).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "cox" => Some(RaidType::Cox),
            "tob" => Some(RaidType::Tob),
            "toa" => Some(RaidType::Toa),
            _ => None,
        }
    }
}

/// CoX/ToB only ever report a *mode* ("Challenge Mode"/"Hard Mode"/`None` for regular); ToA
/// reports a numeric *invocation level* instead. The two are mutually exclusive per [`RaidType`],
/// so this is modeled as an enum rather than two `Option` fields that could both be set or both
/// `None` at once.
#[derive(Deserialize, Serialize, Clone, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RaidDifficulty {
    Mode {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<String>,
    },
    Level {
        level: i32,
    },
}
impl RaidDifficulty {
    /// The merge key component (see `raid_merge`) - two completions only merge when this string
    /// matches exactly, so a level-300 and a level-350 ToA run (or CM vs non-CM CoX) never merge.
    pub fn merge_key(&self) -> String {
        match self {
            RaidDifficulty::Mode { mode } => format!("mode:{}", mode.as_deref().unwrap_or("")),
            RaidDifficulty::Level { level } => format!("level:{}", level),
        }
    }
}

/// One raid-completion report from a single reporting member's client. `loot` is that reporter's
/// own share of the reward-chest loot only - the server sums per-member value when merging
/// multiple reporters' reports into one feed entry (see `raid_merge`).
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RaidCompletionEvent {
    pub raid_type: RaidType,
    pub difficulty: RaidDifficulty,
    pub world_x: i32,
    pub world_y: i32,
    pub plane: i32,
    pub world: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<DateTime<Utc>>,
    pub loot: Vec<LootItem>,
}

/// Discriminated on the plugin's own `"type"` field ("kill"/"death"/"loot"/"raid"), matching
/// `KillLootDeathEvents`'/`RaidCompletionEvents`' transport shape field-for-field.
#[derive(Deserialize, Serialize, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum GameEvent {
    Kill(KillEvent),
    Death(DeathEvent),
    Loot(LootEvent),
    Raid(RaidCompletionEvent),
}
impl GameEvent {
    pub fn event_type(&self) -> &'static str {
        match self {
            GameEvent::Kill(_) => "kill",
            GameEvent::Death(_) => "death",
            GameEvent::Loot(_) => "loot",
            GameEvent::Raid(_) => "raid",
        }
    }

    /// The plugin-generated dedup id, when present - see [`KillEvent::event_id`]. Raid
    /// completions aren't included here since they already have their own merge-based dedup
    /// path (`raid_merge`) rather than going through [`crate::db::insert_activity_event`].
    pub fn event_id(&self) -> Option<&str> {
        match self {
            GameEvent::Kill(kill) => kill.event_id.as_deref(),
            GameEvent::Death(death) => death.event_id.as_deref(),
            GameEvent::Loot(loot) => loot.event_id.as_deref(),
            GameEvent::Raid(_) => None,
        }
    }

    /// When the plugin captured this event, if it sent one. A resend of a buffered event (e.g.
    /// after the client was offline or a connection drop delayed the upload) should be stored
    /// under the moment it actually happened, not the moment it finally reached the server -
    /// otherwise a batch of stale events flushed alongside fresh ones all land at "now" and can
    /// wrongly appear to be one continuous farming session with events reported live.
    pub fn occurred_at(&self) -> Option<DateTime<Utc>> {
        match self {
            GameEvent::Kill(kill) => kill.occurred_at,
            GameEvent::Death(death) => death.occurred_at,
            GameEvent::Loot(loot) => loot.occurred_at,
            GameEvent::Raid(raid) => raid.occurred_at,
        }
    }
}

/// One NPC dialogue event, field names matching `InteractionEvents.onDialogue`'s transport
/// shape verbatim (`groupscape-plugin`'s "interactions" upload key). The plugin always sends
/// `"type": "dialogue"` even though it's the only shape on this key today - kept as a plain
/// field rather than an enum tag like [`GameEvent`] since there's nothing to discriminate yet.
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DialogueEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub npc_id: i32,
    pub npc_name: String,
    pub combat_level: i32,
    pub world_x: i32,
    pub world_y: i32,
    pub plane: i32,
    pub world: i32,
}

/// One object-interaction event, field names matching
/// `ObjectInteractionEvents.onObjectInteraction`'s transport shape verbatim
/// (`groupscape-plugin`'s "object_interactions" upload key). Unlike [`DialogueEvent`], the
/// plugin never tags these with a "type" field.
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ObjectInteractionEvent {
    pub object_id: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    pub world_x: i32,
    pub world_y: i32,
    pub plane: i32,
    pub world: i32,
}

/// Discriminated on the plugin's own `"type"` field ("low_hp"/"wilderness_entry"), matching
/// `AlertEvents`' transport shape field-for-field (`groupscape-plugin`'s "alerts" upload key).
/// Unlike [`GameEvent`], these never land in `activity_events` - they're consumed once, straight
/// off the heartbeat, to trigger a web push and are not stored.
#[derive(Deserialize, Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AlertEvent {
    LowHp(LowHpAlert),
    WildernessEntry(WildernessEntryAlert),
}
impl AlertEvent {
    /// Matches the enum's own serde tag ("low_hp"/"wilderness_entry"/"timer_ready") - used as both
    /// the push payload's `type` field and its notification `tag` (so a fresh low-HP alert
    /// replaces a stale one instead of stacking, per `groupscape-web#39`'s design review).
    pub fn alert_type(&self) -> &'static str {
        match self {
            AlertEvent::LowHp(_) => "low_hp",
            AlertEvent::WildernessEntry(_) => "wilderness_entry",
        }
    }

    /// Low-HP alerts stay on screen until dismissed - unlike the other alert/toast copy in this
    /// app, this one exists specifically for when the player isn't looking at the tab, so letting
    /// it auto-vanish after a few seconds defeats the point. Wilderness entry and timer-ready are
    /// informational and auto-dismiss like a normal OS notification.
    pub fn requires_interaction(&self) -> bool {
        matches!(self, AlertEvent::LowHp(_))
    }

    /// Push notification title/body, mirroring `groupscape-old`'s alert-push copy, with the
    /// member name appended so a multi-character account can tell which one fired.
    pub fn push_title_and_body(&self, member_name: &str) -> (String, String) {
        match self {
            AlertEvent::LowHp(alert) => (
                format!("Low HP — {}", member_name),
                format!("{}/{} HP remaining", alert.current_hp, alert.max_hp),
            ),
            AlertEvent::WildernessEntry(alert) => (
                format!("Wilderness entry — {}", member_name),
                format!("Entered level {} wilderness", alert.wilderness_level),
            ),
        }
    }
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LowHpAlert {
    pub current_hp: i32,
    pub max_hp: i32,
    pub world_x: i32,
    pub world_y: i32,
    pub plane: i32,
    pub world: i32,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WildernessEntryAlert {
    pub wilderness_level: i32,
    pub world_x: i32,
    pub world_y: i32,
    pub plane: i32,
    pub world: i32,
}

/// Discriminates a [`NotableDropEvent`]'s loot source, matching the plugin's own
/// `LootRecordType`-derived tag verbatim (`groupscape-plugin`'s `notableDropSourceType`).
/// Drives which chat-message flavor [`NotableDropEvent::to_message`] picks.
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DropSourceType {
    Kill,
    Pvp,
    Chest,
    Pickpocket,
    Unknown,
}

/// One notable-drop event, field names matching the plugin's `NotableDropEvents.onNotableDrop`
/// transport shape verbatim (`groupscape-plugin`'s "notableDrops" upload key). `item_name`/
/// `item_value` describe the single highest-value item in the drop (the chat message highlights
/// one item rather than listing everything); `total_value` is the full drop's combined GE value,
/// which is what actually gates whether this event exists at all (the plugin only emits one once
/// the total crosses its configured threshold).
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NotableDropEvent {
    pub source_type: DropSourceType,
    pub source_name: String,
    pub item_id: i32,
    pub item_name: String,
    pub item_value: i64,
    pub total_value: i64,
}
impl NotableDropEvent {
    /// Builds the chat/Discord message text server-side (rather than in the plugin) so wording
    /// can change without a plugin release, and so the same string can be relayed verbatim to
    /// both the roster websocket and a Discord embed.
    pub fn to_message(&self, member_name: &str) -> String {
        let item_value = format_gp(self.item_value);
        let total_value = format_gp(self.total_value);
        match self.source_type {
            DropSourceType::Kill => format!(
                "{} received a drop from {}: {} ({} gp) — total {} gp",
                member_name, self.source_name, self.item_name, item_value, total_value
            ),
            DropSourceType::Chest => format!(
                "{} opened {} and got a drop: {} ({} gp) — total {} gp",
                member_name, self.source_name, self.item_name, item_value, total_value
            ),
            DropSourceType::Pickpocket => format!(
                "{} pickpocketed a drop from {}: {} ({} gp) — total {} gp",
                member_name, self.source_name, self.item_name, item_value, total_value
            ),
            DropSourceType::Pvp => format!(
                "{} got a drop from killing {}: {} ({} gp) — total {} gp",
                member_name, self.source_name, self.item_name, item_value, total_value
            ),
            DropSourceType::Unknown => format!(
                "{} got a drop: {} ({} gp) — total {} gp",
                member_name, self.item_name, item_value, total_value
            ),
        }
    }
}

/// One reporting member's contribution to a merged raid-completion [`ActivityEvent`] payload -
/// see `raid_merge` for how multiple members' [`RaidCompletionEvent`]s fold into one row.
#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RaidParticipant {
    pub member_name: String,
    pub value: i64,
    pub loot: Vec<LootItem>,
}

/// The `groupscape.activity_events.payload` shape for `event_type = "raid"`, built by
/// `raid_merge` rather than a straight `serde_json::to_value(&RaidCompletionEvent)` - a merged
/// row carries every reporting member's contribution, not just the first reporter's.
#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RaidCompletionPayload {
    pub raid_type: RaidType,
    pub difficulty: RaidDifficulty,
    pub participants: Vec<RaidParticipant>,
    pub total_value: i64,
    /// `true` while still inside the 5-minute merge window and eligible to receive more
    /// participants; flipped to `false` by `raid_merge`'s finalize step, at which point the one
    /// websocket/Discord relay for this completion fires.
    pub merge_open: bool,
}
/// Sums a raid participant's loot at current GE prices - unpriced items (untradeable uniques,
/// GE-cache misses) contribute 0 rather than failing the whole completion.
pub fn raid_loot_value(loot: &[LootItem], ge_prices: &GEPrices) -> i64 {
    loot.iter()
        .map(|item| ge_prices.get(&item.item_id).copied().unwrap_or(0) * item.quantity as i64)
        .sum()
}

impl RaidCompletionPayload {
    pub fn first(reporter: &str, event: &RaidCompletionEvent, ge_prices: &GEPrices) -> Self {
        let value = raid_loot_value(&event.loot, ge_prices);
        RaidCompletionPayload {
            raid_type: event.raid_type,
            difficulty: event.difficulty.clone(),
            participants: vec![RaidParticipant {
                member_name: reporter.to_string(),
                value,
                loot: event.loot.clone(),
            }],
            total_value: value,
            merge_open: true,
        }
    }

    /// Folds another reporting member's contribution into this payload in place - used by
    /// `raid_merge` when a second (or third, ...) party member's `RaidCompletionEvent` arrives
    /// for the same raid+difficulty within the merge window. No-ops if `member_name` already
    /// reported (a member's own client should never report the same completion twice, but this
    /// keeps a retry/duplicate-heartbeat from double-counting their loot).
    pub fn append(&mut self, reporter: &str, event: &RaidCompletionEvent, ge_prices: &GEPrices) {
        if self.participants.iter().any(|p| p.member_name == reporter) {
            return;
        }
        let value = raid_loot_value(&event.loot, ge_prices);
        self.participants.push(RaidParticipant {
            member_name: reporter.to_string(),
            value,
            loot: event.loot.clone(),
        });
        self.total_value += value;
    }

    /// The one message this completion ever relays (websocket + Discord), built once at finalize
    /// time so a 4-member raid never produces four near-duplicate "X completed..." posts.
    pub fn to_message(&self) -> String {
        let names: Vec<&str> = self
            .participants
            .iter()
            .map(|p| p.member_name.as_str())
            .collect();
        let name_list = join_names(&names);
        let suffix = match &self.difficulty {
            RaidDifficulty::Level { level } if *level > 0 => format!(" (level {})", level),
            RaidDifficulty::Level { .. } => String::new(),
            RaidDifficulty::Mode { mode: Some(mode) } => format!(" ({})", mode),
            RaidDifficulty::Mode { mode: None } => String::new(),
        };
        let gp_suffix = if names.len() > 1 { "gp total" } else { "gp" };
        format!(
            "{} completed {}{} — worth {} {}",
            name_list,
            self.raid_type.display_name(),
            suffix,
            format_gp(self.total_value),
            gp_suffix
        )
    }
}

fn join_names(names: &[&str]) -> String {
    match names.len() {
        0 => String::new(),
        1 => names[0].to_string(),
        2 => format!("{} and {}", names[0], names[1]),
        _ => {
            let (last, rest) = names.split_last().unwrap();
            format!("{}, and {}", rest.join(", "), last)
        }
    }
}

pub(crate) fn format_gp(value: i64) -> String {
    let s = value.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    let grouped: String = out.chars().rev().collect();
    if value < 0 {
        format!("-{}", grouped)
    } else {
        grouped
    }
}

#[derive(Serialize, Deserialize)]
pub struct ActivityEvent {
    pub id: i64,
    pub session_id: i64,
    pub member_name: String,
    pub event_type: String,
    pub occurred_at: DateTime<Utc>,
    pub payload: serde_json::Value,
    /// Reaction/comment data - only ever populated by `get_activity_events` (never by
    /// `list_kill_events`/`list_loot_and_kill_events_page`, the loot log's readers of this same
    /// struct), and only when the group has `activity_reactions_enabled` on. `skip_serializing_if`
    /// keeps these keys fully absent from the JSON (not just `null`) when the feature is off, per
    /// the "must render exactly as before" requirement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reactions: Option<Vec<ActivityReactionCount>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_reaction: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_count: Option<i64>,
}

/// The 5 reaction types likes/comments supports - kept as a plain string on the wire (validated
/// against this list server-side) rather than a serde enum, so a 6th type is just a data change.
pub const ACTIVITY_REACTION_KINDS: [&str; 5] = ["like", "gg", "lol", "rare", "f"];

#[derive(Serialize, Deserialize, Clone)]
pub struct ActivityReactionCount {
    pub reaction: String,
    pub count: i64,
}

/// Response body for react/unreact and the batch `get-activity-reactions` lookup.
#[derive(Serialize, Deserialize, Clone)]
pub struct ActivityReactionsSummary {
    pub reactions: Vec<ActivityReactionCount>,
    /// `None` when the acting account has no reaction on this event (either it never reacted, or
    /// this just toggled its own reaction off).
    pub my_reaction: Option<String>,
    /// Only meaningful on the batch `get-activity-reactions` lookup (the feed's comment-count
    /// badge needs this alongside reactions in the same poll); `react_to_activity_event`'s single-
    /// event response always sends `0` here since reacting never changes the comment count.
    #[serde(default)]
    pub comment_count: i64,
}

/// Group-wide activity feed setting - one combined toggle for both likes and comments (§ product
/// spec: "one combined boolean toggle per group"). `GET` is reachable by any group member (the
/// feed needs it to decide whether to render reaction UI at all); only `PUT` is gated to
/// `ManageSettings`.
#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
pub struct ActivitySettings {
    pub reactions_enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReactToActivityEventRequest {
    pub reaction: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ActivityComment {
    pub comment_id: i64,
    pub member_name: String,
    pub comment_text: String,
    pub created_at: DateTime<Utc>,
}

/// Response body for both `list_activity_comments` and a successful `add_activity_comment` - the
/// `n / 10 comments` indicator needs the count alongside the list/new comment on every response
/// rather than as a separate round trip.
#[derive(Serialize, Clone)]
pub struct ActivityCommentsPage {
    pub comments: Vec<ActivityComment>,
    pub comment_count: i64,
}

pub const ACTIVITY_COMMENT_CAP: i64 = 10;
/// Matches the site's `men-input`/discord-url-style free-text fields elsewhere - generous enough
/// for a real reaction, short enough that the feed row doesn't need to clamp/scroll it.
pub const ACTIVITY_COMMENT_MAX_LEN: usize = 300;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddActivityCommentRequest {
    pub comment_text: String,
}

/// A `groupscape.chat_messages` row, as returned by both backfill and a successful send. Own
/// table, own surfaces (side-panel tab / webapp chat panel / in-game overlay) - deliberately not
/// part of the activity feed or `activity_event_comments`.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub message_id: i64,
    pub member_name: Option<String>,
    pub message_text: String,
    pub created_at: DateTime<Utc>,
}

/// Backfill cap - see the "Chat history and backfill behavior" spec ticket. Bounds payload size
/// after a long gap between connects.
pub const CHAT_BACKFILL_CAP: i64 = 200;
/// Truncated silently, not rejected - see the "Message formatting and length limits" spec ticket.
pub const CHAT_MESSAGE_MAX_LEN: usize = 150;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendChatMessageRequest {
    pub text: String,
}

/// One item entry within a [`LootLogEvent`] - the loot log's per-event, per-item view (unlike
/// the deleted `LootSummaryRow`, this doesn't pre-aggregate across events, so the client can do
/// the 45-minute session merge itself from raw per-event timestamps).
#[derive(Serialize, Deserialize)]
pub struct LootLogItem {
    pub item_id: i32,
    /// Only set when the item is in the curated `drop_rates` table - `None` falls back
    /// client-side to the full item catalog (see loot-log-tile.js's `itemName` getter).
    pub item_name: Option<String>,
    pub quantity: i32,
    pub unit_value: Option<i64>,
    pub total_value: i64,
    pub rarity: Option<String>,
    pub is_unique: bool,
    pub drop_rate: Option<String>,
    /// Whether this specific item satisfied an item-level search clause (value/quantity/id),
    /// used to dim non-matching items within an otherwise-matched session entry. `None` when no
    /// search is active, or when the event matched by a non-item clause (member/source/level) -
    /// see `loot_log_search`/`authed::build_matching_loot_log_event` for the dimming rule.
    pub matched: Option<bool>,
}

/// One raw `kill`/`loot` activity event, normalized for the Loot Log page. The client groups
/// these into farming-session "entries" (consecutive same member/source events <=45min apart) -
/// see loot-log-page.js.
#[derive(Serialize, Deserialize)]
pub struct LootLogEvent {
    pub member_name: String,
    pub occurred_at: DateTime<Utc>,
    pub source_name: String,
    /// "kill" | "chest" | "clue"
    pub source_type: String,
    /// "beginner".."master" - only set when `source_type` is "clue".
    pub clue_tier: Option<String>,
    pub items: Vec<LootLogItem>,
}

/// One page of `get-loot-log`, newest-first.
#[derive(Serialize, Deserialize)]
pub struct LootLogPage {
    pub events: Vec<LootLogEvent>,
    /// Cursor for the next page - `None` means there's no more raw history to scan (see
    /// `scan_exhausted`).
    pub next_before: Option<DateTime<Utc>>,
    /// True when `next_before` is `None` because history truly ended, as opposed to the server
    /// just hitting its per-request scan cap (in which case there may still be more).
    pub scan_exhausted: bool,
}

/// All-time (or all-matching-search) totals for the loot log's summary bar - a full unbounded
/// scan since it only needs to accumulate two numbers, unlike `get_loot_log`'s paginated rows.
#[derive(Serialize, Deserialize)]
pub struct LootLogSummary {
    pub total_value: i64,
    pub event_count: i64,
}

/// Wire shape for `GET .../get-item-bonuses` - the equip-screen bonus panel's Attack/Defence/
/// Other-bonuses columns, scraped from the OSRS Wiki and cached server-side (30-day TTL, see
/// [`crate::item_bonuses`]).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ItemBonusesResponse {
    pub item_id: i32,
    pub attack: CombatStyleBonuses,
    pub defence: CombatStyleBonuses,
    pub melee_strength: i32,
    pub ranged_strength: i32,
    /// Tenths of a percent (30 = 3.0%), so it round-trips exactly without floats.
    pub magic_damage: i32,
    pub prayer: i32,
    /// Game ticks. `None` for non-weapon equipment - the wiki's `{{Infobox Bonuses}}` template
    /// omits `speed` entirely for those.
    pub attack_speed: Option<i32>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CombatStyleBonuses {
    pub stab: i32,
    pub slash: i32,
    pub crush: i32,
    pub magic: i32,
    pub ranged: i32,
}

#[derive(Serialize, Deserialize)]
pub struct GroupSession {
    pub id: i64,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}
#[derive(Serialize)]
pub struct AggregateSkillData {
    pub time: DateTime<Utc>,
    pub data: Vec<i32>,
}
#[derive(Serialize)]
pub struct MemberSkillData {
    pub name: String,
    pub skill_data: Vec<AggregateSkillData>,
}
pub type GroupSkillData = Vec<MemberSkillData>;
#[derive(Serialize, Deserialize)]
pub struct MetricDataPoint {
    pub time: DateTime<Utc>,
    pub value: i64,
}
#[derive(Serialize, Deserialize)]
pub struct MemberMetricData {
    pub name: String,
    pub metric_data: Vec<MetricDataPoint>,
}
pub type GroupMetricData = Vec<MemberMetricData>;
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateGroup {
    pub name: String,
    pub member_names: Vec<String>,
    #[serde(default, skip_serializing)]
    pub captcha_response: String,
    #[serde(default = "default_token")]
    #[serde(skip_deserializing)]
    pub token: String,
}
fn default_token() -> String {
    uuid::Uuid::new_v4().hyphenated().to_string()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AmIInGroupRequest {
    pub member_name: String,
}
#[derive(Deserialize)]
pub struct WikiGEPrice {
    pub high: Option<i64>,
    pub low: Option<i64>,
}
#[derive(Deserialize)]
pub struct WikiGEPrices {
    pub data: std::collections::HashMap<i32, WikiGEPrice>,
}
pub type GEPrices = std::collections::HashMap<i32, i64>;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterAccount {
    pub username: String,
    pub password: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginAccount {
    pub username: String,
    pub password: String,
}
/// Discord's redirect back to `/discord/callback` - not `deny_unknown_fields` since Discord
/// controls this query string, not us.
#[derive(Deserialize)]
pub struct DiscordCallbackQuery {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
}
#[derive(Serialize)]
pub struct Account {
    pub id: i64,
    /// `None` for a Discord-only account that has never set a username/password.
    pub username: Option<String>,
    pub discord_name: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Set by an admin password reset; the frontend redirects to the password-change section
    /// as soon as it sees this on any authed response, rather than waiting for the 403 the
    /// gate returns on the next *other* authed endpoint.
    pub must_change_password: bool,
}
#[derive(Serialize)]
pub struct AuthenticatedAccount {
    pub account: Account,
    pub token: String,
    /// Only set on `register`/first-ever Discord login, the one time the raw API key is
    /// available to hand back - `login` always returns `None`, the key is never re-shown.
    pub api_key: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateAccountUsername {
    pub username: String,
}
/// No `current_password` field - an active session is treated as sufficient proof of identity
/// for this mutation, same as `UpdateAccountUsername` above.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeAccountPassword {
    pub new_password: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkCharacter {
    pub account_hash: String,
    pub rsn: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkCharacterToGroup {
    pub character_id: i64,
    pub group_name: String,
    pub group_token: String,
}
/// Mirrors the browser's `PushSubscriptionJSON` shape verbatim so the site can forward its
/// `pushManager.subscribe()` result straight through without reshaping it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscribePush {
    pub endpoint: String,
    pub keys: PushSubscriptionKeys,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PushSubscriptionKeys {
    pub p256dh: String,
    pub auth: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnsubscribePush {
    pub endpoint: String,
}
#[derive(Serialize)]
pub struct CharacterGroupLink {
    pub character_id: i64,
    pub group_id: i64,
    pub linked_at: DateTime<Utc>,
}
impl From<crate::db::CharacterGroupLink> for CharacterGroupLink {
    fn from(link: crate::db::CharacterGroupLink) -> Self {
        CharacterGroupLink {
            character_id: link.character_id,
            group_id: link.group_id,
            linked_at: link.linked_at,
        }
    }
}
#[derive(Serialize)]
pub struct Character {
    pub id: i64,
    pub account_hash: String,
    pub display_rsn: String,
    pub bound_at: DateTime<Utc>,
    pub status: String,
    pub combat_level: Option<i16>,
    pub total_level: Option<i32>,
    /// A real "not in a group" signal only on the account-scoped character list (see
    /// `list_characters_for_account_with_group_status`), which joins against
    /// `character_group_links`. Every other endpoint that returns `Character` (confirm,
    /// remove-pending, unlink, link) always sends `None` here regardless of actual group
    /// status - they don't do that join, so absence isn't meaningful there.
    pub group_id: Option<i64>,
    /// Same "only meaningful on the account-scoped list" caveat as `group_id` above.
    pub group_name: Option<String>,
}
impl From<crate::db::Character> for Character {
    fn from(character: crate::db::Character) -> Self {
        Character {
            id: character.id,
            account_hash: character.account_hash,
            display_rsn: character.display_rsn,
            bound_at: character.bound_at,
            status: character.status,
            combat_level: character.combat_level,
            total_level: character.total_level,
            group_id: None,
            group_name: None,
        }
    }
}
impl From<crate::db::CharacterWithGroupStatus> for Character {
    fn from(entry: crate::db::CharacterWithGroupStatus) -> Self {
        let mut character = Character::from(entry.character);
        character.group_id = entry.group_id;
        character.group_name = entry.group_name;
        character
    }
}
/// Returned once from `register`/first Discord login and from `regenerate_api_key` - the raw
/// key is never re-shown after that (only its hash is persisted).
#[derive(Serialize)]
pub struct AccountApiKey {
    pub api_key: String,
}
/// Sent by the plugin independently of group-link status (see `character_identity_scope` in
/// main.rs) so a pending, not-yet-grouped character's RSN reaches the site's confirm card.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentifyCharacter {
    pub rsn: String,
    #[serde(default)]
    pub combat_level: Option<i16>,
    #[serde(default)]
    pub total_level: Option<i32>,
}

/// Per-member permission toggles, ported from `groupscape-old`'s `PermissionKey`
/// (`repositories/memberships.ts`). All keys default `false` for a new member (§6) - the
/// group admin's implicit all-permissions override lives outside this struct, computed from
/// `groups.admin_account_id` (see the "group admin has all permissions by default" ticket).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PermissionFlags {
    pub invite_members: bool,
    pub regenerate_group_key: bool,
    pub kick_members: bool,
    pub manage_settings: bool,
    pub manage_permissions: bool,
    pub post_map_markers: bool,
    pub post_callouts: bool,
    pub manage_goals: bool,
    pub manage_discord: bool,
    pub manage_events: bool,
}
impl PermissionFlags {
    pub fn all_true() -> Self {
        PermissionFlags {
            invite_members: true,
            regenerate_group_key: true,
            kick_members: true,
            manage_settings: true,
            manage_permissions: true,
            post_map_markers: true,
            post_callouts: true,
            manage_goals: true,
            manage_discord: true,
            manage_events: true,
        }
    }

    pub fn get(&self, key: PermissionKey) -> bool {
        match key {
            PermissionKey::InviteMembers => self.invite_members,
            PermissionKey::RegenerateGroupKey => self.regenerate_group_key,
            PermissionKey::KickMembers => self.kick_members,
            PermissionKey::ManageSettings => self.manage_settings,
            PermissionKey::ManagePermissions => self.manage_permissions,
            PermissionKey::PostMapMarkers => self.post_map_markers,
            PermissionKey::PostCallouts => self.post_callouts,
            PermissionKey::ManageGoals => self.manage_goals,
            PermissionKey::ManageDiscord => self.manage_discord,
            PermissionKey::ManageEvents => self.manage_events,
        }
    }
}

/// Response body for `/get-my-permissions`: the caller's effective flags plus their own member
/// name in this group (`None` if their account has no member row here yet). Lets the client
/// identify which roster row is "you" without a second round trip, e.g. to hide the
/// remove/block-self controls.
#[derive(Serialize, Debug)]
pub struct MyPermissions {
    pub member_name: Option<String>,
    #[serde(flatten)]
    pub flags: PermissionFlags,
}

/// One key per `PermissionFlags` toggle - lets callers reference a permission by name (e.g.
/// enforcement middleware) instead of reaching into the struct field directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionKey {
    InviteMembers,
    RegenerateGroupKey,
    KickMembers,
    ManageSettings,
    ManagePermissions,
    PostMapMarkers,
    PostCallouts,
    ManageGoals,
    ManageDiscord,
    ManageEvents,
}

/// Partial update over [`PermissionFlags`] - `None` per field leaves the stored value
/// untouched.
#[derive(Deserialize, Clone, Copy, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct PermissionFlagsPatch {
    #[serde(default)]
    pub invite_members: Option<bool>,
    #[serde(default)]
    pub regenerate_group_key: Option<bool>,
    #[serde(default)]
    pub kick_members: Option<bool>,
    #[serde(default)]
    pub manage_settings: Option<bool>,
    #[serde(default)]
    pub manage_permissions: Option<bool>,
    #[serde(default)]
    pub post_map_markers: Option<bool>,
    #[serde(default)]
    pub post_callouts: Option<bool>,
    #[serde(default)]
    pub manage_goals: Option<bool>,
    #[serde(default)]
    pub manage_discord: Option<bool>,
    #[serde(default)]
    pub manage_events: Option<bool>,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
pub struct GroupPermissions {
    pub group_id: i64,
    pub account_id: i64,
    #[serde(flatten)]
    pub flags: PermissionFlags,
}

/// [`GroupPermissions`] plus the display name the permission-management UI lists a member
/// under - `group_permissions` only knows `account_id`, not any RSN, so this joins in the
/// most-recently-bound character's `display_rsn` for that account. `is_admin` marks the
/// group's implicit all-permissions holder (`flags` is still that account's real, mostly-false
/// stored row - the site renders admins as locked/all-on rather than trusting `flags` for them).
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct GroupMemberPermissions {
    pub account_id: i64,
    pub display_rsn: String,
    pub is_admin: bool,
    /// `None` for an account whose linked character has no member row yet (e.g. joined before
    /// this feature existed and hasn't reconnected to trigger the backfill in
    /// `ensure_member_for_linked_character`).
    pub color: Option<String>,
    #[serde(flatten)]
    pub flags: PermissionFlags,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateMemberColorRequest {
    pub account_id: i64,
    pub color: String,
}

/// `kind` is one of [`DiscordWebhookSettings`]'s own `notify_*` field names - the settings page's
/// notification checkboxes already carry that name in their `data-key`, so the "Send test
/// message" button next to each one just forwards it as-is.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestDiscordNotificationRequest {
    pub kind: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateGroupPermissionsRequest {
    pub account_id: i64,
    #[serde(flatten)]
    pub patch: PermissionFlagsPatch,
}

/// `webhook_url: None` disables dispatch (stored as `NULL`) rather than being an "unset,
/// leave alone" patch field like [`PermissionFlagsPatch`] - the discord settings form always
/// submits its full state, mirroring `groupscape-old`'s `PUT /groups/:groupId/discord`.
#[derive(Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct DiscordWebhookSettings {
    pub webhook_url: Option<String>,
    pub notify_kills: bool,
    pub notify_deaths: bool,
    /// Replaces the old separate `notify_loot`/`notify_notable_drops` toggles - one "Drops"
    /// switch, gated by `drops_min_value` regardless of whether the drop came off a kill or a
    /// plugin-side notable-drop alert.
    pub notify_drops: bool,
    pub drops_min_value: i64,
    /// When set, `drops_min_value` is ignored entirely - only items `drop_rates::lookup` marks
    /// `is_unique` for their source NPC are posted. Items with no curated entry are treated as
    /// not unique (excluded), even if untradeable.
    pub drops_unique_only: bool,
    pub notify_raids: bool,
    pub notify_combat_achievements: bool,
    pub notify_collection_log: bool,
    pub notify_quests: bool,
    pub notify_diaries: bool,
    /// Split out of `notify_drops` - a pet item is excluded from the "Drops" line and posted as
    /// its own "Pet" embed instead when this is on.
    pub notify_pets: bool,
    /// Split out of `notify_drops` - a clue casket opening posts as its own "Clue casket" embed
    /// (with a tier field) instead of riding the generic "Drops" line when this is on.
    pub notify_clues: bool,
    /// A member reaches a skill level that's a multiple of `level_up_interval` (level 99 always
    /// posts regardless). Decoupled from the activity feed's own fixed milestone schedule (see
    /// `progress_events::diff_skills_fine`) - Discord's cadence is independently configurable.
    pub notify_level_ups: bool,
    /// Post only when the reached level is a multiple of this (e.g. 5 -> 70, 75, 80, 85...), plus
    /// level 99 unconditionally. One of 1, 5, or 10 - not validated server-side beyond that it's
    /// a positive divisor check at dispatch time (`level % interval == 0`).
    pub level_up_interval: i32,
}

#[derive(Deserialize)]
pub struct CaptchaVerifyResponse {
    pub success: bool,
    // NOTE: unused
    // #[serde(rename = "error-codes", default)]
    // pub error_codes: std::vec::Vec<String>,
}

fn default_admin_page() -> i64 {
    1
}
fn default_admin_page_size() -> i64 {
    25
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminGroupsQuery {
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default = "default_admin_page")]
    pub page: i64,
    #[serde(default = "default_admin_page_size")]
    pub page_size: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminPageQuery {
    #[serde(default = "default_admin_page")]
    pub page: i64,
    #[serde(default = "default_admin_page_size")]
    pub page_size: i64,
}

#[derive(Serialize)]
pub struct AdminGroupSummary {
    pub group_id: i64,
    pub group_name: String,
    pub version: i32,
    pub member_count: i64,
    pub status: String,
}

#[derive(Serialize)]
pub struct AdminGroupsResponse {
    pub groups: Vec<AdminGroupSummary>,
    pub total: i64,
}

#[derive(Serialize)]
pub struct AdminGroupMember {
    pub member_id: i64,
    pub member_name: String,
}

#[derive(Serialize)]
pub struct AdminGroupDetail {
    pub group_id: i64,
    pub group_name: String,
    pub version: i32,
    pub status: String,
    pub reason: Option<String>,
    pub members: Vec<AdminGroupMember>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminModerationRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

/// One member/data-type pair from the group-detail "Data management" matrix - see
/// `db::admin_clear_member_data`. `data_type` is one of `collection_log`, `combat_achievements`,
/// `skill_xp_history`, `bank_value_history`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminClearMemberDataItem {
    pub member_id: i64,
    pub data_type: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminClearMemberDataRequest {
    pub items: Vec<AdminClearMemberDataItem>,
}

/// Ids of specific `activity_events` rows an admin picked from the activity feed to delete for
/// everyone - see `db::admin_delete_activity_events`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminDeleteActivityEventsRequest {
    pub ids: Vec<i64>,
}


#[derive(Serialize)]
pub struct AdminAuditLogEntry {
    pub id: i64,
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub detail: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize)]
pub struct AdminAuditLogResponse {
    pub entries: Vec<AdminAuditLogEntry>,
    pub total: i64,
}

#[derive(Serialize)]
pub struct AdminAccountsSummary {
    pub count: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminAccountsQuery {
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub group_id: Option<i64>,
    #[serde(default = "default_admin_page")]
    pub page: i64,
    #[serde(default = "default_admin_page_size")]
    pub page_size: i64,
}

#[derive(Serialize)]
pub struct AdminAccountSummary {
    pub id: i64,
    pub username: Option<String>,
    pub status: String,
    pub must_change_password: bool,
    pub locked_out: bool,
    pub created_at: DateTime<Utc>,
    pub last_visit_at: Option<DateTime<Utc>>,
    pub is_online: bool,
}

#[derive(Serialize)]
pub struct AdminAccountsResponse {
    pub accounts: Vec<AdminAccountSummary>,
    pub total: i64,
}

#[derive(Serialize)]
pub struct AdminAccountGroup {
    pub group_id: i64,
    pub group_name: String,
    pub is_owner: bool,
}

#[derive(Serialize)]
pub struct AdminAccountCharacter {
    pub id: i64,
    pub display_rsn: String,
    pub status: String,
    pub bound_at: DateTime<Utc>,
    pub group_id: Option<i64>,
    pub group_name: Option<String>,
}

#[derive(Serialize)]
pub struct AdminAccountSession {
    pub session_id: i64,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Serialize)]
pub struct AdminAccountDetail {
    pub id: i64,
    pub username: Option<String>,
    pub status: String,
    pub must_change_password: bool,
    pub locked_out: bool,
    pub created_at: DateTime<Utc>,
    pub last_visit_at: Option<DateTime<Utc>>,
    pub groups: Vec<AdminAccountGroup>,
    pub characters: Vec<AdminAccountCharacter>,
    pub session_count: i64,
}

#[derive(Serialize)]
pub struct AdminPasswordResetResponse {
    pub temp_password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminSetAccountStatus {
    pub status: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminSetAccountUsername {
    pub username: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminAddAccountToGroup {
    pub group_id: i64,
}

#[derive(Serialize)]
pub struct AdminSearchResponse {
    pub accounts: Vec<AdminAccountSummary>,
    pub groups: Vec<AdminGroupSummary>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminSearchQuery {
    pub q: String,
}

#[derive(Serialize)]
pub struct AdminDashboard {
    pub total: i64,
    pub active: i64,
    pub suspended: i64,
    pub banned: i64,
    pub deleted: i64,
    pub live_sessions: i64,
    pub locked_out: i64,
    pub recent_audit: Vec<AdminAuditLogEntry>,
}

#[cfg(test)]
mod raid_tests {
    use super::*;

    #[test]
    fn game_event_raid_round_trips_level_difficulty() {
        let json = serde_json::json!({
            "type": "raid",
            "raidType": "toa",
            "difficulty": {"kind": "level", "level": 350},
            "worldX": 1,
            "worldY": 2,
            "plane": 0,
            "world": 301,
            "loot": [{"itemId": 5, "quantity": 2}]
        });
        let event: GameEvent = serde_json::from_value(json).unwrap();
        match &event {
            GameEvent::Raid(raid) => {
                assert_eq!(raid.raid_type, RaidType::Toa);
                assert!(matches!(raid.difficulty, RaidDifficulty::Level { level: 350 }));
                assert_eq!(raid.loot.len(), 1);
            }
            _ => panic!("expected GameEvent::Raid"),
        }
        assert_eq!(event.event_type(), "raid");
    }

    #[test]
    fn game_event_raid_round_trips_mode_difficulty_with_no_mode() {
        let json = serde_json::json!({
            "type": "raid",
            "raidType": "tob",
            "difficulty": {"kind": "mode"},
            "worldX": 0,
            "worldY": 0,
            "plane": 0,
            "world": 301,
            "loot": []
        });
        let event: GameEvent = serde_json::from_value(json).unwrap();
        match &event {
            GameEvent::Raid(raid) => {
                assert_eq!(raid.raid_type, RaidType::Tob);
                assert!(matches!(raid.difficulty, RaidDifficulty::Mode { mode: None }));
            }
            _ => panic!("expected GameEvent::Raid"),
        }
    }

    #[test]
    fn merge_key_distinguishes_level_and_mode() {
        assert_ne!(
            RaidDifficulty::Level { level: 300 }.merge_key(),
            RaidDifficulty::Level { level: 350 }.merge_key()
        );
        assert_ne!(
            RaidDifficulty::Mode { mode: None }.merge_key(),
            RaidDifficulty::Mode {
                mode: Some("Challenge Mode".to_string())
            }
            .merge_key()
        );
    }

    fn sample_event(loot_value_items: Vec<LootItem>) -> RaidCompletionEvent {
        RaidCompletionEvent {
            raid_type: RaidType::Toa,
            difficulty: RaidDifficulty::Level { level: 300 },
            world_x: 0,
            world_y: 0,
            plane: 0,
            world: 301,
            occurred_at: None,
            loot: loot_value_items,
        }
    }

    #[test]
    fn append_sums_participants_and_ignores_duplicate_reporter() {
        let mut ge_prices = GEPrices::new();
        ge_prices.insert(1, 100);
        ge_prices.insert(2, 50);

        let event_a = sample_event(vec![LootItem {
            item_id: 1,
            quantity: 2,
        }]);
        let mut payload = RaidCompletionPayload::first("Alice", &event_a, &ge_prices);
        assert_eq!(payload.total_value, 200);

        let event_b = sample_event(vec![LootItem {
            item_id: 2,
            quantity: 1,
        }]);
        payload.append("Bob", &event_b, &ge_prices);
        assert_eq!(payload.participants.len(), 2);
        assert_eq!(payload.total_value, 250);

        // A duplicate report from an existing participant must not double-count.
        payload.append("Bob", &event_b, &ge_prices);
        assert_eq!(payload.participants.len(), 2);
        assert_eq!(payload.total_value, 250);
    }

    #[test]
    fn to_message_pluralizes_gp_suffix_for_groups() {
        let ge_prices = GEPrices::new();
        let event = sample_event(vec![]);
        let mut payload = RaidCompletionPayload::first("Alice", &event, &ge_prices);
        assert!(payload.to_message().ends_with("worth 0 gp"));

        payload.append("Bob", &event, &ge_prices);
        assert!(payload.to_message().ends_with("worth 0 gp total"));
        assert!(payload.to_message().starts_with("Alice and Bob completed"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_hp_alert_requires_interaction_and_names_the_member() {
        let alert = AlertEvent::LowHp(LowHpAlert {
            current_hp: 14,
            max_hp: 99,
            world_x: 0,
            world_y: 0,
            plane: 0,
            world: 0,
        });
        assert_eq!(alert.alert_type(), "low_hp");
        assert!(alert.requires_interaction());
        let (title, body) = alert.push_title_and_body("Woox");
        assert_eq!(title, "Low HP — Woox");
        assert_eq!(body, "14/99 HP remaining");
    }

    #[test]
    fn wilderness_entry_alert_auto_dismisses_and_names_the_member() {
        let alert = AlertEvent::WildernessEntry(WildernessEntryAlert {
            wilderness_level: 27,
            world_x: 0,
            world_y: 0,
            plane: 0,
            world: 0,
        });
        assert_eq!(alert.alert_type(), "wilderness_entry");
        assert!(!alert.requires_interaction());
        let (title, body) = alert.push_title_and_body("Woox");
        assert_eq!(title, "Wilderness entry — Woox");
        assert_eq!(body, "Entered level 27 wilderness");
    }
}
