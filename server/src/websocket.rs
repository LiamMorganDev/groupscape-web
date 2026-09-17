use crate::auth_middleware::Authenticated;
use crate::db;
use crate::error::ApiError;
use crate::models::{normalize_interacting, GroupMember, SHARED_MEMBER};
use actix_web::{rt, web, Error, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use deadpool_postgres::Pool;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;
use tokio::sync::broadcast;

const CHANNEL_CAPACITY: usize = 256;

/// Stable per-member display colors assigned by join order, mirroring
/// groupscape-old's JOIN_ORDER_PALETTE.
const JOIN_ORDER_PALETTE: [&str; 8] = [
    "#5B8DEF", "#E4572E", "#4CAF50", "#E8C547", "#9B59B6", "#1ABC9C", "#E67E22", "#EC407A",
];

pub fn member_color(join_order_index: usize) -> String {
    JOIN_ORDER_PALETTE[join_order_index % JOIN_ORDER_PALETTE.len()].to_string()
}

/// Per-group fan-out registry for the party overlay WebSocket. Each connected
/// overlay subscribes to its group's broadcast channel; `update-group-member`
/// publishes a lightweight vitals-only message into it, bypassing the
/// batched DB writer so overlay updates stay low latency.
pub struct GroupBroadcastRegistry {
    inner: RwLock<HashMap<i64, broadcast::Sender<String>>>,
}

impl GroupBroadcastRegistry {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    pub fn subscribe(&self, group_id: i64) -> broadcast::Receiver<String> {
        let mut inner = self.inner.write().unwrap();
        let sender = inner
            .entry(group_id)
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0);
        sender.subscribe()
    }

    pub fn has_subscribers(&self, group_id: i64) -> bool {
        let inner = self.inner.read().unwrap();
        inner
            .get(&group_id)
            .map(|sender| sender.receiver_count() > 0)
            .unwrap_or(false)
    }

    pub fn publish(&self, group_id: i64, message: String) {
        let inner = self.inner.read().unwrap();
        if let Some(sender) = inner.get(&group_id) {
            let _ = sender.send(message);
        }
    }
}

impl Default for GroupBroadcastRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Longest a ping can go without a fresh Start/Update before `PingRegistry::list_active` prunes
/// it - a safety net for the web poll path in case a client crashes/disconnects without ever
/// sending `PingAction::End` (the plugin's own 60s client-side timeout is the primary expiry;
/// this is deliberately looser so it never prunes a still-live ping out from under a slightly
/// slow heartbeat).
const PING_TTL: std::time::Duration = std::time::Duration::from_secs(70);

/// One group member's active ping, as tracked in-memory for the web map's poll endpoint
/// (`get_active_pings`). The RuneLite-facing path doesn't need this snapshot at all - it just
/// forwards `PingStart`/`PingUpdate`/`PingEnd` frames straight through `GroupBroadcastRegistry` as
/// they arrive - but the web map itself still polls rather than reading its own `/ws` connection
/// (the webapp's only websocket consumer today is the chat drawer - see `authed::submit_ping`'s
/// doc comment), which needs something to poll *from*.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ActivePing {
    pub ping_id: String,
    pub member_name: String,
    pub kind: PingKind,
    pub x: i32,
    pub y: i32,
    pub plane: i32,
    pub npc_name: Option<String>,
    #[serde(skip)]
    pub last_seen: std::time::Instant,
}

/// Per-group active-ping table backing the web map's poll endpoint. Never persisted to the DB -
/// same ephemeral treatment as `GroupBroadcastRegistry`'s in-memory channels.
pub struct PingRegistry {
    inner: RwLock<HashMap<i64, HashMap<String, ActivePing>>>,
}

impl PingRegistry {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    pub fn start(&self, group_id: i64, ping: ActivePing) {
        let mut inner = self.inner.write().unwrap();
        inner
            .entry(group_id)
            .or_insert_with(HashMap::new)
            .insert(ping.ping_id.clone(), ping);
    }

    pub fn update(&self, group_id: i64, ping_id: &str, x: i32, y: i32, plane: i32) {
        let mut inner = self.inner.write().unwrap();
        if let Some(group) = inner.get_mut(&group_id) {
            if let Some(ping) = group.get_mut(ping_id) {
                ping.x = x;
                ping.y = y;
                ping.plane = plane;
                ping.last_seen = std::time::Instant::now();
            }
        }
    }

    pub fn end(&self, group_id: i64, ping_id: &str) {
        let mut inner = self.inner.write().unwrap();
        if let Some(group) = inner.get_mut(&group_id) {
            group.remove(ping_id);
        }
    }

    /// Prunes anything past `PING_TTL` for this group, then returns what's left.
    pub fn list_active(&self, group_id: i64) -> Vec<ActivePing> {
        let mut inner = self.inner.write().unwrap();
        let Some(group) = inner.get_mut(&group_id) else {
            return Vec::new();
        };
        group.retain(|_, ping| ping.last_seen.elapsed() < PING_TTL);
        group.values().cloned().collect()
    }
}

impl Default for PingRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// The fixed set of raid-callout marker types - see `RaidMarkerStartPayload::marker_type`.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MarkerType {
    Danger,
    Defend,
    Loot,
    Focus,
    One,
    Two,
    Three,
    Four,
    A,
    B,
    C,
    D,
}

/// One group member's active raid marker, as tracked in-memory for the web map's poll endpoint
/// (`get_active_raid_markers`) - see `ActivePing` for the equivalent plain-ping version this
/// mirrors. Unlike a ping, a marker never auto-expires, so `RaidMarkerRegistry::list_active` never
/// prunes on `last_seen` - it's kept only for debugging/staleness visibility.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ActiveRaidMarker {
    pub marker_id: String,
    pub member_name: String,
    pub marker_type: MarkerType,
    pub kind: PingKind,
    pub x: i32,
    pub y: i32,
    pub plane: i32,
    pub npc_name: Option<String>,
    #[serde(skip)]
    pub last_seen: std::time::Instant,
}

/// Per-group active-raid-marker table backing the web map's poll endpoint - see `PingRegistry` for
/// the equivalent plain-ping version this mirrors. `list_active` deliberately has no TTL-based
/// pruning: a marker persists until an explicit `RaidMarkerAction::End` (manual clear/redrop, NPC
/// despawn, or the owner's logout/hop/plugin-shutdown clear-all), never on a timer.
pub struct RaidMarkerRegistry {
    inner: RwLock<HashMap<i64, HashMap<String, ActiveRaidMarker>>>,
}

impl RaidMarkerRegistry {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    pub fn start(&self, group_id: i64, marker: ActiveRaidMarker) {
        let mut inner = self.inner.write().unwrap();
        inner
            .entry(group_id)
            .or_insert_with(HashMap::new)
            .insert(marker.marker_id.clone(), marker);
    }

    pub fn update(&self, group_id: i64, marker_id: &str, x: i32, y: i32, plane: i32) {
        let mut inner = self.inner.write().unwrap();
        if let Some(group) = inner.get_mut(&group_id) {
            if let Some(marker) = group.get_mut(marker_id) {
                marker.x = x;
                marker.y = y;
                marker.plane = plane;
                marker.last_seen = std::time::Instant::now();
            }
        }
    }

    pub fn end(&self, group_id: i64, marker_id: &str) {
        let mut inner = self.inner.write().unwrap();
        if let Some(group) = inner.get_mut(&group_id) {
            group.remove(marker_id);
        }
    }

    pub fn list_active(&self, group_id: i64) -> Vec<ActiveRaidMarker> {
        let inner = self.inner.read().unwrap();
        inner
            .get(&group_id)
            .map(|group| group.values().cloned().collect())
            .unwrap_or_default()
    }
}

impl Default for RaidMarkerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WireVitals {
    pub hp: Option<i32>,
    pub max_hp: Option<i32>,
    pub prayer: Option<i32>,
    pub max_prayer: Option<i32>,
    pub run_energy: Option<i32>,
    pub spec_energy: Option<i32>,
    pub world: Option<i32>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub target_name: Option<String>,
    pub target_health_ratio: Option<i32>,
    pub target_health_scale: Option<i32>,
    pub active_prayers: Vec<String>,
    pub rich_presence: Option<String>,
    /// `[x, y, plane]` (sometimes `[x, y, plane, is_on_boat, world]` - see `authed.rs`'s
    /// `record_location_sample`), truncated to 3 once persisted to the `members` row but carried
    /// through in full on the broadcast merge. Powers the in-game world map/minimap markers.
    pub coordinates: Option<Vec<i32>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterMemberEntry {
    pub name: String,
    pub color: String,
    pub vitals: Option<WireVitals>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterSnapshotPayload {
    pub roster: Vec<RosterMemberEntry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VitalsUpdatePayload {
    pub name: String,
    pub vitals: WireVitals,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KillEventPayload {
    pub member_name: String,
    pub npc_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DropEventPayload {
    pub member_name: String,
    /// Pre-built by `NotableDropEvent::to_message` - the plugin relays this verbatim.
    pub message: String,
}

/// Fires once per raid completion, after `raid_merge`'s 5-minute window closes - not per
/// reporting member, unlike `KillEventPayload`. `message` is pre-built by
/// `RaidCompletionPayload::to_message` and already names every reporting member.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RaidEventPayload {
    pub message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorUpdatePayload {
    pub name: String,
    pub color: String,
}

/// Color is deliberately not carried here - every surface already tracks member->color via
/// `RosterSnapshotPayload`/`ColorUpdatePayload`, so renderers key into that existing state by
/// `member_name` instead of denormalizing a copy that can go stale.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessagePayload {
    pub message_id: i64,
    pub member_name: Option<String>,
    pub text: String,
}

/// Broadcast group-wide like every other `WsEnvelope` variant (no per-session targeting exists);
/// `member_name` lets each connected session tell whether the limited sender was itself. `None`
/// for a sender with no resolved character, matching `ChatMessagePayload::member_name`.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChatRateLimitedPayload {
    pub member_name: Option<String>,
}

/// Fires when an account's read cursor advances (`POST /mark-chat-read`) - see the "Chat history
/// and backfill behavior" spec ticket's read-cursor section (§6). Broadcast group-wide, same
/// no-per-session-targeting constraint as `ChatRateLimitedPayload`: `member_name` lets each of the
/// *reading* account's other connected sessions (plugin + other browser tabs) recognize themselves
/// and clear their own unread dot/badge, while every other member's session just ignores a frame
/// that isn't about them. `message_id` is the cursor's new value (see
/// `db::advance_chat_read_cursor` for why it can be higher than what the caller requested).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChatReadPayload {
    pub member_name: Option<String>,
    pub message_id: i64,
}

/// Fires when a group admin deletes a message (`DELETE /delete-chat-message/{message_id}`) - lets
/// every other connected session (plugin + other browser tabs, any member's) drop the message
/// from its own in-memory history live, without re-fetching. Broadcast group-wide like every
/// other `WsEnvelope` variant.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageDeletedPayload {
    pub message_id: i64,
}

/// Chat flood guard - see the "Minimum flood-protection guard" spec ticket. Keyed on `account_id`
/// (not websocket connection) so it survives reconnects, per spec. Fixed window, same shape as
/// `AdminLoginRateLimiter`: a window resets once it's been open longer than `CHAT_RATE_LIMIT_WINDOW`
/// rather than sliding, which is a fine approximation at this cap (order-of-magnitude anchor per
/// spec, not an exact algorithm requirement).
const CHAT_RATE_LIMIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);
const CHAT_RATE_LIMIT_MAX_MESSAGES: u32 = 10;
const CHAT_RATE_LIMIT_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

struct ChatRateLimitWindow {
    count: u32,
    window_start: std::time::Instant,
}

struct ChatRateLimiterInner {
    windows: HashMap<i64, ChatRateLimitWindow>,
    last_sweep: std::time::Instant,
}

pub struct ChatRateLimiter {
    inner: RwLock<ChatRateLimiterInner>,
}

impl ChatRateLimiter {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(ChatRateLimiterInner {
                windows: HashMap::new(),
                last_sweep: std::time::Instant::now(),
            }),
        }
    }

    fn sweep_if_due(inner: &mut ChatRateLimiterInner, now: std::time::Instant) {
        if now.duration_since(inner.last_sweep) < CHAT_RATE_LIMIT_SWEEP_INTERVAL {
            return;
        }
        inner
            .windows
            .retain(|_, window| now.duration_since(window.window_start) < CHAT_RATE_LIMIT_WINDOW);
        inner.last_sweep = now;
    }

    /// Records a send attempt for `account_id` and reports whether it's over the cap. Over-cap
    /// attempts still count against the window (so a caller hammering past the limit doesn't get a
    /// fresh allowance), but the caller is expected to drop the message rather than store/broadcast
    /// it - see `authed::send_chat_message`.
    pub fn check_and_record(&self, account_id: i64) -> bool {
        let Ok(mut inner) = self.inner.write() else {
            return true;
        };
        let now = std::time::Instant::now();
        Self::sweep_if_due(&mut inner, now);

        let window = inner.windows.entry(account_id).or_insert(ChatRateLimitWindow {
            count: 0,
            window_start: now,
        });

        if now.duration_since(window.window_start) >= CHAT_RATE_LIMIT_WINDOW {
            window.count = 0;
            window.window_start = now;
        }

        window.count += 1;
        window.count <= CHAT_RATE_LIMIT_MAX_MESSAGES
    }
}

impl Default for ChatRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// What a ping was dropped on - see `PingStartPayload::npc_name`.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PingKind {
    Tile,
    Npc,
}

/// A ping's lifecycle rides its own dedicated envelope variants (Start/Update/End) rather than
/// piggybacking on `WireVitals` - unlike vitals, a ping is a discrete event stream, not continuous
/// per-tick state. `pingId` lets receivers match `PingUpdate`/`PingEnd` frames back to the
/// `PingStart` they belong to (one player can only have one active ping - see
/// `authed::submit_ping` - but a fresh ping's id still needs to be distinguishable from a stale one
/// still in flight over the wire).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PingStartPayload {
    pub ping_id: String,
    pub member_name: String,
    pub kind: PingKind,
    pub x: i32,
    pub y: i32,
    pub plane: i32,
    /// Set only for `PingKind::Npc` - the tracked NPC's name, for the marker tooltip/chat line.
    pub npc_name: Option<String>,
}

/// Live-tracking re-broadcast of an NPC ping's current tile - only the pinging player's own
/// client observes the NPC and resends its position; other clients never resolve the NPC locally.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PingUpdatePayload {
    pub ping_id: String,
    pub x: i32,
    pub y: i32,
    pub plane: i32,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PingEndPayload {
    pub ping_id: String,
}

/// A raid marker's lifecycle rides its own dedicated envelope variants, same as a plain ping - see
/// `PingStartPayload`. Unlike a ping, a player can have up to 4 active markers per kind at once (one
/// per `MarkerType`) rather than just one, so `markerId` alone (not "the sender's one active ping")
/// distinguishes which of a player's markers a later `MarkerUpdate`/`MarkerEnd` applies to.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RaidMarkerStartPayload {
    pub marker_id: String,
    pub member_name: String,
    pub marker_type: MarkerType,
    pub kind: PingKind,
    pub x: i32,
    pub y: i32,
    pub plane: i32,
    pub npc_name: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RaidMarkerUpdatePayload {
    pub marker_id: String,
    pub x: i32,
    pub y: i32,
    pub plane: i32,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RaidMarkerEndPayload {
    pub marker_id: String,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsEnvelope {
    RosterSnapshot {
        payload: RosterSnapshotPayload,
        ts: DateTime<Utc>,
    },
    VitalsUpdate {
        payload: VitalsUpdatePayload,
        ts: DateTime<Utc>,
    },
    KillEvent {
        payload: KillEventPayload,
        ts: DateTime<Utc>,
    },
    DropEvent {
        payload: DropEventPayload,
        ts: DateTime<Utc>,
    },
    RaidEvent {
        payload: RaidEventPayload,
        ts: DateTime<Utc>,
    },
    ColorUpdate {
        payload: ColorUpdatePayload,
        ts: DateTime<Utc>,
    },
    PingStart {
        payload: PingStartPayload,
        ts: DateTime<Utc>,
    },
    PingUpdate {
        payload: PingUpdatePayload,
        ts: DateTime<Utc>,
    },
    PingEnd {
        payload: PingEndPayload,
        ts: DateTime<Utc>,
    },
    MarkerStart {
        payload: RaidMarkerStartPayload,
        ts: DateTime<Utc>,
    },
    MarkerUpdate {
        payload: RaidMarkerUpdatePayload,
        ts: DateTime<Utc>,
    },
    MarkerEnd {
        payload: RaidMarkerEndPayload,
        ts: DateTime<Utc>,
    },
    ChatMessage {
        payload: ChatMessagePayload,
        ts: DateTime<Utc>,
    },
    ChatRateLimited {
        payload: ChatRateLimitedPayload,
        ts: DateTime<Utc>,
    },
    ChatRead {
        payload: ChatReadPayload,
        ts: DateTime<Utc>,
    },
    ChatMessageDeleted {
        payload: ChatMessageDeletedPayload,
        ts: DateTime<Utc>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_event_serializes_with_snake_case_type_and_camel_case_payload() {
        let envelope = WsEnvelope::KillEvent {
            payload: KillEventPayload {
                member_name: "Zezima".to_string(),
                npc_name: "Zulrah".to_string(),
            },
            ts: DateTime::<Utc>::MIN_UTC,
        };

        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["type"], "kill_event");
        assert_eq!(json["payload"]["memberName"], "Zezima");
        assert_eq!(json["payload"]["npcName"], "Zulrah");
    }

    #[test]
    fn marker_start_serializes_with_snake_case_type_and_marker_type() {
        let envelope = WsEnvelope::MarkerStart {
            payload: RaidMarkerStartPayload {
                marker_id: "abc".to_string(),
                member_name: "Zezima".to_string(),
                marker_type: MarkerType::Defend,
                kind: PingKind::Tile,
                x: 3200,
                y: 3200,
                plane: 0,
                npc_name: None,
            },
            ts: DateTime::<Utc>::MIN_UTC,
        };

        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["type"], "marker_start");
        assert_eq!(json["payload"]["markerType"], "defend");
        assert_eq!(json["payload"]["kind"], "tile");
    }

    #[test]
    fn chat_message_serializes_with_snake_case_type_and_camel_case_payload() {
        let envelope = WsEnvelope::ChatMessage {
            payload: ChatMessagePayload {
                message_id: 42,
                member_name: Some("Zezima".to_string()),
                text: "gz on the pet".to_string(),
            },
            ts: DateTime::<Utc>::MIN_UTC,
        };

        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["type"], "chat_message");
        assert_eq!(json["payload"]["messageId"], 42);
        assert_eq!(json["payload"]["memberName"], "Zezima");
        assert_eq!(json["payload"]["text"], "gz on the pet");
    }

    #[test]
    fn chat_message_serializes_null_member_name_when_absent() {
        let envelope = WsEnvelope::ChatMessage {
            payload: ChatMessagePayload {
                message_id: 43,
                member_name: None,
                text: "hello".to_string(),
            },
            ts: DateTime::<Utc>::MIN_UTC,
        };

        let json = serde_json::to_value(&envelope).unwrap();
        assert!(json["payload"]["memberName"].is_null());
    }

    #[test]
    fn chat_rate_limited_serializes_with_snake_case_type() {
        let envelope = WsEnvelope::ChatRateLimited {
            payload: ChatRateLimitedPayload {
                member_name: Some("Zezima".to_string()),
            },
            ts: DateTime::<Utc>::MIN_UTC,
        };

        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["type"], "chat_rate_limited");
        assert_eq!(json["payload"]["memberName"], "Zezima");
    }

    #[test]
    fn chat_read_serializes_with_snake_case_type_and_camel_case_payload() {
        let envelope = WsEnvelope::ChatRead {
            payload: ChatReadPayload {
                member_name: Some("Zezima".to_string()),
                message_id: 42,
            },
            ts: DateTime::<Utc>::MIN_UTC,
        };

        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["type"], "chat_read");
        assert_eq!(json["payload"]["memberName"], "Zezima");
        assert_eq!(json["payload"]["messageId"], 42);
    }

    #[test]
    fn chat_rate_limiter_allows_up_to_the_cap_then_blocks() {
        let limiter = ChatRateLimiter::new();
        for _ in 0..CHAT_RATE_LIMIT_MAX_MESSAGES {
            assert!(limiter.check_and_record(1));
        }
        assert!(!limiter.check_and_record(1));
    }

    #[test]
    fn chat_rate_limiter_tracks_accounts_independently() {
        let limiter = ChatRateLimiter::new();
        for _ in 0..CHAT_RATE_LIMIT_MAX_MESSAGES {
            assert!(limiter.check_and_record(1));
        }
        assert!(!limiter.check_and_record(1));
        assert!(limiter.check_and_record(2));
    }

    #[test]
    fn raid_marker_registry_never_prunes_on_ttl() {
        let registry = RaidMarkerRegistry::new();
        registry.start(
            1,
            ActiveRaidMarker {
                marker_id: "abc".to_string(),
                member_name: "Zezima".to_string(),
                marker_type: MarkerType::Danger,
                kind: PingKind::Npc,
                x: 3200,
                y: 3200,
                plane: 0,
                npc_name: Some("Vorkath".to_string()),
                last_seen: std::time::Instant::now()
                    .checked_sub(std::time::Duration::from_secs(60 * 60))
                    .unwrap(),
            },
        );

        assert_eq!(registry.list_active(1).len(), 1);
    }
}

/// Maps the persisted/POSTed GroupMember shape onto the overlay's wire vitals.
pub fn to_wire_vitals(member: &GroupMember) -> WireVitals {
    let (hp, max_hp, prayer, max_prayer, run_energy, world) = match &member.stats {
        Some(stats) if stats.len() >= 7 => (
            Some(stats[0]),
            Some(stats[1]),
            Some(stats[2]),
            Some(stats[3]),
            Some(stats[4]),
            Some(stats[6]),
        ),
        _ => (None, None, None, None, None, None),
    };

    let (target_name, target_health_ratio, target_health_scale) =
        match normalize_interacting(member.interacting.clone()) {
            Some(interacting) => (
                Some(interacting.name.clone()),
                Some(interacting.ratio),
                Some(interacting.scale),
            ),
            None => (None, None, None),
        };

    WireVitals {
        hp,
        max_hp,
        prayer,
        max_prayer,
        run_energy,
        spec_energy: member.special_attack,
        world,
        last_heartbeat_at: member.last_updated,
        target_name,
        target_health_ratio,
        target_health_scale,
        active_prayers: member.active_prayers.clone().unwrap_or_default(),
        rich_presence: member.rich_presence.clone(),
        coordinates: member.coordinates.clone(),
    }
}

/// Real-time push feed shared by the RuneLite party overlay (`/api/characters/{account_hash}/ws`,
/// character-key auth) and the webapp (`/api/group/{group_name}/ws`, group-token auth - see
/// `auth_middleware.rs`'s query-param fallback for how a browser `WebSocket`, which can't set an
/// `Authorization` header, authenticates here). Both paths resolve to the same
/// `Authenticated{group_id}` this handler actually reads, so one implementation serves both. On
/// connect, sends one `roster_snapshot` built from the current DB state, then forwards this
/// group's broadcast channel - `vitals_update`, `chat_message`, and everything else in
/// `WsEnvelope` - until the socket closes.
pub async fn party_overlay_ws(
    req: HttpRequest,
    stream: web::Payload,
    auth: Authenticated,
    db_pool: web::Data<Pool>,
    registry: web::Data<GroupBroadcastRegistry>,
) -> Result<HttpResponse, Error> {
    let group_id = auth.group_id;
    let (response, mut session, mut msg_stream) = actix_ws::handle(&req, stream)?;

    let client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let colors = db::get_member_color_map(&client, group_id).await?;
    let members = db::get_group_data(
        &client,
        group_id,
        &DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
    )
    .await?;
    drop(client);

    let roster: Vec<RosterMemberEntry> = members
        .iter()
        .filter(|member| member.name != SHARED_MEMBER)
        .map(|member| RosterMemberEntry {
            name: member.name.clone(),
            // Prefer the admin-assigned helmet colour (`members.color`) over the join-order
            // fallback palette, so the overlay stripe matches the site's colour picker. Falls
            // back to join order for members nobody has assigned a colour to yet.
            color: member
                .color
                .as_deref()
                .and_then(crate::models::named_color_to_hex)
                .or_else(|| colors.get(&member.name).cloned())
                .unwrap_or_else(|| "#808080".to_string()),
            vitals: Some(to_wire_vitals(member)),
        })
        .collect();

    let snapshot = WsEnvelope::RosterSnapshot {
        payload: RosterSnapshotPayload { roster },
        ts: Utc::now(),
    };
    let snapshot_json = serde_json::to_string(&snapshot).map_err(ApiError::SerdeJsonError)?;

    let mut receiver = registry.subscribe(group_id);
    let account_hash = auth.account_hash.clone().unwrap_or_default();
    log::info!(
        "Party overlay WebSocket connected (group_id={}, account_hash={})",
        group_id,
        account_hash
    );

    rt::spawn(async move {
        if session.text(snapshot_json).await.is_err() {
            log::info!(
                "Party overlay WebSocket closed: failed to send initial snapshot (group_id={}, account_hash={})",
                group_id, account_hash
            );
            return;
        }

        let close_reason = loop {
            tokio::select! {
                incoming = msg_stream.next() => {
                    match incoming {
                        Some(Ok(actix_ws::Message::Ping(bytes))) => {
                            if session.pong(&bytes).await.is_err() {
                                break "failed to send pong";
                            }
                        }
                        Some(Ok(actix_ws::Message::Close(_))) => break "client sent close",
                        None => break "client stream ended",
                        Some(Err(_)) => break "client stream error",
                        _ => {}
                    }
                }
                update = receiver.recv() => {
                    match update {
                        Ok(message) => {
                            if session.text(message).await.is_err() {
                                break "failed to send update";
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            log::warn!(
                                "Party overlay WebSocket receiver lagged, dropped {} messages (group_id={}, account_hash={})",
                                skipped, group_id, account_hash
                            );
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => break "broadcast channel closed",
                    }
                }
            }
        };
        log::info!(
            "Party overlay WebSocket disconnected: {} (group_id={}, account_hash={})",
            close_reason, group_id, account_hash
        );

        let _ = session.close(None).await;
    });

    Ok(response)
}
