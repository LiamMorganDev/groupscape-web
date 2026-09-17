use crate::auth_middleware::Authenticated;
use crate::cache::RedisCache;
use crate::character_auth_middleware::CharacterIdentified;
use crate::config::Config;
use crate::crypto::session_token_hash;
use crate::db;
use crate::discord;
use crate::drop_rates;
use crate::error::ApiError;
use crate::hiscores;
use crate::item_bonuses;
use crate::leaderboard::{LeaderboardMetric, LeaderboardResult, LeaderboardWindow};
use crate::loot_log_search::{
    combat_level, extract_date_clauses, extract_kill_count_clause, matches_category_keyword,
    matches_rarity_keyword, numeric_clause_matches, numeric_clause_matches_f64, parse_drop_rate_clause,
    parse_numeric_clause, rarity_keyword, split_search_groups, DateBounds,
};
use crate::models::{
    ActivityCommentsPage, ActivityEvent, ActivityReactionsSummary, ActivitySettings,
    AddActivityCommentRequest, AmIInGroupRequest, BlockedMember, ChatMessage,
    DiscordWebhookSettings, GameEvent, GroupCredentials, GroupMember, GroupMemberName,
    GroupMemberPermissions, GroupMetricData, GroupSession, GroupSkillData, IdentifyCharacter,
    ItemBonusesResponse, LootItem, LootLogEvent, LootLogItem, LootLogPage, LootLogSummary,
    MyPermissions, PermissionFlags, PermissionKey, ReactToActivityEventRequest, RenameGroup,
    SendChatMessageRequest, SlayerTaskHistoryPage, SlayerTaskStats, TestDiscordNotificationRequest,
    UpdateGroupPermissionsRequest, UpdateMemberColorRequest, ACTIVITY_COMMENT_MAX_LEN,
    ACTIVITY_REACTION_KINDS, CHAT_MESSAGE_MAX_LEN, SHARED_MEMBER,
};
use crate::notable_npcs;
use crate::permissions::{
    require_account, require_any_group_permission, require_group_permission, ACCOUNT_AUTH_HEADER,
};
use crate::progress_events;
use crate::push;
use crate::raid_merge;
use crate::update_batcher;
use crate::unauthed::get_ge_prices_map;
use crate::validators::{valid_name, validate_member_prop_length, ArrayFormat};
use crate::websocket::{
    self, ActivePing, ActiveRaidMarker, ChatMessagePayload, DropEventPayload,
    GroupBroadcastRegistry, KillEventPayload, MarkerType, PingEndPayload, PingKind, PingRegistry,
    PingStartPayload, PingUpdatePayload, RaidMarkerEndPayload, RaidMarkerRegistry,
    RaidMarkerStartPayload, RaidMarkerUpdatePayload, VitalsUpdatePayload, WsEnvelope,
};
use actix_web::{delete, get, post, put, web, Error, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use deadpool_postgres::{Client, Pool};
use serde::Deserialize;
use std::collections::HashMap;
use tokio::sync::mpsc;

/// TTL for the short-lived Redis response cache on the leaderboard/metric-data/sessions/loot-log-
/// summary endpoints (see `crate::cache::RedisCache`). All four read data that's written on a hot
/// per-tick path (or a 30-min aggregator job), so a short TTL keeps staleness unnoticeable while
/// avoiding the need to thread explicit cache-invalidation calls through that hot write path.
const CACHE_TTL_SECS: u64 = 15;

/// `get_loot_log` (unlike the other three cached endpoints above) is keyed off an explicit
/// per-group version counter rather than a short TTL: it has far more cache-key variations
/// (pagination cursor x limit x search x categories) for a TTL to keep warm across, so instead
/// each cached page is tagged with the group's current counter value and stays valid until a new
/// kill/loot event bumps it (see the `redis.incr` call beside `insert_activity_event` below) -
/// giving instant invalidation on write instead of up-to-CACHE_TTL_SECS staleness. `LOOT_LOG_CACHE_TTL_SECS`
/// is still applied underneath as a safety net so an orphaned entry (e.g. after a Redis restart
/// resets the counter to 0) can't linger forever.
const LOOT_LOG_CACHE_TTL_SECS: u64 = 3600;
/// Safety-net TTL underneath `get_activity_events`'s version-counter invalidation - see
/// `activity_log_version_key`'s doc comment.
const ACTIVITY_LOG_CACHE_TTL_SECS: u64 = 3600;

pub(crate) fn loot_log_version_key(group_id: i64) -> String {
    format!("v1:loot-log-version:{}", group_id)
}

/// Same version-counter invalidation as `loot_log_version_key`, but for `get_activity_events` -
/// bumped on every successful `insert_activity_event`/`insert_progress_event` write (any event
/// type, unlike the loot log's Kill/Loot-only filter) and, for raid completions specifically, at
/// each of `raid_merge`'s three write points (initial insert, a later reporter merging in, and
/// the merge-window finalize) since all three change what a cached feed page would show for that
/// row.
pub(crate) fn activity_log_version_key(group_id: i64) -> String {
    format!("v1:activity-log-version:{}", group_id)
}

#[delete("/delete-group-member")]
pub async fn delete_group_member(
    req: HttpRequest,
    auth: Authenticated,
    group_member: web::Json<GroupMember>,
    db_pool: web::Data<Pool>,
) -> Result<HttpResponse, Error> {
    crate::demo::reject_if_demo(auth.group_id)?;
    if group_member.name.eq(SHARED_MEMBER) {
        return Ok(
            HttpResponse::BadRequest().body(format!("Member name {} not allowed", SHARED_MEMBER))
        );
    }

    let mut client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let account_id =
        require_group_permission(&req, &client, auth.group_id, PermissionKey::KickMembers).await?;
    if db::resolve_member_name_for_account(&client, auth.group_id, account_id)
        .await?
        .as_deref()
        == Some(group_member.name.as_str())
    {
        return Err(ApiError::CannotRemoveSelfError.into());
    }
    db::delete_group_member(&mut client, auth.group_id, &group_member.name).await?;
    Ok(HttpResponse::Ok().finish())
}

#[post("/block-group-member")]
pub async fn block_group_member(
    req: HttpRequest,
    auth: Authenticated,
    group_member: web::Json<GroupMemberName>,
    db_pool: web::Data<Pool>,
) -> Result<HttpResponse, Error> {
    crate::demo::reject_if_demo(auth.group_id)?;
    if group_member.name.eq(SHARED_MEMBER) {
        return Ok(
            HttpResponse::BadRequest().body(format!("Member name {} not allowed", SHARED_MEMBER))
        );
    }

    let mut client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let account_id =
        require_group_permission(&req, &client, auth.group_id, PermissionKey::KickMembers).await?;
    if db::resolve_member_name_for_account(&client, auth.group_id, account_id)
        .await?
        .as_deref()
        == Some(group_member.name.as_str())
    {
        return Err(ApiError::CannotBlockSelfError.into());
    }
    db::block_group_member(&mut client, auth.group_id, &group_member.name).await?;
    Ok(HttpResponse::Ok().finish())
}

#[post("/unblock-group-member")]
pub async fn unblock_group_member(
    req: HttpRequest,
    auth: Authenticated,
    group_member: web::Json<GroupMemberName>,
    db_pool: web::Data<Pool>,
) -> Result<HttpResponse, Error> {
    crate::demo::reject_if_demo(auth.group_id)?;
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    require_group_permission(&req, &client, auth.group_id, PermissionKey::KickMembers).await?;
    db::unblock_group_member(&client, auth.group_id, &group_member.name).await?;
    Ok(HttpResponse::Ok().finish())
}

/// Self-check for the site's remove/block member controls: a 401/403 here means the acting
/// account can't call `delete-group-member`/`block-group-member`/`unblock-group-member` either,
/// so the site hides those controls rather than showing ones that would just 403 on click.
#[get("/can-kick-members")]
pub async fn can_kick_members(
    req: HttpRequest,
    auth: Authenticated,
    db_pool: web::Data<Pool>,
) -> Result<HttpResponse, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    require_group_permission(&req, &client, auth.group_id, PermissionKey::KickMembers).await?;
    Ok(HttpResponse::Ok().finish())
}

#[get("/get-blocked-members")]
pub async fn get_blocked_members(
    auth: Authenticated,
    db_pool: web::Data<Pool>,
) -> Result<web::Json<Vec<BlockedMember>>, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let blocked_members = db::get_blocked_members(&client, auth.group_id).await?;
    Ok(web::Json(blocked_members))
}

#[put("/rename-group")]
pub async fn rename_group(
    req: HttpRequest,
    auth: Authenticated,
    rename_group: web::Json<RenameGroup>,
    db_pool: web::Data<Pool>,
) -> Result<HttpResponse, Error> {
    crate::demo::reject_if_demo(auth.group_id)?;
    let new_name = rename_group.new_name.trim().to_string();
    if !valid_name(&new_name) {
        return Ok(HttpResponse::BadRequest().body("Provided group name is not valid"));
    }

    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    require_group_permission(&req, &client, auth.group_id, PermissionKey::ManageSettings).await?;
    let new_token = db::rename_group(&client, auth.group_id, &new_name).await?;
    Ok(HttpResponse::Ok().json(&GroupCredentials {
        name: new_name,
        token: new_token,
    }))
}

#[post("/reroll-group-token")]
pub async fn reroll_group_token(
    req: HttpRequest,
    auth: Authenticated,
    path: web::Path<String>,
    db_pool: web::Data<Pool>,
) -> Result<HttpResponse, Error> {
    crate::demo::reject_if_demo(auth.group_id)?;
    let group_name = path.into_inner();
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    require_group_permission(
        &req,
        &client,
        auth.group_id,
        PermissionKey::RegenerateGroupKey,
    )
    .await?;
    let new_token = db::reroll_group_token(&client, auth.group_id, &group_name).await?;
    Ok(HttpResponse::Ok().json(&GroupCredentials {
        name: group_name,
        token: new_token,
    }))
}

#[delete("/delete-group")]
pub async fn delete_group(
    req: HttpRequest,
    auth: Authenticated,
    db_pool: web::Data<Pool>,
) -> Result<HttpResponse, Error> {
    crate::demo::reject_if_demo(auth.group_id)?;
    let mut client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    require_group_permission(&req, &client, auth.group_id, PermissionKey::ManageSettings).await?;
    db::admin_delete_group(&mut client, auth.group_id).await?;
    Ok(HttpResponse::Ok().finish())
}

/// Doubles as the client-side admin gate for the member-roster UI: accounts holding
/// `ManagePermissions` (permission toggles) or `ManageSettings` (helmet-colour assignment) can
/// call this - either implies enough to see *some* control in this section, so the site treats a
/// 401/403 here as "hide the section" rather than needing a separate am-I-admin check. Which
/// controls a given caller actually gets to edit is decided client-side from `/get-my-permissions`.
#[get("/get-group-permissions")]
pub async fn get_group_permissions(
    req: HttpRequest,
    auth: Authenticated,
    db_pool: web::Data<Pool>,
) -> Result<web::Json<Vec<GroupMemberPermissions>>, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    require_any_group_permission(
        &req,
        &client,
        auth.group_id,
        &[PermissionKey::ManagePermissions, PermissionKey::ManageSettings],
    )
    .await?;
    let permissions = db::list_group_member_permissions(&client, auth.group_id).await?;
    Ok(web::Json(permissions))
}

/// The caller's own effective flags (admin override folded in) plus their own member name in
/// this group, keyed off whatever account `X-Account-Authorization` resolves to - unlike
/// `get_group_permissions` this isn't gated by `ManagePermissions`, since every member needs to
/// know their own capabilities to decide which controls to show (e.g. the remove-member button,
/// and `member_name` lets the client hide that button on its own row - the server rejects
/// self-removal too, see `delete_group_member`/`block_group_member`). No/invalid account header
/// or no permissions row all fall back to [`PermissionFlags::default`] (every flag off) and a
/// `None` member name, rather than erroring, so a logged-out browser just sees nothing gated.
#[get("/get-my-permissions")]
pub async fn get_my_permissions(
    req: HttpRequest,
    auth: Authenticated,
    db_pool: web::Data<Pool>,
) -> Result<web::Json<MyPermissions>, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;

    let account = match req
        .headers()
        .get(ACCOUNT_AUTH_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        Some(token) => {
            db::get_account_by_session_token_hash(&client, &session_token_hash(token)).await?
        }
        None => None,
    };

    let (flags, member_name) = match account {
        Some(account) => {
            let flags =
                db::get_effective_permission_flags(&client, auth.group_id, account.id).await?;
            let member_name =
                db::resolve_member_name_for_account(&client, auth.group_id, account.id).await?;
            (flags, member_name)
        }
        None => (PermissionFlags::default(), None),
    };

    Ok(web::Json(MyPermissions { member_name, flags }))
}

#[put("/update-group-permissions")]
pub async fn update_group_permissions(
    req: HttpRequest,
    auth: Authenticated,
    body: web::Json<UpdateGroupPermissionsRequest>,
    db_pool: web::Data<Pool>,
) -> Result<HttpResponse, Error> {
    crate::demo::reject_if_demo(auth.group_id)?;
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    require_group_permission(
        &req,
        &client,
        auth.group_id,
        PermissionKey::ManagePermissions,
    )
    .await?;
    let body = body.into_inner();
    let updated =
        db::update_group_permissions(&client, auth.group_id, body.account_id, body.patch).await?;
    match updated {
        Some(permissions) => Ok(HttpResponse::Ok().json(permissions)),
        None => Ok(HttpResponse::NotFound().body("Account has no permissions row in this group")),
    }
}

/// Assigns a member's helmet/accent colour. Gated on `ManageSettings` rather than
/// `ManagePermissions` - deliberately a different flag from `update_group_permissions`, so an
/// account with settings access but not permissions access can still restyle the roster.
#[put("/update-member-color")]
pub async fn update_member_color(
    req: HttpRequest,
    auth: Authenticated,
    body: web::Json<UpdateMemberColorRequest>,
    db_pool: web::Data<Pool>,
    broadcast_registry: web::Data<GroupBroadcastRegistry>,
) -> Result<HttpResponse, Error> {
    crate::demo::reject_if_demo(auth.group_id)?;
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    require_group_permission(&req, &client, auth.group_id, PermissionKey::ManageSettings).await?;
    let body = body.into_inner();
    let (member_name, updated) =
        db::update_member_color(&client, auth.group_id, body.account_id, &body.color).await?;

    // Push the new colour straight to any connected party overlays so the plugin's accent
    // stripe updates without waiting for a reconnect/resync of the roster snapshot.
    if let Some(hex) = crate::models::named_color_to_hex(&body.color) {
        let envelope = WsEnvelope::ColorUpdate {
            payload: websocket::ColorUpdatePayload {
                name: member_name,
                color: hex,
            },
            ts: Utc::now(),
        };
        if let Ok(message) = serde_json::to_string(&envelope) {
            broadcast_registry.publish(auth.group_id, message);
        }
    }

    Ok(HttpResponse::Ok().json(updated))
}

/// Gated on `ManageDiscord` (kept separate from `ManageSettings` since the webhook URL is a
/// credential, matching `groupscape-old`'s `discordSettings.ts` decision) - the `GET` doubles as
/// the client-side admin gate for the settings page, same pattern as `get_group_permissions`.
#[get("/get-discord-settings")]
pub async fn get_discord_settings(
    req: HttpRequest,
    auth: Authenticated,
    db_pool: web::Data<Pool>,
) -> Result<web::Json<DiscordWebhookSettings>, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    require_group_permission(&req, &client, auth.group_id, PermissionKey::ManageDiscord).await?;
    let settings = db::get_discord_webhook_settings(&client, auth.group_id).await?;
    Ok(web::Json(settings))
}

/// Always submits full settings state (not a patch) - see [`DiscordWebhookSettings`]. A
/// `webhook_url` that's new or changed from what's currently stored is validated with a live
/// test message before saving, so a typo'd URL never gets silently stored; an empty/`None` URL
/// clears it without a test call. Re-saving an unchanged URL (e.g. toggling a notify checkbox)
/// skips the test call so it doesn't spam the channel on every settings save.
#[put("/update-discord-settings")]
pub async fn update_discord_settings(
    req: HttpRequest,
    auth: Authenticated,
    body: web::Json<DiscordWebhookSettings>,
    db_pool: web::Data<Pool>,
) -> Result<web::Json<DiscordWebhookSettings>, Error> {
    crate::demo::reject_if_demo(auth.group_id)?;
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    require_group_permission(&req, &client, auth.group_id, PermissionKey::ManageDiscord).await?;

    let mut settings = body.into_inner();
    settings.webhook_url = settings
        .webhook_url
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty());

    let existing = db::get_discord_webhook_settings(&client, auth.group_id).await?;
    if let Some(url) = &settings.webhook_url {
        if existing.webhook_url.as_deref() != Some(url.as_str()) {
            discord::test_webhook(url).await?;
        }
    }

    db::update_discord_webhook_settings(&client, auth.group_id, &settings).await?;
    Ok(web::Json(settings))
}

/// Sends one fabricated example embed for whichever notification category the admin clicked
/// "Send test message" on (see [`discord::send_test_notification`]) - lets them see the real
/// format/icon without waiting for it to actually happen in-game. Gated the same as the other
/// Discord settings endpoints; requires a webhook URL already saved (there's nothing to send a
/// test to otherwise, and this never persists a URL of its own the way `update-discord-settings`
/// does).
#[post("/test-discord-notification")]
pub async fn test_discord_notification(
    req: HttpRequest,
    auth: Authenticated,
    body: web::Json<TestDiscordNotificationRequest>,
    db_pool: web::Data<Pool>,
    config: web::Data<Config>,
) -> Result<HttpResponse, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    require_group_permission(&req, &client, auth.group_id, PermissionKey::ManageDiscord).await?;

    let settings = db::get_discord_webhook_settings(&client, auth.group_id).await?;
    let Some(webhook_url) = settings.webhook_url else {
        return Err(ApiError::DiscordWebhookInvalidError(
            "Save a webhook URL first.".to_string(),
        )
        .into());
    };

    discord::send_test_notification(webhook_url, config.web_origin.clone(), &body.kind).await?;
    Ok(HttpResponse::Ok().finish())
}

pub async fn update_group_member(
    auth: Authenticated,
    group_member: web::Json<GroupMember>,
    sender: web::Data<mpsc::Sender<GroupMember>>,
    broadcast_registry: web::Data<GroupBroadcastRegistry>,
    raid_merge_registry: web::Data<raid_merge::RaidMergeRegistry>,
    db_pool: web::Data<Pool>,
    config: web::Data<Config>,
    redis: web::Data<RedisCache>,
) -> Result<HttpResponse, Error> {
    crate::demo::reject_if_demo(auth.group_id)?;
    if group_member.name.eq(SHARED_MEMBER) {
        return Ok(
            HttpResponse::BadRequest().body(format!("Member name {} not allowed", SHARED_MEMBER))
        );
    }

    // No manual "add member" step - the group roster is provisioned lazily from
    // telemetry itself. This also enforces the block list and the 5-member cap on
    // brand-new names before anything gets written or broadcast.
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    db::ensure_group_member(&client, auth.group_id, &group_member.name).await?;

    let mut group_member_inner: GroupMember = group_member.into_inner();
    group_member_inner.group_id = Some(auth.group_id);

    // Derive the canonical member name from the account_hash rather than trusting the
    // client-supplied name, when the character is linked (account created, character linked, and
    // linked to this specific group). Prefer the server-resolved account_hash from the
    // character-key auth middleware (`auth.account_hash`) over the client-supplied JSON field -
    // the plugin's real payloads never populate the latter (it only ever sends account_hash in
    // the URL, not the body), so relying on the JSON field left every member row's account_hash
    // permanently NULL and broke anything keyed off it (e.g. the group panel's portrait lookup).
    // Legacy plugin builds and characters that aren't linked yet fall back to matching an
    // existing member row by name, so group setup can still take a typed RSN in the meantime.
    // Also resolves the account a low-HP/wilderness alert (below) should push to - a character
    // can carry alerts once linked to an account even before it's linked to this specific group.
    let mut linked_account_id: Option<i64> = None;
    if let Some(account_hash) = auth
        .account_hash
        .clone()
        .or_else(|| group_member_inner.account_hash.clone())
    {
        let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
        let character = match auth.character_id {
            Some(character_id) => db::find_character_by_id(&client, character_id).await?,
            // Legacy plugin builds and the client-supplied JSON fallback don't carry a resolved
            // character_id, so fall back to whichever account has this account_hash linked to
            // this specific group (unambiguous even if multiple accounts share the account_hash).
            None => {
                db::find_character_by_account_hash_and_group(&client, &account_hash, auth.group_id)
                    .await?
            }
        };
        if let Some(character) = character {
            linked_account_id = Some(character.account_id);
            let linked_to_this_group = db::find_character_group_link(&client, character.id)
                .await?
                .is_some_and(|link| link.group_id == auth.group_id);
            if linked_to_this_group {
                group_member_inner.name = db::ensure_member_for_linked_character(
                    &client,
                    auth.group_id,
                    &account_hash,
                    &character.display_rsn,
                )
                .await?;
            }
        }
    }

    validate_member_prop_length("stats", &group_member_inner.stats, 7, 7, ArrayFormat::Flat)?;
    validate_member_prop_length(
        "coordinates",
        &group_member_inner.coordinates,
        3,
        5,
        ArrayFormat::Flat,
    )?;

    if let Some(coordinates) = group_member_inner.coordinates.as_deref() {
        if coordinates.len() >= 5 {
            let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
            db::record_location_sample(
                &client,
                auth.group_id,
                &group_member_inner.name,
                Utc::now(),
                coordinates[4],
                coordinates[2],
                coordinates[0],
                coordinates[1],
            )
            .await?;
        }
    }
    validate_member_prop_length(
        "skills",
        &group_member_inner.skills,
        23,
        24,
        ArrayFormat::Flat,
    )?;
    validate_member_prop_length(
        "quests",
        &group_member_inner.quests,
        0,
        250,
        ArrayFormat::Flat,
    )?;
    validate_member_prop_length(
        "inventory",
        &group_member_inner.inventory,
        56,
        56,
        ArrayFormat::ItemPairs,
    )?;
    validate_member_prop_length(
        "equipment",
        &group_member_inner.equipment,
        28,
        28,
        ArrayFormat::ItemPairs,
    )?;
    validate_member_prop_length(
        "bank",
        &group_member_inner.bank,
        0,
        3000,
        ArrayFormat::ItemPairs,
    )?;
    validate_member_prop_length(
        "shared_bank",
        &group_member_inner.shared_bank,
        0,
        1000,
        ArrayFormat::ItemPairs,
    )?;
    validate_member_prop_length(
        "rune_pouch",
        &group_member_inner.rune_pouch,
        6,
        8,
        ArrayFormat::ItemPairs,
    )?;
    validate_member_prop_length(
        "seed_vault",
        &group_member_inner.seed_vault,
        0,
        500,
        ArrayFormat::ItemPairs,
    )?;
    validate_member_prop_length(
        "deposited",
        &group_member_inner.deposited,
        0,
        200,
        ArrayFormat::ItemPairs,
    )?;
    validate_member_prop_length(
        "diary_vars",
        &group_member_inner.diary_vars,
        0,
        62,
        ArrayFormat::Flat,
    )?;
    validate_member_prop_length(
        "collection_log_v2",
        &group_member_inner.collection_log_v2,
        0,
        4000,
        ArrayFormat::Flat,
    )?;
    validate_member_prop_length(
        "collection_log_sync",
        &group_member_inner.collection_log_sync,
        0,
        4000,
        ArrayFormat::Flat,
    )?;
    validate_member_prop_length(
        "potion_storage",
        &group_member_inner.potion_storage,
        0,
        400,
        ArrayFormat::ItemPairs,
    )?;

    // Discrete kill/death events are written straight through (not routed via the batcher,
    // which only knows latest-value-wins upserts) before the vitals fields hand off to it.
    // `.take()` since the batcher's `GroupMember` merge/dedupe logic has no notion of this
    // field and shouldn't carry it along.
    if let Some(events) = group_member_inner.events.take() {
        if !events.is_empty() {
            let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
            let session_id = db::ensure_open_session(&client, auth.group_id).await?;
            for event in &events {
                // Raid completions don't go through the generic insert/relay path below at all -
                // they need to merge with other party members' reports of the same raid before
                // anything is relayed, which `raid_merge::handle_raid_completion` owns end to end
                // (including its own `insert_activity_event`-equivalent write).
                if let GameEvent::Raid(raid) = event {
                    raid_merge::handle_raid_completion(
                        &client,
                        db_pool.clone(),
                        redis.get_ref().clone(),
                        raid_merge_registry.clone(),
                        broadcast_registry.clone(),
                        auth.group_id,
                        session_id,
                        group_member_inner.name.clone(),
                        raid.clone(),
                    )
                    .await?;
                    continue;
                }

                let mut stored_event = event.clone();
                if let GameEvent::Kill(kill) = &mut stored_event {
                    let occurred_at = kill.occurred_at.unwrap_or_else(Utc::now);
                    kill.participants = Some(
                        db::nearby_members_for_kill(
                            &client,
                            auth.group_id,
                            occurred_at,
                            kill.world,
                            kill.plane,
                            kill.world_x,
                            kill.world_y,
                            &group_member_inner.name,
                        )
                        .await?,
                    );
                }
                let inserted = db::insert_activity_event(
                    &client,
                    auth.group_id,
                    session_id,
                    &group_member_inner.name,
                    &stored_event,
                )
                .await?;
                // A rejected duplicate (see insert_activity_event's doc comment - most commonly
                // a resend after a server restart dropped the connection before its response
                // reached the client) already has a Discord message and feed row from the first
                // successful insert; firing either again here would just double them.
                if !inserted {
                    continue;
                }
                // Keeps get_loot_log's/get_activity_events's per-group cached pages from going
                // stale instead of relying on a TTL - see loot_log_version_key's/
                // activity_log_version_key's doc comments.
                if matches!(event, GameEvent::Kill(_) | GameEvent::Loot(_)) {
                    redis.incr(&loot_log_version_key(auth.group_id)).await;
                }
                redis.incr(&activity_log_version_key(auth.group_id)).await;
                discord::dispatch_event_webhook(
                    db_pool.get_ref().clone(),
                    auth.group_id,
                    group_member_inner.name.clone(),
                    event.clone(),
                    config.web_origin.clone(),
                );

                // Kills are pushed straight to the group's connected overlays too, so other
                // members' chat can react live - unlike vitals, this is worth a dedicated
                // envelope rather than a value bag, and death is left off the wire since
                // nothing consumes it yet. Restricted to bosses (matching the Discord kill
                // notification's filter) since the plugin's "Boss kill" chat setting has no
                // other way to tell a boss kill from an arbitrary slayer-task kill.
                if let GameEvent::Kill(kill) = event {
                    if notable_npcs::is_notable(&kill.npc_name) && broadcast_registry.has_subscribers(auth.group_id) {
                        let envelope = WsEnvelope::KillEvent {
                            payload: KillEventPayload {
                                member_name: group_member_inner.name.clone(),
                                npc_name: kill.npc_name.clone(),
                            },
                            ts: Utc::now(),
                        };
                        if let Ok(message) = serde_json::to_string(&envelope) {
                            broadcast_registry.publish(auth.group_id, message);
                        }
                    }
                }
            }
        }
    }

    // NPC dialogue/object-interaction events still ride the heartbeat under their own
    // "interactions"/"object_interactions" keys, but they're no longer feed material - the
    // activity feed is scoped to milestones only. They're still `.take()`n so the batcher's
    // `GroupMember` merge/dedupe logic doesn't have to carry fields it has no notion of.
    group_member_inner.interactions.take();
    group_member_inner.object_interactions.take();

    // Quest/diary/combat-task/collection-log milestones aren't discrete plugin events - they're
    // transitions in the full-snapshot progress columns, so they have to be diffed against the
    // stored row *before* the batcher overwrites it. The batcher can't do this itself: it only
    // does latest-value-wins upserts and never reads the old row.
    //
    // Accepted race: the batcher's write can lag this SELECT by up to its flush interval (~50ms),
    // so two heartbeats landing inside that window could diff against the same stale row and
    // double-fire a milestone. Heartbeat cadence is ~1s+, so this is rare enough not to be worth
    // locking/dedup machinery (same tolerance as the non-idempotent deposit retry noted in
    // `update_batcher.rs`).
    {
        let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
        let previous_snapshot =
            db::get_progress_snapshot(&client, auth.group_id, &group_member_inner.name).await?;

        // Fold the live delta onto the stored baseline before diffing against it, so a diffed
        // collection-log item's `page_owned` count (see `diff_collection_log`) reflects the
        // player's full page state, not just what this heartbeat's delta happens to mention.
        // `_sync` bulk-backfill items are folded in further below instead, after the diff, since
        // they shouldn't count as "newly added" for notification purposes.
        if let Some(live) = &group_member_inner.collection_log_v2 {
            let baseline: &[i32] =
                previous_snapshot.as_ref().and_then(|p| p.collection_log.as_deref()).unwrap_or(&[]);
            group_member_inner.collection_log_v2 =
                Some(progress_events::merge_collection_log(baseline, live));
        }

        if let Some(previous) = &previous_snapshot {
            let mut progress_events = progress_events::diff_progress(previous, &group_member_inner);
            // A heartbeat that would post an implausible number of level-ups means the stored
            // `skills` snapshot itself is stale/wrong (see `looks_like_stale_skills_baseline`'s
            // doc comment), not that the player genuinely crossed that many thresholds at once.
            // Drop the level-up batch and re-baseline so the next heartbeat starts fresh instead
            // of flooding the feed with the account's whole level-up history.
            let skills_baseline_is_stale = progress_events::looks_like_stale_skills_baseline(&progress_events);
            if skills_baseline_is_stale {
                log::warn!(
                    "suppressing implausible level-up batch and re-baselining skills: group_id={} member={} level_up_count={}",
                    auth.group_id,
                    group_member_inner.name,
                    progress_events
                        .iter()
                        .filter(|event| event.event_type == progress_events::EVENT_TYPE_LEVEL_UP)
                        .count(),
                );
                progress_events.retain(|event| event.event_type != progress_events::EVENT_TYPE_LEVEL_UP);
                db::reset_member_skills_snapshot(&client, auth.group_id, &group_member_inner.name)
                    .await?;
            }
            if !progress_events.is_empty() {
                let session_id = db::ensure_open_session(&client, auth.group_id).await?;
                for event in &progress_events {
                    db::insert_progress_event(
                        &client,
                        auth.group_id,
                        session_id,
                        &group_member_inner.name,
                        event,
                    )
                    .await?;
                    redis.incr(&activity_log_version_key(auth.group_id)).await;
                    // Level-ups are dispatched separately below at Discord's own configurable
                    // per-level granularity, decoupled from this loop's fixed milestone schedule
                    // (see `progress_events::diff_skills_fine`) - dispatching them here too would
                    // double-post.
                    if event.event_type != progress_events::EVENT_TYPE_LEVEL_UP {
                        discord::dispatch_progress_webhook(
                            db_pool.get_ref().clone(),
                            auth.group_id,
                            group_member_inner.name.clone(),
                            event.clone(),
                            config.web_origin.clone(),
                        );
                    }
                }
            }
            // Not gated on `!progress_events.is_empty()` above - that only reflects the coarse
            // milestone schedule, which can be empty even when finer per-level crossings exist.
            if !skills_baseline_is_stale {
                if let (Some(previous_skills), Some(current_skills)) =
                    (&previous.skills, &group_member_inner.skills)
                {
                    for event in progress_events::diff_skills_fine(previous_skills, current_skills) {
                        discord::dispatch_progress_webhook(
                            db_pool.get_ref().clone(),
                            auth.group_id,
                            group_member_inner.name.clone(),
                            event,
                            config.web_origin.clone(),
                        );
                    }
                }
            }
        }

        // `collection_log_v2` was already folded onto the baseline above (before the diff);
        // `collection_log_sync` (bulk backfill from the player opening their log to browse) is
        // folded on now instead, after the diff, since it was never diffed/dispatched as its own
        // events. Either way what reaches the batcher (and therefore `COALESCE`-replaces the DB
        // row - see `update_batcher::build_update_sql`) ends up as the full merged state, not a
        // partial array that would wipe out everything the delta didn't happen to mention.
        // `.take()`n since it has no column of its own - it only ever exists to feed this merge.
        if let Some(sync) = group_member_inner.collection_log_sync.take() {
            let baseline: Vec<i32> = group_member_inner.collection_log_v2.clone().unwrap_or_else(|| {
                previous_snapshot.as_ref().and_then(|p| p.collection_log.clone()).unwrap_or_default()
            });
            group_member_inner.collection_log_v2 =
                Some(progress_events::merge_collection_log(&baseline, &sync));
        }
    }

    // Low-HP/wilderness-entry alerts are consumed once to trigger a web push (never stored,
    // unlike `events`/`interactions`/`object_interactions` above) - a no-op if the character
    // isn't account-linked yet or push isn't configured on this server.
    if let Some(alerts) = group_member_inner.alerts.take() {
        if let Some(account_id) = linked_account_id {
            for alert in alerts {
                push::dispatch_alert_push(
                    config.push.clone(),
                    db_pool.get_ref().clone(),
                    account_id,
                    group_member_inner.name.clone(),
                    alert,
                );
            }
        }
    }

    // Notable-drop events (loot crossing the plugin's configured value threshold) broadcast a
    // chat message to every connected group member - including the dropper themselves, unlike
    // `KillEvent` above, since nothing else tells them their own drop was notable - and
    // optionally post to Discord. Never stored, matching `alerts`' ephemeral handling above.
    if let Some(notable_drops) = group_member_inner.notable_drops.take() {
        for drop in notable_drops {
            let message = drop.to_message(&group_member_inner.name);

            if broadcast_registry.has_subscribers(auth.group_id) {
                let envelope = WsEnvelope::DropEvent {
                    payload: DropEventPayload {
                        member_name: group_member_inner.name.clone(),
                        message: message.clone(),
                    },
                    ts: Utc::now(),
                };
                if let Ok(json) = serde_json::to_string(&envelope) {
                    broadcast_registry.publish(auth.group_id, json);
                }
            }

            discord::dispatch_drop_webhook(
                db_pool.get_ref().clone(),
                auth.group_id,
                message,
                drop.item_id,
                drop.total_value,
            );
        }
    }

    // Slayer task assignment/close events from `SlayerTaskCloseEvents` - upserted by
    // `client_event_id` into `groupscape.slayer_task_history` (assignment writes a
    // not_started/in_progress row, the later close event for the same task updates that same
    // row in place rather than adding a second one). Never stored on `GroupMember` itself,
    // matching `notable_drops`' ephemeral handling above.
    if let Some(slayer_task_events) = group_member_inner.slayer_task_events.take() {
        for event in slayer_task_events {
            db::upsert_slayer_task_history_event(
                &client,
                auth.group_id,
                &group_member_inner.name,
                &event,
            )
            .await?;
        }
    }

    // Publish straight to any connected party overlays before handing off to
    // the batched DB writer - the batcher trades latency for write
    // efficiency, but the overlay wants these updates as fast as possible.
    if broadcast_registry.has_subscribers(auth.group_id) {
        // `group_member_inner` only carries the fields this particular heartbeat changed (the
        // plugin's DataState diffing skips unchanged ones), so target/spec/active-prayers are
        // routinely absent even while the member is alive and in combat. Broadcasting it as-is
        // would flash those fields to blank on every heartbeat that doesn't happen to touch
        // them. Merge onto the last persisted row first so the wire payload reflects full known
        // state, same as the `roster_snapshot` sent on connect.
        let previous = db::get_group_member(&client, auth.group_id, &group_member_inner.name).await?;
        let mut merged_for_broadcast = previous.unwrap_or_else(|| group_member_inner.clone());
        update_batcher::merge_group_member(&mut merged_for_broadcast, &group_member_inner);
        // This update just arrived, so it proves the member is alive right now - `previous`
        // was read before the batcher persists this heartbeat, so its last_updated is still the
        // prior heartbeat's timestamp. Stamping "now" here (rather than trusting the merge) is
        // what makes the live overlay/side-panel offline check track actual heartbeat cadence
        // instead of always reading ~one heartbeat interval stale.
        merged_for_broadcast.last_updated = Some(Utc::now());

        let envelope = WsEnvelope::VitalsUpdate {
            payload: VitalsUpdatePayload {
                name: group_member_inner.name.clone(),
                vitals: websocket::to_wire_vitals(&merged_for_broadcast),
            },
            ts: Utc::now(),
        };
        if let Ok(message) = serde_json::to_string(&envelope) {
            broadcast_registry.publish(auth.group_id, message);
        }
    }

    match sender.send(group_member_inner).await {
        Ok(_) => Ok(HttpResponse::Ok().finish()),
        Err(_) => Ok(HttpResponse::InternalServerError().body("Failed to submit player update")),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetGroupDataQuery {
    pub from_time: DateTime<Utc>,
}
#[get("/get-group-data")]
pub async fn get_group_data(
    auth: Authenticated,
    db_pool: web::Data<Pool>,
    query: web::Query<GetGroupDataQuery>,
) -> Result<web::Json<Vec<GroupMember>>, Error> {
    let from_time = query.from_time;
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let group_members = db::get_group_data(&client, auth.group_id, &from_time).await?;
    Ok(web::Json(group_members))
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PingAction {
    Start,
    Update,
    End,
}

/// Body for `submit_ping`. `memberName` is only ever sent by the client for `PingAction::Start`
/// (an update/end request only needs `pingId` to identify which of the sender's pings it applies
/// to - the sender's identity was already established when the ping started). `x`/`y`/`plane`/
/// `kind` are `None` for `PingAction::End` (a ping-clear only needs the id) and `kind`/`npc_name`
/// are `None` for `PingAction::Update` (only `Start` establishes what kind of ping this is -
/// updates just move it). Never persisted, matching the other ephemeral event types below
/// (`alerts`, `notable_drops`) - a ping is relayed straight to connected overlays and dropped.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PingRequest {
    pub action: PingAction,
    pub ping_id: String,
    #[serde(default)]
    pub member_name: Option<String>,
    #[serde(default)]
    pub kind: Option<PingKind>,
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
    #[serde(default)]
    pub plane: Option<i32>,
    #[serde(default)]
    pub npc_name: Option<String>,
}

/// `POST /ping` - relays a group member's ping (right-click/hotkey on an NPC or tile) to every
/// connected RuneLite party overlay as a `PingStart`/`PingUpdate`/`PingEnd` frame, and mirrors the
/// same lifecycle into `PingRegistry` so the web map (which doesn't read pings off its own `/ws`
/// connection - it polls `get-active-pings` on the same cadence it already polls member
/// positions) can pick it up too.
/// No DB write and no membership/ownership checks beyond the existing group-scope auth - the
/// sender's own client is the sole source of truth for its ping's lifecycle (e.g. clearing the
/// previous ping before starting a new one), matching this endpoint's ephemeral, trust-the-client
/// design.
pub async fn submit_ping(
    auth: Authenticated,
    body: web::Json<PingRequest>,
    broadcast_registry: web::Data<GroupBroadcastRegistry>,
    ping_registry: web::Data<PingRegistry>,
) -> Result<HttpResponse, Error> {
    let envelope = match body.action {
        PingAction::Start => {
            let (Some(x), Some(y), Some(plane), Some(kind), Some(member_name)) =
                (body.x, body.y, body.plane, body.kind, body.member_name.clone())
            else {
                return Ok(HttpResponse::BadRequest()
                    .body("start requires x, y, plane, kind and memberName"));
            };
            ping_registry.start(
                auth.group_id,
                ActivePing {
                    ping_id: body.ping_id.clone(),
                    member_name: member_name.clone(),
                    kind,
                    x,
                    y,
                    plane,
                    npc_name: body.npc_name.clone(),
                    last_seen: std::time::Instant::now(),
                },
            );
            WsEnvelope::PingStart {
                payload: PingStartPayload {
                    ping_id: body.ping_id.clone(),
                    member_name,
                    kind,
                    x,
                    y,
                    plane,
                    npc_name: body.npc_name.clone(),
                },
                ts: Utc::now(),
            }
        }
        PingAction::Update => {
            let (Some(x), Some(y), Some(plane)) = (body.x, body.y, body.plane) else {
                return Ok(HttpResponse::BadRequest().body("update requires x, y and plane"));
            };
            ping_registry.update(auth.group_id, &body.ping_id, x, y, plane);
            WsEnvelope::PingUpdate {
                payload: PingUpdatePayload {
                    ping_id: body.ping_id.clone(),
                    x,
                    y,
                    plane,
                },
                ts: Utc::now(),
            }
        }
        PingAction::End => {
            ping_registry.end(auth.group_id, &body.ping_id);
            WsEnvelope::PingEnd {
                payload: PingEndPayload {
                    ping_id: body.ping_id.clone(),
                },
                ts: Utc::now(),
            }
        }
    };

    if broadcast_registry.has_subscribers(auth.group_id) {
        if let Ok(message) = serde_json::to_string(&envelope) {
            broadcast_registry.publish(auth.group_id, message);
        }
    }
    Ok(HttpResponse::Ok().finish())
}

/// `GET /get-active-pings` - the web map's poll-based counterpart to the RuneLite-facing
/// broadcast in `submit_ping`. Polled on the same cadence the site already polls member
/// positions.
pub async fn get_active_pings(
    auth: Authenticated,
    ping_registry: web::Data<PingRegistry>,
) -> Result<web::Json<Vec<ActivePing>>, Error> {
    Ok(web::Json(ping_registry.list_active(auth.group_id)))
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RaidMarkerAction {
    Start,
    Update,
    End,
}

/// Body for `submit_raid_marker` - see `PingRequest` for the equivalent plain-ping version this
/// mirrors. `markerId` alone identifies which of the sender's (up to 8) active markers a request
/// applies to, since unlike a ping a player can have more than one active marker at a time.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RaidMarkerRequest {
    pub action: RaidMarkerAction,
    pub marker_id: String,
    #[serde(default)]
    pub member_name: Option<String>,
    #[serde(default)]
    pub marker_type: Option<MarkerType>,
    #[serde(default)]
    pub kind: Option<PingKind>,
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
    #[serde(default)]
    pub plane: Option<i32>,
    #[serde(default)]
    pub npc_name: Option<String>,
}

/// `POST /raid-marker` - relays a group member's raid marker (right-click "Raid Markers" submenu
/// on an NPC or tile) to every connected RuneLite party overlay as a `MarkerStart`/`MarkerUpdate`/
/// `MarkerEnd` frame, and mirrors the same lifecycle into `RaidMarkerRegistry` so the web map can
/// poll it via `get_active_raid_markers` - see `submit_ping`'s doc comment for why polling exists
/// alongside the websocket. Same ephemeral, trust-the-client design as `submit_ping`: no DB write,
/// and the sender's own client is the sole source of truth for a marker's lifecycle (clearing a
/// prior marker of the same type before starting a new one, clearing all of its markers on
/// logout/hop). Unlike a ping, a marker is never pruned by a server-side TTL.
pub async fn submit_raid_marker(
    auth: Authenticated,
    body: web::Json<RaidMarkerRequest>,
    broadcast_registry: web::Data<GroupBroadcastRegistry>,
    raid_marker_registry: web::Data<RaidMarkerRegistry>,
) -> Result<HttpResponse, Error> {
    let envelope = match body.action {
        RaidMarkerAction::Start => {
            let (Some(x), Some(y), Some(plane), Some(kind), Some(marker_type), Some(member_name)) = (
                body.x,
                body.y,
                body.plane,
                body.kind,
                body.marker_type,
                body.member_name.clone(),
            ) else {
                return Ok(HttpResponse::BadRequest()
                    .body("start requires x, y, plane, kind, markerType and memberName"));
            };
            raid_marker_registry.start(
                auth.group_id,
                ActiveRaidMarker {
                    marker_id: body.marker_id.clone(),
                    member_name: member_name.clone(),
                    marker_type,
                    kind,
                    x,
                    y,
                    plane,
                    npc_name: body.npc_name.clone(),
                    last_seen: std::time::Instant::now(),
                },
            );
            WsEnvelope::MarkerStart {
                payload: RaidMarkerStartPayload {
                    marker_id: body.marker_id.clone(),
                    member_name,
                    marker_type,
                    kind,
                    x,
                    y,
                    plane,
                    npc_name: body.npc_name.clone(),
                },
                ts: Utc::now(),
            }
        }
        RaidMarkerAction::Update => {
            let (Some(x), Some(y), Some(plane)) = (body.x, body.y, body.plane) else {
                return Ok(HttpResponse::BadRequest().body("update requires x, y and plane"));
            };
            raid_marker_registry.update(auth.group_id, &body.marker_id, x, y, plane);
            WsEnvelope::MarkerUpdate {
                payload: RaidMarkerUpdatePayload {
                    marker_id: body.marker_id.clone(),
                    x,
                    y,
                    plane,
                },
                ts: Utc::now(),
            }
        }
        RaidMarkerAction::End => {
            raid_marker_registry.end(auth.group_id, &body.marker_id);
            WsEnvelope::MarkerEnd {
                payload: RaidMarkerEndPayload {
                    marker_id: body.marker_id.clone(),
                },
                ts: Utc::now(),
            }
        }
    };

    if broadcast_registry.has_subscribers(auth.group_id) {
        if let Ok(message) = serde_json::to_string(&envelope) {
            broadcast_registry.publish(auth.group_id, message);
        }
    }
    Ok(HttpResponse::Ok().finish())
}

/// `GET /get-active-raid-markers` - the web map's poll-based counterpart to the RuneLite-facing
/// broadcast in `submit_raid_marker`.
pub async fn get_active_raid_markers(
    auth: Authenticated,
    raid_marker_registry: web::Data<RaidMarkerRegistry>,
) -> Result<web::Json<Vec<ActiveRaidMarker>>, Error> {
    Ok(web::Json(raid_marker_registry.list_active(auth.group_id)))
}

fn default_activity_limit() -> i64 {
    30
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetActivityEventsQuery {
    #[serde(default)]
    pub member_name: Option<String>,
    #[serde(default)]
    pub event_type: Option<String>,
    #[serde(default)]
    pub before: Option<DateTime<Utc>>,
    #[serde(default = "default_activity_limit")]
    pub limit: i64,
}
pub async fn get_activity_events(
    auth: Authenticated,
    db_pool: web::Data<Pool>,
    redis: web::Data<RedisCache>,
    query: web::Query<GetActivityEventsQuery>,
) -> Result<web::Json<Vec<ActivityEvent>>, Error> {
    let version = redis.get_i64(&activity_log_version_key(auth.group_id)).await;
    let cache_key = format!(
        "v1:activity-log:{}:{}:{}:{}:{}:{}",
        auth.group_id,
        version,
        query.member_name.as_deref().unwrap_or(""),
        query.event_type.as_deref().unwrap_or(""),
        query.before.map(|d| d.to_rfc3339()).unwrap_or_default(),
        query.limit,
    );
    if let Some(cached) = redis.get_json::<Vec<ActivityEvent>>(&cache_key).await {
        return Ok(web::Json(cached));
    }

    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let events = db::list_activity_events(
        &client,
        auth.group_id,
        query.member_name.as_deref(),
        query.event_type.as_deref(),
        query.before,
        query.limit,
    )
    .await?;
    redis.set_json(&cache_key, &events, ACTIVITY_LOG_CACHE_TTL_SECS).await;
    Ok(web::Json(events))
}

/// Group-wide likes/comments toggle - reachable by any group member (no permission gate), since
/// the activity feed itself needs to know whether to render reaction UI at all, not just the
/// admin settings page. See `update_activity_settings` for the gated write side.
#[get("/get-activity-settings")]
pub async fn get_activity_settings(
    auth: Authenticated,
    db_pool: web::Data<Pool>,
) -> Result<web::Json<ActivitySettings>, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let settings = db::get_activity_settings(&client, auth.group_id).await?;
    Ok(web::Json(settings))
}

/// Gated on `ManageSettings`, same permission `group-settings.js`'s colour picker and other
/// group-wide toggles use - matches the product spec's "changeable by group owner/admin only"
/// (the group admin holds every permission implicitly, see `has_group_permission`).
#[put("/update-activity-settings")]
pub async fn update_activity_settings(
    req: HttpRequest,
    auth: Authenticated,
    body: web::Json<ActivitySettings>,
    db_pool: web::Data<Pool>,
) -> Result<web::Json<ActivitySettings>, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    require_group_permission(&req, &client, auth.group_id, PermissionKey::ManageSettings).await?;
    let settings = body.into_inner();
    db::update_activity_settings(&client, auth.group_id, settings).await?;
    Ok(web::Json(settings))
}

/// Confirms `event_id` belongs to `group_id` before any react/comment handler touches it - every
/// one of those routes takes a bare `{event_id}` with no group in the path, so without this check
/// an account in group A could react to/comment on group B's activity purely by guessing ids.
async fn require_activity_event_in_group(
    client: &Client,
    group_id: i64,
    event_id: i64,
) -> Result<String, ApiError> {
    let (event_group_id, member_name) = db::get_activity_event_group_and_member(client, event_id)
        .await?
        .ok_or(ApiError::ActivityEventNotFoundError)?;
    if event_group_id != group_id {
        return Err(ApiError::ActivityEventNotFoundError);
    }
    Ok(member_name)
}

#[derive(Deserialize)]
pub struct ActivityEventPath {
    pub event_id: i64,
}

/// Toggles the caller's reaction on an activity event (§ product spec: tap = `like`, long-press
/// radial = the other 4 types). Same type twice removes it; a different type replaces it -
/// [`db::toggle_activity_reaction`] does both in one round trip. Requires a logged-in account
/// (`X-Account-Authorization`) - there is no notion of an anonymous reaction. Reacting to your own
/// activity is allowed (§ product spec, revised) - the event only has to belong to this group.
#[put("/activity-events/{event_id}/react")]
pub async fn react_to_activity_event(
    req: HttpRequest,
    auth: Authenticated,
    path: web::Path<ActivityEventPath>,
    body: web::Json<ReactToActivityEventRequest>,
    db_pool: web::Data<Pool>,
    redis: web::Data<RedisCache>,
) -> Result<web::Json<ActivityReactionsSummary>, Error> {
    let mut client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let account_id = require_account(&req, &client).await?;

    let settings = db::get_activity_settings(&client, auth.group_id).await?;
    if !settings.reactions_enabled {
        return Err(ApiError::ActivityReactionsDisabledError.into());
    }
    if !ACTIVITY_REACTION_KINDS.contains(&body.reaction.as_str()) {
        return Err(ApiError::InvalidReactionTypeError.into());
    }

    require_activity_event_in_group(&client, auth.group_id, path.event_id).await?;

    let summary =
        db::toggle_activity_reaction(&mut client, path.event_id, account_id, &body.reaction).await?;
    redis.incr(&activity_log_version_key(auth.group_id)).await;
    Ok(web::Json(summary))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetActivityReactionsQuery {
    /// Comma-separated event ids - the activity feed page polls this for whatever page of events
    /// it currently has rendered (see `activity-feed-page.js`'s `refreshReactions`), separately
    /// from `get-activity-events`' own cursor pagination, since a reaction/comment on an
    /// already-loaded event never changes that event's `occurred_at` and so would never surface
    /// through the feed's "only prepend strictly newer events" poll otherwise. Same
    /// comma-joined-query-param shape as `get-loot-log`'s `item_ids`/`categories`.
    #[serde(default)]
    pub event_ids: Option<String>,
}
fn parse_activity_event_ids(raw: Option<&str>) -> Vec<i64> {
    raw.unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<i64>().ok())
        .collect()
}

const MAX_ACTIVITY_REACTIONS_BATCH: usize = 200;

/// Batch reaction+comment-count lookup for a set of already-loaded activity events - see
/// [`GetActivityReactionsQuery`] for why this is separate from `get-activity-events`. Returns an
/// empty map when the group has reactions turned off, so the frontend's "is this feature even on"
/// check can be this response being empty-vs-populated rather than a second settings fetch.
#[get("/get-activity-reactions")]
pub async fn get_activity_reactions(
    req: HttpRequest,
    auth: Authenticated,
    query: web::Query<GetActivityReactionsQuery>,
    db_pool: web::Data<Pool>,
) -> Result<web::Json<HashMap<i64, ActivityReactionsSummary>>, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let event_ids = parse_activity_event_ids(query.event_ids.as_deref());
    let settings = db::get_activity_settings(&client, auth.group_id).await?;
    if !settings.reactions_enabled || event_ids.is_empty() {
        return Ok(web::Json(HashMap::new()));
    }

    let event_ids: Vec<i64> = event_ids.into_iter().take(MAX_ACTIVITY_REACTIONS_BATCH).collect();
    // Best-effort account resolution: this endpoint is reachable without being logged in (reading
    // the feed never requires an account), so a missing/invalid header just means every event's
    // `my_reaction` comes back `None` rather than a 401.
    let account_id = req
        .headers()
        .get(ACCOUNT_AUTH_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(crate::crypto::session_token_hash);
    let account_id = match account_id {
        Some(hash) => db::get_account_by_session_token_hash(&client, &hash)
            .await?
            .map(|account| account.id),
        None => None,
    };

    let summaries = db::get_reactions_summary_for_events(&client, &event_ids, account_id).await?;
    Ok(web::Json(summaries))
}

/// Lists an event's comments oldest-first, plus the current count for the "n / 10 comments"
/// indicator. Reachable without an account (same reasoning as `get_activity_reactions`) - only
/// posting a comment requires being logged in.
#[get("/activity-events/{event_id}/comments")]
pub async fn list_activity_comments(
    auth: Authenticated,
    path: web::Path<ActivityEventPath>,
    db_pool: web::Data<Pool>,
) -> Result<web::Json<ActivityCommentsPage>, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let settings = db::get_activity_settings(&client, auth.group_id).await?;
    if !settings.reactions_enabled {
        return Err(ApiError::ActivityReactionsDisabledError.into());
    }
    require_activity_event_in_group(&client, auth.group_id, path.event_id).await?;

    let comments = db::list_activity_comments(&client, path.event_id).await?;
    let comment_count = comments.len() as i64;
    Ok(web::Json(ActivityCommentsPage { comments, comment_count }))
}

/// Adds a comment as the caller's linked character in this group. Unlike reactions, commenting on
/// your own activity is allowed (§ product spec) - only the 10-per-event cap
/// ([`db::add_activity_comment`]'s atomic `WHERE COUNT(*) < 10`) can reject this.
#[post("/activity-events/{event_id}/comments")]
pub async fn add_activity_comment(
    req: HttpRequest,
    auth: Authenticated,
    path: web::Path<ActivityEventPath>,
    body: web::Json<AddActivityCommentRequest>,
    db_pool: web::Data<Pool>,
    redis: web::Data<RedisCache>,
) -> Result<web::Json<ActivityCommentsPage>, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let account_id = require_account(&req, &client).await?;

    let settings = db::get_activity_settings(&client, auth.group_id).await?;
    if !settings.reactions_enabled {
        return Err(ApiError::ActivityReactionsDisabledError.into());
    }
    require_activity_event_in_group(&client, auth.group_id, path.event_id).await?;

    let comment_text = body.comment_text.trim().to_string();
    if comment_text.is_empty() || comment_text.chars().count() > ACTIVITY_COMMENT_MAX_LEN {
        return Err(ApiError::ActivityCommentValidationError(format!(
            "Comment must be between 1 and {} characters",
            ACTIVITY_COMMENT_MAX_LEN
        ))
        .into());
    }

    let member_name = db::resolve_member_name_for_account(&client, auth.group_id, account_id)
        .await?
        .ok_or(ApiError::CommentRequiresLinkedCharacterError)?;

    let inserted =
        db::add_activity_comment(&client, path.event_id, account_id, &member_name, &comment_text)
            .await?;
    if inserted.is_none() {
        return Err(ApiError::ActivityCommentLimitReachedError.into());
    }
    redis.incr(&activity_log_version_key(auth.group_id)).await;

    let comments = db::list_activity_comments(&client, path.event_id).await?;
    let comment_count = comments.len() as i64;
    Ok(web::Json(ActivityCommentsPage { comments, comment_count }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetChatMessagesQuery {
    /// The account's last-seen `message_id` cursor - see the "Chat history and backfill
    /// behavior" spec ticket. `0` (a fresh account) returns the most recent page instead of full
    /// history.
    #[serde(default)]
    pub since: i64,
}

/// Own surface, own table - not the activity feed. Backfills up to `CHAT_BACKFILL_CAP` messages
/// newer than `since`, oldest-first. Cursor *advancement* (delivery vs. read) is tracked
/// separately and isn't this endpoint's concern - see the "Chat history and backfill behavior"
/// and "New-message notification behavior" spec tickets.
#[get("/get-chat-messages")]
pub async fn get_chat_messages(
    auth: Authenticated,
    query: web::Query<GetChatMessagesQuery>,
    db_pool: web::Data<Pool>,
) -> Result<web::Json<Vec<ChatMessage>>, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let messages = db::list_chat_messages_since(&client, auth.group_id, query.since).await?;
    Ok(web::Json(messages))
}

/// Sends a group chat message as the caller's linked character. Requires a linked character
/// (`ChatRequiresLinkedCharacterError`) - see the "Chat auth and permission model" spec ticket.
/// Truncates silently to `CHAT_MESSAGE_MAX_LEN` rather than rejecting - see the "Message
/// formatting and length limits" spec ticket. Flood-guarded via `ChatRateLimiter`, keyed on
/// `account_id` so it survives reconnects - see the "Minimum flood-protection guard" spec ticket.
/// An over-cap attempt is dropped (never reaches `db::add_chat_message`) and reported to the
/// sender two ways: the HTTP response is `ApiError::ChatRateLimited`, and a `ChatRateLimited`
/// envelope is broadcast so every one of the sender's own connected sessions (plugin + webapp
/// tabs) can react, not just whichever one made this particular call.
///
/// Reachable from two auth scopes with different notions of "who's sending": the character-key
/// scope (the plugin) already resolved an `account_id` from the API key during auth - see
/// `AuthenticationResult::account_id` - so use that directly. The group-token scope (the webapp)
/// has no such notion; fall back to the webapp's own session header there, exactly as before this
/// endpoint was reachable from the plugin. See the "!gs" chat spec's account_id resolution ticket.
#[post("/send-chat-message")]
pub async fn send_chat_message(
    req: HttpRequest,
    auth: Authenticated,
    body: web::Json<SendChatMessageRequest>,
    db_pool: web::Data<Pool>,
    broadcast_registry: web::Data<GroupBroadcastRegistry>,
    chat_rate_limiter: web::Data<websocket::ChatRateLimiter>,
) -> Result<web::Json<ChatMessage>, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let account_id = match auth.account_id {
        Some(account_id) => account_id,
        None => require_account(&req, &client).await?,
    };

    let member_name = db::resolve_member_name_for_account(&client, auth.group_id, account_id)
        .await?
        .ok_or(ApiError::ChatRequiresLinkedCharacterError)?;

    let text: String = body.text.trim().chars().take(CHAT_MESSAGE_MAX_LEN).collect();
    if text.is_empty() {
        return Err(ApiError::ChatMessageValidationError("Chat message must not be empty".to_string())
            .into());
    }

    if !chat_rate_limiter.check_and_record(account_id) {
        let envelope = WsEnvelope::ChatRateLimited {
            payload: websocket::ChatRateLimitedPayload {
                member_name: Some(member_name),
            },
            ts: Utc::now(),
        };
        if let Ok(json) = serde_json::to_string(&envelope) {
            broadcast_registry.publish(auth.group_id, json);
        }
        return Err(ApiError::ChatRateLimited.into());
    }

    let message =
        db::add_chat_message(&client, auth.group_id, account_id, Some(&member_name), &text)
            .await?;

    let envelope = WsEnvelope::ChatMessage {
        payload: ChatMessagePayload {
            message_id: message.message_id,
            member_name: message.member_name.clone(),
            text: message.message_text.clone(),
        },
        ts: Utc::now(),
    };
    if let Ok(json) = serde_json::to_string(&envelope) {
        broadcast_registry.publish(auth.group_id, json);
    }

    Ok(web::Json(message))
}

fn default_sessions_limit() -> i64 {
    20
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetSessionsQuery {
    #[serde(default = "default_sessions_limit")]
    pub limit: i64,
}
pub async fn get_sessions(
    auth: Authenticated,
    db_pool: web::Data<Pool>,
    redis: web::Data<RedisCache>,
    query: web::Query<GetSessionsQuery>,
) -> Result<web::Json<Vec<GroupSession>>, Error> {
    let cache_key = format!("v1:sessions:{}:{}", auth.group_id, query.limit);
    if let Some(cached) = redis.get_json::<Vec<GroupSession>>(&cache_key).await {
        return Ok(web::Json(cached));
    }
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let sessions = db::list_sessions(&client, auth.group_id, query.limit).await?;
    redis.set_json(&cache_key, &sessions, CACHE_TTL_SECS).await;
    Ok(web::Json(sessions))
}

/// One normalized (source_name, source_type, clue_tier, loot) view over either a
/// `GameEvent::Kill` or `GameEvent::Loot` payload, so the loot log endpoints below don't need
/// parallel match arms for every field they read.
struct LootSourceEvent {
    source_name: String,
    source_type: &'static str,
    clue_tier: Option<String>,
    loot: Vec<LootItem>,
}
/// Synthetic NPC names that have shown up in real kill/loot data from manual plugin testing -
/// never a real drop, so they're filtered out of the Loot Log rather than needing a DB cleanup
/// every time one turns up.
const DIAGNOSTIC_NPC_NAMES: &[&str] = &["DiagTestNpc"];

fn is_diagnostic_source(source_name: &str) -> bool {
    DIAGNOSTIC_NPC_NAMES
        .iter()
        .any(|name| name.eq_ignore_ascii_case(source_name))
}

/// Gauntlet/Corrupted Gauntlet trash mobs - real NPCs (unlike [`DIAGNOSTIC_NPC_NAMES`]) whose
/// kills/loot are still fine to show in the activity feed if they somehow kill a player, but are
/// too noisy to be worth surfacing in the Loot Log.
const EXCLUDED_LOOT_LOG_NPC_NAMES: &[&str] = &[
    "Corrupted wolf",
    "Crystalline wolf",
    "Crystalline spider",
    "Corrupted spider",
    "Corrupted bat",
    "Crystalline bat",
    "Crystalline rat",
    "Corrupted rat",
    "Crystalline dragon",
    "Corrupted dragon",
    "Crystalline bear",
    "Corrupted bear",
    "Crystalline dark beast",
    "Corrupted dark beast",
    "Crystalline scorpion",
    "Corrupted scorpion",
    "Corrupted unicorn",
    "Crystalline unicorn",
    "Ba-Ba",
    "Akkha",
    "Kephri",
    "Zebak",
];

fn is_excluded_loot_log_source(source_name: &str) -> bool {
    EXCLUDED_LOOT_LOG_NPC_NAMES
        .iter()
        .any(|name| name.eq_ignore_ascii_case(source_name))
}

fn as_loot_source_event(event: &GameEvent) -> Option<LootSourceEvent> {
    match event {
        GameEvent::Kill(kill) => Some(LootSourceEvent {
            source_name: kill.npc_name.clone(),
            source_type: "kill",
            clue_tier: None,
            loot: kill.loot.clone().unwrap_or_default(),
        }),
        GameEvent::Loot(loot_event) => Some(LootSourceEvent {
            source_name: loot_event.source_name.clone(),
            source_type: match loot_event.source_type {
                crate::models::LootSourceType::Chest => "chest",
                crate::models::LootSourceType::Clue => "clue",
            },
            clue_tier: loot_event.clue_tier.clone(),
            loot: loot_event.loot.clone(),
        }),
        GameEvent::Death(_) => None,
        // Raid completions are stored under `event_type = "raid"`, excluded from
        // `list_loot_and_kill_events_page`'s `event_type IN ('kill', 'loot')` filter entirely, and
        // use a payload shape (`RaidCompletionPayload`) this enum doesn't even model - this arm
        // only exists to keep the match exhaustive.
        GameEvent::Raid(_) => None,
    }
}
fn parse_item_ids(raw: Option<&str>) -> std::collections::HashSet<i32> {
    raw.unwrap_or("")
        .split(',')
        .filter_map(|part| part.trim().parse::<i32>().ok())
        .collect()
}

/// Which of the Loot Log page's four filter categories are active - "boss" and "other" both
/// split `source_type == "kill"` further (see [`LootLogCategories::matches`]), keyed off the
/// same curated [`crate::notable_npcs`] list the activity feed uses to decide "is this worth
/// calling out". `None`/empty/unparseable input means all four are active, matching the client's
/// own default-all-selected state - this endpoint never needs to distinguish "no filter" from
/// "every category explicitly on".
struct LootLogCategories {
    boss: bool,
    other: bool,
    chest: bool,
    clue: bool,
}
impl LootLogCategories {
    fn parse(raw: Option<&str>) -> Self {
        let raw = raw.unwrap_or("").trim();
        if raw.is_empty() {
            return Self { boss: true, other: true, chest: true, clue: true };
        }
        let parts: std::collections::HashSet<&str> = raw.split(',').map(str::trim).collect();
        Self {
            boss: parts.contains("boss"),
            other: parts.contains("other"),
            chest: parts.contains("chest"),
            clue: parts.contains("clue"),
        }
    }

    fn matches(&self, source: &LootSourceEvent) -> bool {
        match source.source_type {
            "kill" => {
                if crate::notable_npcs::is_notable(&source.source_name) {
                    self.boss
                } else {
                    self.other
                }
            }
            "chest" => self.chest,
            "clue" => self.clue,
            _ => true,
        }
    }
}

#[cfg(test)]
mod loot_log_categories_tests {
    use super::*;

    fn source(source_type: &'static str, source_name: &str) -> LootSourceEvent {
        LootSourceEvent {
            source_name: source_name.to_string(),
            source_type,
            clue_tier: None,
            loot: Vec::new(),
        }
    }

    #[test]
    fn no_query_param_matches_everything() {
        let categories = LootLogCategories::parse(None);
        assert!(categories.matches(&source("kill", "Vorkath")));
        assert!(categories.matches(&source("kill", "Goblin")));
        assert!(categories.matches(&source("chest", "Chambers of Xeric")));
        assert!(categories.matches(&source("clue", "Master Casket")));
    }

    #[test]
    fn empty_or_whitespace_param_matches_everything() {
        assert!(LootLogCategories::parse(Some("")).matches(&source("kill", "Goblin")));
        assert!(LootLogCategories::parse(Some("  ")).matches(&source("kill", "Goblin")));
    }

    #[test]
    fn splits_kill_events_into_boss_and_other() {
        let bosses_only = LootLogCategories::parse(Some("boss"));
        assert!(bosses_only.matches(&source("kill", "Vorkath")));
        assert!(!bosses_only.matches(&source("kill", "Goblin")));

        let other_only = LootLogCategories::parse(Some("other"));
        assert!(!other_only.matches(&source("kill", "Vorkath")));
        assert!(other_only.matches(&source("kill", "Goblin")));
    }

    #[test]
    fn filters_chest_and_clue_independently() {
        let chest_only = LootLogCategories::parse(Some("chest"));
        assert!(chest_only.matches(&source("chest", "Barrows")));
        assert!(!chest_only.matches(&source("clue", "Master Casket")));
        assert!(!chest_only.matches(&source("kill", "Vorkath")));

        let clue_only = LootLogCategories::parse(Some("clue"));
        assert!(clue_only.matches(&source("clue", "Master Casket")));
        assert!(!clue_only.matches(&source("chest", "Barrows")));
    }

    #[test]
    fn accepts_multiple_comma_separated_categories() {
        let boss_and_chest = LootLogCategories::parse(Some("boss,chest"));
        assert!(boss_and_chest.matches(&source("kill", "Vorkath")));
        assert!(boss_and_chest.matches(&source("chest", "Barrows")));
        assert!(!boss_and_chest.matches(&source("kill", "Goblin")));
        assert!(!boss_and_chest.matches(&source("clue", "Master Casket")));
    }
}

/// Builds one `LootLogEvent` from a normalized loot source, applying the search grammar (see
/// `loot_log_search`) when `search_active`. `groups` comes from `split_search_groups`: every
/// group must match (AND across groups, e.g. `vorkath && >1m`), while tokens within a single
/// group still match on ANY of them (OR), same as before `&&`/`and` support existed. Returns
/// `None` when a search is active and this event fails at least one group. Item-level `matched`
/// flags are only set (`Some(_)`) when the match came from at least one item individually
/// satisfying a value/quantity/id clause in some group - if the event only ever matched via
/// member/source-name substring or the monster-level clause, every item is left un-dimmed
/// (`Some(true)`), since none of them individually "matched" anything.
fn build_matching_loot_log_event(
    source: &LootSourceEvent,
    member_name: &str,
    occurred_at: DateTime<Utc>,
    ge_prices: &crate::models::GEPrices,
    groups: &[Vec<String>],
    item_ids: &std::collections::HashSet<i32>,
    search_active: bool,
) -> Option<LootLogEvent> {
    let mut items: Vec<LootLogItem> = Vec::with_capacity(source.loot.len());
    for item in &source.loot {
        let unit_value = ge_prices.get(&item.item_id).copied();
        let total_value = unit_value.unwrap_or(0) * item.quantity as i64;
        let drop = (source.clue_tier.is_none())
            .then(|| drop_rates::lookup(&source.source_name, item.item_id))
            .flatten();
        items.push(LootLogItem {
            item_id: item.item_id,
            item_name: drop.map(|d| d.name.clone()),
            quantity: item.quantity,
            unit_value,
            total_value,
            rarity: drop.map(|d| d.rarity.clone()),
            is_unique: drop.map(|d| d.is_unique).unwrap_or(false),
            drop_rate: drop.and_then(|d| d.rate.clone()),
            matched: None,
        });
    }

    if !search_active {
        return Some(LootLogEvent {
            member_name: member_name.to_string(),
            occurred_at,
            source_name: source.source_name.clone(),
            source_type: source.source_type.to_string(),
            clue_tier: source.clue_tier.clone(),
            items,
        });
    }

    let member_lower = member_name.to_lowercase();
    let source_lower = source.source_name.to_lowercase();
    let level = combat_level(&source.source_name);
    // A numeric clause can also match this kill's combined loot value (e.g. `>1m` should surface
    // a kill whose drops are collectively worth over a million even if no single item stack
    // clears that bar on its own) - alongside, not instead of, the per-item matching below.
    let kill_total_value: i64 = items.iter().map(|item| item.total_value).sum();

    let mut item_matched_in_any_group = vec![false; items.len()];
    for group in groups {
        // A kill-count clause (e.g. `>10kc`) can't be evaluated per-event - it's a farming-session
        // total the client computes after merging events (see `extract_kill_count_clause`'s doc
        // comment). Strip it here so it isn't misread as a literal text token that would wrongly
        // exclude this event; a group that was ONLY a kill-count clause imposes no server-side
        // constraint at all, since the client applies it independently.
        let (kill_count_clause, group) = extract_kill_count_clause(group);
        if kill_count_clause.is_some() && group.is_empty() {
            continue;
        }

        // A group that's nothing but a single value clause (the whole search is just `>100k`, or
        // one side of `vorkath && >100k`) reads as "worth over X" against the number actually
        // shown on a farming-session card - loot-log-group.js's merged `totalValue` - not a lone
        // raw event's own total, so (like a kill-count clause above) it can't safely exclude an
        // event here: a real 500k session made of many small kills would otherwise vanish because
        // no single kill in it individually cleared the bar (this was itself a filed bug - see
        // `matches_on_combined_kill_value_even_when_no_single_item_clears_the_bar`, which only
        // covers one event's own combined total, not a whole session). So a bare value group
        // imposes no server-side exclusion either; `loot-log-page.js`'s own copy of this carve-out
        // (`parseValueClauses`) re-filters session cards afterward against session-total-OR-any-
        // event-total, once session merging has actually happened client-side. Item-level
        // matching/dimming below still runs as normal - only the final exclusion check is skipped.
        let bare_value_group = group.len() == 1 && parse_numeric_clause(&group[0]).is_some();

        // Sorts each remaining token in the group into exactly one clause bucket: `unique`/rarity
        // keywords and drop-rate fractions are per-item alternatives alongside id/value matching
        // (see the per-item loop below); anything left over falls through to the existing
        // name-substring/combat-level text path.
        let mut numeric_clauses: Vec<_> = Vec::new();
        let mut drop_rate_clauses: Vec<_> = Vec::new();
        let mut unique_required = false;
        let mut rarity_required: Option<&'static str> = None;
        let mut text_tokens: Vec<String> = Vec::new();
        for token in &group {
            if token.eq_ignore_ascii_case("unique") {
                unique_required = true;
            } else if let Some(keyword) = rarity_keyword(token) {
                rarity_required = Some(keyword);
            } else if let Some(clause) = parse_drop_rate_clause(token) {
                drop_rate_clauses.push(clause);
            } else if let Some(clause) = parse_numeric_clause(token) {
                numeric_clauses.push(clause);
            } else {
                text_tokens.push(token.to_lowercase());
            }
        }

        let mut context_matched = text_tokens.iter().any(|t| {
            member_lower.contains(t.as_str())
                || source_lower.contains(t.as_str())
                || matches_category_keyword(t, source.source_type)
        });
        if !context_matched {
            if let Some(level) = level {
                context_matched = numeric_clauses
                    .iter()
                    .any(|clause| numeric_clause_matches(clause, level as i64));
            }
        }

        let mut any_item_level_matched = false;
        for (idx, item) in items.iter().enumerate() {
            let id_match = item_ids.contains(&item.item_id);
            let value_match = numeric_clauses.iter().any(|clause| {
                numeric_clause_matches(clause, item.total_value)
                    || item
                        .unit_value
                        .map(|v| numeric_clause_matches(clause, v))
                        .unwrap_or(false)
                    || numeric_clause_matches(clause, item.quantity as i64)
            });
            let unique_match = unique_required && item.is_unique;
            let rarity_match = rarity_required.is_some_and(|kw| matches_rarity_keyword(kw, item.rarity.as_deref()));
            let drop_rate_match = !drop_rate_clauses.is_empty()
                && item
                    .drop_rate
                    .as_deref()
                    .and_then(drop_rates::probability)
                    .is_some_and(|p| drop_rate_clauses.iter().any(|clause| numeric_clause_matches_f64(clause, p)));
            if id_match || value_match || unique_match || rarity_match || drop_rate_match {
                any_item_level_matched = true;
                item_matched_in_any_group[idx] = true;
            }
        }

        let total_value_matched = numeric_clauses
            .iter()
            .any(|clause| numeric_clause_matches(clause, kill_total_value));

        if !bare_value_group && !(context_matched || any_item_level_matched || total_value_matched) {
            return None;
        }
    }

    let any_item_matched = item_matched_in_any_group.iter().any(|m| *m);
    for (idx, item) in items.iter_mut().enumerate() {
        item.matched = Some(if any_item_matched { item_matched_in_any_group[idx] } else { true });
    }

    Some(LootLogEvent {
        member_name: member_name.to_string(),
        occurred_at,
        source_name: source.source_name.clone(),
        source_type: source.source_type.to_string(),
        clue_tier: source.clue_tier.clone(),
        items,
    })
}

#[cfg(test)]
mod build_matching_loot_log_event_tests {
    use super::*;
    use crate::models::LootItem;

    fn source(loot: Vec<LootItem>) -> LootSourceEvent {
        LootSourceEvent { source_name: "Dust devil".to_string(), source_type: "kill", clue_tier: None, loot }
    }

    // Real curated drop_rates.json entries under "vorkath" - needed to exercise unique/rarity/
    // drop-rate keywords, since those are looked up from the curated table, not synthesized.
    // Item 11286 (Draconic visage): very_rare, unique, 1/5000. Item 383 (Raw shark): uncommon,
    // not unique, 3/300.
    fn vorkath_source(loot: Vec<LootItem>) -> LootSourceEvent {
        LootSourceEvent { source_name: "Vorkath".to_string(), source_type: "kill", clue_tier: None, loot }
    }

    fn call(source: &LootSourceEvent, ge_prices: &crate::models::GEPrices, search: &str) -> Option<LootLogEvent> {
        let groups = split_search_groups(search);
        build_matching_loot_log_event(
            source,
            "Some Member",
            Utc::now(),
            ge_prices,
            &groups,
            &std::collections::HashSet::new(),
            true,
        )
    }

    #[test]
    fn matches_on_combined_kill_value_even_when_no_single_item_clears_the_bar() {
        // Three items each worth well under 1m individually, but summing past it together.
        let source = source(vec![
            LootItem { item_id: 1, quantity: 1 },
            LootItem { item_id: 2, quantity: 1 },
            LootItem { item_id: 3, quantity: 1 },
        ]);
        let ge_prices = crate::models::GEPrices::from([(1, 400_000), (2, 400_000), (3, 400_000)]);

        let event = call(&source, &ge_prices, ">1m").expect("kill's combined loot value should match >1m");
        // Matched only via the combined total, not any single item - none should be dimmed.
        assert!(event.items.iter().all(|item| item.matched == Some(true)));
    }

    #[test]
    fn still_matches_and_dims_on_a_single_qualifying_item() {
        let source = source(vec![LootItem { item_id: 1, quantity: 1 }, LootItem { item_id: 2, quantity: 1 }]);
        let ge_prices = crate::models::GEPrices::from([(1, 2_000_000), (2, 10)]);

        let event = call(&source, &ge_prices, ">1m").expect("item worth over 1m should match");
        assert_eq!(event.items[0].matched, Some(true));
        assert_eq!(event.items[1].matched, Some(false));
    }

    #[test]
    fn bare_value_group_imposes_no_server_side_constraint() {
        // ">1m" alone is a "bare" value group (see `bare_value_group` in
        // build_matching_loot_log_event) - it can't be checked against a lone event's own total
        // the way a combined clause still is, since the number the user means is a farming-
        // session's merged total, which only exists client-side after session grouping. So this
        // event (worth well under 1m) still passes through server-side, undimmed (no item
        // individually matched anything), and the client re-filters by session/event total.
        let source = source(vec![LootItem { item_id: 1, quantity: 1 }, LootItem { item_id: 2, quantity: 1 }]);
        let ge_prices = crate::models::GEPrices::from([(1, 1_000), (2, 2_000)]);

        let event = call(&source, &ge_prices, ">1m").expect("bare value clause should not exclude server-side");
        assert!(event.items.iter().all(|item| item.matched == Some(true)));
    }

    #[test]
    fn double_ampersand_requires_every_group_to_match() {
        // Source is "Dust devil" (not vorkath), so "vorkath && >1m" should fail even though the
        // kill clears the value bar - both AND'd groups must independently match.
        let source = source(vec![LootItem { item_id: 1, quantity: 1 }]);
        let ge_prices = crate::models::GEPrices::from([(1, 2_000_000)]);

        assert!(call(&source, &ge_prices, "vorkath && >1m").is_none());
        assert!(call(&source, &ge_prices, "dust && >1m").is_some());
    }

    #[test]
    fn and_word_behaves_like_double_ampersand() {
        let source = source(vec![LootItem { item_id: 1, quantity: 1 }]);
        let ge_prices = crate::models::GEPrices::from([(1, 2_000_000)]);

        assert!(call(&source, &ge_prices, "dust and >1m").is_some());
        assert!(call(&source, &ge_prices, "vorkath and >1m").is_none());
    }

    #[test]
    fn kill_count_only_group_imposes_no_server_side_constraint() {
        // "vorkath && >10kc" - the kc half can't be checked per-event, so a lone Dust devil kill
        // that matches the (unrelated, wrong-boss) name half should still fail on "vorkath" alone,
        // while a matching-name event should pass through untouched by the kc clause.
        let source = source(vec![LootItem { item_id: 1, quantity: 1 }]);
        let ge_prices = crate::models::GEPrices::from([(1, 10)]);

        assert!(call(&source, &ge_prices, "vorkath && >10kc").is_none());
        assert!(call(&source, &ge_prices, "dust && >10kc").is_some());
        // A search that's purely a kill-count clause matches every event server-side - the client
        // applies the actual count filter after session grouping.
        assert!(call(&source, &ge_prices, ">10kc").is_some());
    }

    #[test]
    fn unique_keyword_matches_only_events_with_a_unique_drop() {
        let ge_prices = crate::models::GEPrices::from([(11286, 5_000_000), (383, 100)]);

        let with_unique = vorkath_source(vec![LootItem { item_id: 11286, quantity: 1 }]);
        assert!(call(&with_unique, &ge_prices, "unique").is_some());

        let without_unique = vorkath_source(vec![LootItem { item_id: 383, quantity: 1 }]);
        assert!(call(&without_unique, &ge_prices, "unique").is_none());
    }

    #[test]
    fn rare_keyword_matches_very_rare_tier_too() {
        let ge_prices = crate::models::GEPrices::from([(11286, 5_000_000), (383, 100)]);

        let very_rare_item = vorkath_source(vec![LootItem { item_id: 11286, quantity: 1 }]);
        assert!(call(&very_rare_item, &ge_prices, "rare").is_some());

        let uncommon_item = vorkath_source(vec![LootItem { item_id: 383, quantity: 1 }]);
        assert!(call(&uncommon_item, &ge_prices, "rare").is_none());
    }

    #[test]
    fn drop_rate_clause_matches_by_probability() {
        let ge_prices = crate::models::GEPrices::from([(11286, 5_000_000), (383, 100)]);

        // Draconic visage is 1/5000 - rarer than 1/1000.
        let rare_drop = vorkath_source(vec![LootItem { item_id: 11286, quantity: 1 }]);
        assert!(call(&rare_drop, &ge_prices, "<1/1000").is_some());

        // Raw shark is 3/300 (1%) - not rarer than 1/1000.
        let common_drop = vorkath_source(vec![LootItem { item_id: 383, quantity: 1 }]);
        assert!(call(&common_drop, &ge_prices, "<1/1000").is_none());
    }

    #[test]
    fn category_keyword_matches_source_type_generically() {
        let ge_prices = crate::models::GEPrices::from([(1, 100)]);
        let kill = source(vec![LootItem { item_id: 1, quantity: 1 }]);

        assert!(call(&kill, &ge_prices, "kill").is_some());
        assert!(call(&kill, &ge_prices, "chest").is_none());
    }

    #[test]
    fn keywords_combine_with_and_grouping() {
        let ge_prices = crate::models::GEPrices::from([(11286, 5_000_000), (383, 100)]);
        let event = vorkath_source(vec![
            LootItem { item_id: 11286, quantity: 1 },
            LootItem { item_id: 383, quantity: 1 },
        ]);

        assert!(call(&event, &ge_prices, "vorkath && unique").is_some());
        assert!(call(&event, &ge_prices, "zulrah && unique").is_none());
    }

    #[test]
    fn plain_spaces_within_a_name_are_not_torn_apart_by_and_grouping() {
        // Multi-word NPC name search must keep working as an OR-of-words single group, not be
        // sliced into per-word AND'd groups.
        let source = LootSourceEvent {
            source_name: "General Graardor".to_string(),
            source_type: "kill",
            clue_tier: None,
            loot: vec![LootItem { item_id: 1, quantity: 1 }],
        };
        let ge_prices = crate::models::GEPrices::from([(1, 10)]);

        assert!(call(&source, &ge_prices, "general graardor").is_some());
    }
}

fn default_loot_log_limit() -> i64 {
    25
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetLootLogQuery {
    #[serde(default)]
    pub before: Option<DateTime<Utc>>,
    #[serde(default = "default_loot_log_limit")]
    pub limit: i64,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub item_ids: Option<String>,
    /// Comma-separated subset of "boss","other","chest","clue" - see [`LootLogCategories`].
    #[serde(default)]
    pub categories: Option<String>,
    /// The viewer's `Date.prototype.getTimezoneOffset()` (minutes local time is behind UTC,
    /// positive west of UTC) - plumbing only, never shown in any UI. Needed to resolve calendar-
    /// relative search tokens like "today"/"this week" (see `loot_log_search::extract_date_clauses`)
    /// against the viewer's own calendar day rather than the server's, since the search string
    /// itself carries no timezone info. Defaults to 0 (UTC) when the client omits it.
    #[serde(default)]
    pub tz_offset_minutes: i32,
}

// Prevents a search that matches almost nothing in a huge history from doing unbounded work in
// one request - the client just resumes from `next_before` on its next page request instead.
const LOOT_LOG_SCAN_CAP: i64 = 5000;
const LOOT_LOG_BATCH_SIZE: i64 = 200;

/// `Utc::now()` rounded down to the minute - used as the "now" reference for resolving relative
/// date search clauses ("today", "this hour", "last 3 hours", ...) so that requests landing in
/// the same minute resolve to the same window and can share `get_loot_log`/`get_loot_log_summary`'s
/// Redis cache entry. Caps how stale a rolling window ("this hour", "last N hours") can appear
/// (up to ~60s) against real-time - well under the loot-log page's own 15s poll interval, so it's
/// not user-visible in practice.
fn now_rounded_to_minute() -> DateTime<Utc> {
    let now = Utc::now();
    now - chrono::Duration::seconds(now.timestamp() % 60)
}

/// Extracts and AND-intersects every date clause across all of a search's AND-groups (see
/// `extract_date_clauses`), returning the combined bounds plus the groups with date tokens
/// stripped out - a group left empty by extraction is dropped entirely, imposing no further
/// per-event text constraint (mirrors `extract_kill_count_clause`'s bare-clause carve-out, since
/// the date bound itself already fully governs which events are in scope via the scan cursor).
fn extract_search_date_bounds(
    groups: Vec<Vec<String>>,
    now_utc: DateTime<Utc>,
    tz_offset_minutes: i32,
) -> (DateBounds, Vec<Vec<String>>) {
    let mut combined = DateBounds::default();
    let mut remaining_groups = Vec::with_capacity(groups.len());
    for group in groups {
        let (bounds, rest) = extract_date_clauses(&group, now_utc, tz_offset_minutes);
        combined = combined.intersect(bounds);
        if !rest.is_empty() {
            remaining_groups.push(rest);
        }
    }
    (combined, remaining_groups)
}

/// Paginated, newest-first, one-row-per-raw-event loot log. Unlike the deleted `get_loot_summary`
/// (which pre-aggregated into one row per (member, source, item) across the whole unbounded
/// scope), this returns each underlying `kill`/`loot` event individually so the client can derive
/// 45-minute farming-session entries from real per-event timestamps - aggregating first would
/// make both the session grouping and the pagination cursor meaningless.
pub async fn get_loot_log(
    auth: Authenticated,
    db_pool: web::Data<Pool>,
    redis: web::Data<RedisCache>,
    query: web::Query<GetLootLogQuery>,
) -> Result<web::Json<LootLogPage>, Error> {
    let limit = query.limit.clamp(1, 100);
    let now_utc = now_rounded_to_minute();
    let version = redis.get_i64(&loot_log_version_key(auth.group_id)).await;
    let cache_key = format!(
        "v1:loot-log:{}:{}:{}:{}:{}:{}:{}:{}:{}",
        auth.group_id,
        version,
        query.before.map(|d| d.to_rfc3339()).unwrap_or_default(),
        limit,
        query.search.as_deref().unwrap_or(""),
        query.item_ids.as_deref().unwrap_or(""),
        query.categories.as_deref().unwrap_or(""),
        query.tz_offset_minutes,
        now_utc.to_rfc3339(),
    );
    if let Some(cached) = redis.get_json::<LootLogPage>(&cache_key).await {
        return Ok(web::Json(cached));
    }

    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let item_ids = parse_item_ids(query.item_ids.as_deref());
    let search_active = query.search.as_deref().is_some_and(|s| !s.trim().is_empty()) || !item_ids.is_empty();
    let (date_bounds, groups) =
        extract_search_date_bounds(split_search_groups(query.search.as_deref().unwrap_or("")), now_utc, query.tz_offset_minutes);
    let categories = LootLogCategories::parse(query.categories.as_deref());
    let ge_prices = get_ge_prices_map();

    let mut events: Vec<LootLogEvent> = Vec::new();
    let mut cursor = match (query.before, date_bounds.before) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let mut scanned: i64 = 0;
    let mut scan_exhausted = false;
    let mut next_before = cursor;

    'scan: loop {
        let batch = db::list_loot_and_kill_events_page(&client, auth.group_id, cursor, LOOT_LOG_BATCH_SIZE).await?;
        if batch.is_empty() {
            scan_exhausted = true;
            break;
        }
        let batch_len = batch.len() as i64;
        for row in &batch {
            // Rows arrive newest-first, so the moment one falls below the search's date-clause
            // lower bound, every row from here on (this batch and any further page) is too -
            // the same "provably no more" reasoning as an empty batch, just mid-batch.
            if date_bounds.after.is_some_and(|after| row.occurred_at < after) {
                scan_exhausted = true;
                break 'scan;
            }
            scanned += 1;
            next_before = Some(row.occurred_at);
            let Ok(parsed) = serde_json::from_value::<GameEvent>(row.payload.clone()) else {
                continue;
            };
            let Some(source) = as_loot_source_event(&parsed) else {
                continue;
            };
            if is_diagnostic_source(&source.source_name)
                || is_excluded_loot_log_source(&source.source_name)
                || source.loot.is_empty()
            {
                continue;
            }
            if !categories.matches(&source) {
                continue;
            }
            if let Some(log_event) = build_matching_loot_log_event(
                &source,
                &row.member_name,
                row.occurred_at,
                &ge_prices,
                &groups,
                &item_ids,
                search_active,
            ) {
                events.push(log_event);
                if events.len() as i64 >= limit {
                    break 'scan;
                }
            }
            if scanned >= LOOT_LOG_SCAN_CAP {
                break 'scan;
            }
        }
        if batch_len < LOOT_LOG_BATCH_SIZE {
            scan_exhausted = true;
            break;
        }
        cursor = next_before;
    }

    let page = LootLogPage {
        events,
        next_before: if scan_exhausted { None } else { next_before },
        scan_exhausted,
    };
    redis.set_json(&cache_key, &page, LOOT_LOG_CACHE_TTL_SECS).await;
    Ok(web::Json(page))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetLootLogSummaryQuery {
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub item_ids: Option<String>,
    /// Comma-separated subset of "boss","other","chest","clue" - see [`LootLogCategories`].
    #[serde(default)]
    pub categories: Option<String>,
    /// See `GetLootLogQuery::tz_offset_minutes` - same plumbing, same reason.
    #[serde(default)]
    pub tz_offset_minutes: i32,
}

const LOOT_LOG_SUMMARY_BATCH_SIZE: i64 = 500;

/// All-time (or all-matching-search) totals for the loot log's summary bar - a full unbounded
/// scan of the group's history, applying the same search filter as `get_loot_log` so an empty
/// search matches its all-time totals and a search narrows both consistently. Intentionally
/// unbounded (unlike `get_loot_log`'s scan cap): this only accumulates two numbers rather than
/// building/serializing full row objects, so the cost stays acceptable even over a long history.
pub async fn get_loot_log_summary(
    auth: Authenticated,
    db_pool: web::Data<Pool>,
    redis: web::Data<RedisCache>,
    query: web::Query<GetLootLogSummaryQuery>,
) -> Result<web::Json<LootLogSummary>, Error> {
    let now_utc = now_rounded_to_minute();
    let cache_key = format!(
        "v1:loot-log-summary:{}:{}:{}:{}:{}:{}",
        auth.group_id,
        query.search.as_deref().unwrap_or(""),
        query.item_ids.as_deref().unwrap_or(""),
        query.categories.as_deref().unwrap_or(""),
        query.tz_offset_minutes,
        now_utc.to_rfc3339(),
    );
    if let Some(cached) = redis.get_json::<LootLogSummary>(&cache_key).await {
        return Ok(web::Json(cached));
    }
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let item_ids = parse_item_ids(query.item_ids.as_deref());
    let search_active = query.search.as_deref().is_some_and(|s| !s.trim().is_empty()) || !item_ids.is_empty();
    let (date_bounds, groups) =
        extract_search_date_bounds(split_search_groups(query.search.as_deref().unwrap_or("")), now_utc, query.tz_offset_minutes);
    let categories = LootLogCategories::parse(query.categories.as_deref());
    let ge_prices = get_ge_prices_map();

    let mut total_value: i64 = 0;
    let mut event_count: i64 = 0;
    let mut cursor: Option<DateTime<Utc>> = date_bounds.before;
    'scan: loop {
        let batch =
            db::list_loot_and_kill_events_page(&client, auth.group_id, cursor, LOOT_LOG_SUMMARY_BATCH_SIZE).await?;
        if batch.is_empty() {
            break;
        }
        let batch_len = batch.len();
        for row in &batch {
            // See get_loot_log's matching cutoff - rows arrive newest-first, so crossing below
            // the date clause's lower bound means every remaining row is out of range too.
            if date_bounds.after.is_some_and(|after| row.occurred_at < after) {
                break 'scan;
            }
            let Ok(parsed) = serde_json::from_value::<GameEvent>(row.payload.clone()) else {
                continue;
            };
            let Some(source) = as_loot_source_event(&parsed) else {
                continue;
            };
            if is_diagnostic_source(&source.source_name)
                || is_excluded_loot_log_source(&source.source_name)
                || source.loot.is_empty()
            {
                continue;
            }
            if !categories.matches(&source) {
                continue;
            }
            if let Some(log_event) = build_matching_loot_log_event(
                &source,
                &row.member_name,
                row.occurred_at,
                &ge_prices,
                &groups,
                &item_ids,
                search_active,
            ) {
                event_count += 1;
                total_value += log_event.items.iter().map(|item| item.total_value).sum::<i64>();
            }
        }
        if batch_len < LOOT_LOG_SUMMARY_BATCH_SIZE as usize {
            break;
        }
        cursor = Some(batch.last().unwrap().occurred_at);
    }

    let summary = LootLogSummary {
        total_value,
        event_count,
    };
    redis.set_json(&cache_key, &summary, CACHE_TTL_SECS).await;
    Ok(web::Json(summary))
}

fn default_slayer_task_history_page() -> i64 {
    1
}
fn default_slayer_task_history_page_size() -> i64 {
    5
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetSlayerTaskHistoryQuery {
    pub player_name: String,
    #[serde(default = "default_slayer_task_history_page")]
    pub page: i64,
    #[serde(default = "default_slayer_task_history_page_size")]
    pub page_size: i64,
    #[serde(default)]
    pub status: Option<String>,
    /// Comma-separated exact `master_name` values - the site's master filter dropdown groups
    /// NPCs that are the same slayer master role (Nieve/Steve, Duradel/Kuradal, ...) into one
    /// option, so a single group can mean several underlying names. See
    /// `db::list_slayer_task_history_page`.
    #[serde(default)]
    pub master_name: Option<String>,
}

/// Paginated, newest-first slayer task history for one group member - the slayer panel's
/// History tab. No caching (unlike `get_loot_log`): task closes are rare enough per member that
/// a plain indexed query is cheap, and it avoids having to wire up a version-counter bump
/// alongside `upsert_slayer_task_history_event` just to keep a cache from going stale.
pub async fn get_slayer_task_history(
    auth: Authenticated,
    db_pool: web::Data<Pool>,
    query: web::Query<GetSlayerTaskHistoryQuery>,
) -> Result<web::Json<SlayerTaskHistoryPage>, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let master_names: Option<Vec<String>> = query.master_name.as_deref().map(|names| {
        names
            .split(',')
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect()
    });
    let page = db::list_slayer_task_history_page(
        &client,
        auth.group_id,
        &query.player_name,
        query.page,
        query.page_size,
        query.status.as_deref(),
        master_names.as_deref(),
    )
    .await?;
    Ok(web::Json(page))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetSlayerTaskStatsQuery {
    pub player_name: String,
}

/// All-time slayer task stats for one group member - the slayer panel's Stats tab.
pub async fn get_slayer_task_stats(
    auth: Authenticated,
    db_pool: web::Data<Pool>,
    query: web::Query<GetSlayerTaskStatsQuery>,
) -> Result<web::Json<SlayerTaskStats>, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let stats = db::get_slayer_task_stats(&client, auth.group_id, &query.player_name).await?;
    Ok(web::Json(stats))
}

#[derive(Deserialize, Debug)]
pub enum SkillDataPeriod {
    Hour1,
    Hour6,
    Hour12,
    Day,
    Week,
    Month,
    Year,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetSkillDataQuery {
    pub period: SkillDataPeriod,
}
pub async fn get_skill_data(
    auth: Authenticated,
    db_pool: web::Data<Pool>,
    query: web::Query<GetSkillDataQuery>,
) -> Result<web::Json<GroupSkillData>, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let aggregate_period = match query.period {
        SkillDataPeriod::Hour1 => db::AggregatePeriod::Day,
        SkillDataPeriod::Hour6 => db::AggregatePeriod::Day,
        SkillDataPeriod::Hour12 => db::AggregatePeriod::Day,
        SkillDataPeriod::Day => db::AggregatePeriod::Day,
        SkillDataPeriod::Week => db::AggregatePeriod::Month,
        SkillDataPeriod::Month => db::AggregatePeriod::Month,
        SkillDataPeriod::Year => db::AggregatePeriod::Year,
    };
    let group_skill_data =
        db::get_skills_for_period(&client, auth.group_id, aggregate_period).await?;
    Ok(web::Json(group_skill_data))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetLeaderboardQuery {
    pub metric: LeaderboardMetric,
    pub window: LeaderboardWindow,
    #[serde(default)]
    pub boss: Option<String>,
    /// Only meaningful when `metric == Xp`. A `SkillName` string exactly as the client sends it
    /// (e.g. `"Woodcutting"`, `"Overall"`), or omitted for "Overall". See
    /// `db::skill_array_index`'s doc comment for how this is resolved.
    #[serde(default)]
    pub skill: Option<String>,
    /// Only meaningful when `metric == RaidCompletions`. `"cox"`/`"tob"`/`"toa"`, or omitted/`"all"`
    /// for every raid combined.
    #[serde(default)]
    pub raid_type: Option<String>,
    /// Only meaningful when `metric == RaidCompletions`. `"regular"`/`"cm"` (CoX) or
    /// `"entry"`/`"normal"`/`"expert"` (ToA); ignored for ToB, which has no difficulty split.
    /// Omitted/`"all"` for every difficulty combined.
    #[serde(default)]
    pub raid_difficulty: Option<String>,
}
/// XP/boss-KC/GP-earned/loot-value rankings over a daily/weekly/all-time window - part of the
/// Graphs tab rather than a standalone leaderboards page/feature (see [`crate::leaderboard`]).
pub async fn get_leaderboard(
    auth: Authenticated,
    db_pool: web::Data<Pool>,
    redis: web::Data<RedisCache>,
    query: web::Query<GetLeaderboardQuery>,
) -> Result<web::Json<LeaderboardResult>, Error> {
    let cache_key = format!(
        "v1:leaderboard:{}:{:?}:{:?}:{}:{}:{}:{}",
        auth.group_id,
        query.metric,
        query.window,
        query.boss.as_deref().unwrap_or(""),
        query.skill.as_deref().unwrap_or(""),
        query.raid_type.as_deref().unwrap_or(""),
        query.raid_difficulty.as_deref().unwrap_or(""),
    );
    if let Some(cached) = redis.get_json::<LeaderboardResult>(&cache_key).await {
        return Ok(web::Json(cached));
    }
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let now = Utc::now();

    let (entries, available_bosses) = match query.metric {
        LeaderboardMetric::Xp => {
            let raw = db::get_xp_leaderboard(
                &client,
                auth.group_id,
                query.window,
                query.skill.as_deref(),
                now,
            )
            .await?;
            (crate::leaderboard::rank_entries(raw), vec![])
        }
        LeaderboardMetric::BossKc => {
            let (raw, bosses) = db::get_boss_kc_leaderboard(
                &client,
                auth.group_id,
                query.window,
                query.boss.as_deref(),
                now,
            )
            .await?;
            (crate::leaderboard::rank_entries(raw), bosses)
        }
        LeaderboardMetric::GpEarned => {
            let ge_prices = get_ge_prices_map();
            let raw = db::get_gp_earned_leaderboard(
                &client,
                auth.group_id,
                query.window,
                now,
                &ge_prices,
            )
            .await?;
            (crate::leaderboard::rank_entries(raw), vec![])
        }
        LeaderboardMetric::RaidCompletions => {
            let raw = db::get_raid_completions_leaderboard(
                &client,
                auth.group_id,
                query.window,
                query.raid_type.as_deref(),
                query.raid_difficulty.as_deref(),
                now,
            )
            .await?;
            (crate::leaderboard::rank_entries(raw), vec![])
        }
    };

    let result = LeaderboardResult {
        metric: query.metric,
        window: query.window,
        boss: query.boss.clone(),
        available_bosses,
        entries,
    };
    redis.set_json(&cache_key, &result, CACHE_TTL_SECS).await;
    Ok(web::Json(result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetMetricDataQuery {
    pub metric: LeaderboardMetric,
    pub period: SkillDataPeriod,
    /// Only used when `metric == BossKc`. `None` means "All Bosses" (every kill counted), same
    /// as `get_boss_kc_leaderboard`'s existing `boss: None` semantics.
    #[serde(default)]
    pub boss: Option<String>,
    /// Only used when `metric == RaidCompletions` - see `GetLeaderboardQuery::raid_type`.
    #[serde(default)]
    pub raid_type: Option<String>,
    /// Only used when `metric == RaidCompletions` - see `GetLeaderboardQuery::raid_difficulty`.
    #[serde(default)]
    pub raid_difficulty: Option<String>,
    /// Only used when `metric == RaidCompletions`. `"member"` credits every participant
    /// separately (one line per member); omitted/anything else collapses to one aggregate
    /// "Group Total" line - see `db::get_raid_completions_metric_data`'s `group_by_member`.
    #[serde(default)]
    pub group_by: Option<String>,
}
/// Graphs tab chart data for boss-KC/GP-earned, at the period's bucket granularity,
/// as running cumulative totals (not pre-diffed - the client turns a cumulative series into
/// "gained since period start" itself, mirroring how it already does that for skill XP). XP has
/// its own richer endpoint (`get_skill_data`) and is rejected here.
pub async fn get_metric_data(
    auth: Authenticated,
    db_pool: web::Data<Pool>,
    redis: web::Data<RedisCache>,
    query: web::Query<GetMetricDataQuery>,
) -> Result<web::Json<GroupMetricData>, Error> {
    if query.metric == LeaderboardMetric::Xp {
        return Err(ApiError::MetricDataXpNotSupportedError.into());
    }

    let cache_key = format!(
        "v1:metric-data:{}:{:?}:{:?}:{}:{}:{}:{}",
        auth.group_id,
        query.metric,
        query.period,
        query.boss.as_deref().unwrap_or(""),
        query.raid_type.as_deref().unwrap_or(""),
        query.raid_difficulty.as_deref().unwrap_or(""),
        query.group_by.as_deref().unwrap_or(""),
    );
    if let Some(cached) = redis.get_json::<GroupMetricData>(&cache_key).await {
        return Ok(web::Json(cached));
    }

    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let aggregate_period = match query.period {
        SkillDataPeriod::Hour1 => db::AggregatePeriod::Day,
        SkillDataPeriod::Hour6 => db::AggregatePeriod::Day,
        SkillDataPeriod::Hour12 => db::AggregatePeriod::Day,
        SkillDataPeriod::Day => db::AggregatePeriod::Day,
        SkillDataPeriod::Week => db::AggregatePeriod::Month,
        SkillDataPeriod::Month => db::AggregatePeriod::Month,
        SkillDataPeriod::Year => db::AggregatePeriod::Year,
    };

    let data = match query.metric {
        LeaderboardMetric::Xp => unreachable!("rejected above"),
        LeaderboardMetric::BossKc => {
            db::get_boss_kc_metric_data(
                &client,
                auth.group_id,
                aggregate_period,
                query.boss.as_deref(),
            )
            .await?
        }
        LeaderboardMetric::GpEarned => {
            db::get_bank_value_for_period(&client, auth.group_id, aggregate_period).await?
        }
        LeaderboardMetric::RaidCompletions => {
            db::get_raid_completions_metric_data(
                &client,
                auth.group_id,
                aggregate_period,
                query.raid_type.as_deref(),
                query.raid_difficulty.as_deref(),
                query.group_by.as_deref() == Some("member"),
            )
            .await?
        }
    };

    redis.set_json(&cache_key, &data, CACHE_TTL_SECS).await;
    Ok(web::Json(data))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetItemBonusesQuery {
    pub item_id: i32,
}
/// Equipment bonuses for a single item id (equip-screen Attack/Defence/Other-bonuses columns) -
/// global reference data, not group-scoped, but still requires the same group/character auth as
/// every other endpoint (see main.rs's registration under both scopes). `Authenticated` is
/// required purely as an auth gate here; `auth.group_id` is unused.
pub async fn get_item_bonuses(
    _auth: Authenticated,
    db_pool: web::Data<Pool>,
    query: web::Query<GetItemBonusesQuery>,
) -> Result<web::Json<ItemBonusesResponse>, Error> {
    let bonuses = item_bonuses::get_item_bonuses(db_pool.get_ref(), query.item_id).await?;
    Ok(web::Json(bonuses))
}

pub async fn am_i_logged_in(_auth: Authenticated) -> Result<HttpResponse, Error> {
    Ok(HttpResponse::Ok().finish())
}

pub async fn am_i_in_group(
    auth: Authenticated,
    db_pool: web::Data<Pool>,
    q: web::Query<AmIInGroupRequest>,
) -> Result<HttpResponse, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let in_group: bool = db::is_member_in_group(&client, auth.group_id, &q.member_name).await?;

    if !in_group {
        return Ok(HttpResponse::Unauthorized().body("Player is not a member of this group"));
    }
    Ok(HttpResponse::Ok().finish())
}

/// Character-scoped counterpart to `am_i_in_group`, mounted on `/api/characters/{account_hash}`
/// instead of `/api/group/{group_name}`. The group-token version above has to check `members`
/// for the *named* row because any client holding the group token can ask about any member
/// name - that's the whole point of the check there. Here, `grouped_character_middleware`
/// already 403s unless this character is linked to a group, so reaching this handler at all is
/// the answer. Checking `members` again would recreate the same bootstrap deadlock the plugin
/// used to hit: a character's `members` row is only ever created as a side effect of its first
/// successful `update-group-member` call, but the plugin gates that very first call behind this
/// endpoint returning success - so a freshly linked character could never pass the check that
/// was supposed to let it start sending the update that creates the row.
pub async fn am_i_in_group_for_character(_auth: Authenticated) -> Result<HttpResponse, Error> {
    Ok(HttpResponse::Ok().finish())
}

pub async fn get_collection_log() -> Result<web::Json<HashMap<String, Vec<i32>>>, Error> {
    Ok(web::Json(HashMap::new()))
}

// Not decorated with #[post(...)] - registered manually in main.rs with its own
// larger PayloadConfig since the global 100KB cap rejects real meshes.
pub async fn update_portrait(
    auth: Authenticated,
    // Two dynamic segments are live here: {group_name} from the outer scope plus
    // {member_name} from this route. Path<String> only tolerates a single segment
    // and 404s ("wrong number of parameters: 2 expected 1") otherwise.
    path: web::Path<(String, String)>,
    body: web::Bytes,
    db_pool: web::Data<Pool>,
) -> Result<HttpResponse, Error> {
    crate::demo::reject_if_demo(auth.group_id)?;
    let (_group_name, member_name) = path.into_inner();
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let member_id = db::get_member_id(&client, auth.group_id, &member_name).await?;
    db::upsert_member_mesh(&client, member_id, &body).await?;
    Ok(HttpResponse::Ok().finish())
}

#[get("/portrait/{member_name}")]
pub async fn get_portrait(
    auth: Authenticated,
    path: web::Path<(String, String)>,
    db_pool: web::Data<Pool>,
) -> Result<HttpResponse, Error> {
    let (_group_name, member_name) = path.into_inner();
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    let mesh = db::get_member_mesh(&client, auth.group_id, &member_name).await?;
    match mesh {
        Some(mesh) => Ok(HttpResponse::Ok()
            .append_header(("Cache-Control", "private, max-age=60"))
            .content_type("application/octet-stream")
            .body(mesh)),
        None => Ok(HttpResponse::NotFound().finish()),
    }
}

/// Live OSRS hiscores boss/clue/minigame kill counts for a group member (the `boss-kc-panel`
/// frontend tab) - fetched fresh from Jagex every call (see `hiscores::get_boss_kc`'s doc
/// comment for the short in-flight dedupe that protects against a request storm), not
/// GroupScape's own self-tracked kill-count system. `member_name` doubles as the account's RSN
/// (see `groupscape.members`' doc comment / `db::is_member_in_group`), so no extra RSN lookup is
/// needed beyond confirming the name belongs to this group.
#[get("/boss-kc/{member_name}")]
pub async fn get_boss_kc(
    auth: Authenticated,
    path: web::Path<(String, String)>,
    db_pool: web::Data<Pool>,
) -> Result<HttpResponse, Error> {
    let (_group_name, member_name) = path.into_inner();
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    if !db::is_member_in_group(&client, auth.group_id, &member_name).await? {
        return Ok(HttpResponse::NotFound().json(serde_json::json!({
            "kind": "not_found",
            "message": "That member is not in this group",
        })));
    }

    match hiscores::get_boss_kc(&member_name).await {
        Ok(response) => Ok(HttpResponse::Ok().json(response)),
        Err(hiscores::HiscoresError::NotFound) => Ok(HttpResponse::NotFound().json(serde_json::json!({
            "kind": "not_found",
            "message": "Hiscores profile not found - it may be private, or the account may not exist",
        }))),
        Err(hiscores::HiscoresError::Unavailable(reason)) => {
            log::warn!("hiscores fetch failed for {member_name}: {reason}");
            Ok(HttpResponse::BadGateway().json(serde_json::json!({
                "kind": "unavailable",
                "message": "OSRS hiscores are temporarily unavailable",
            })))
        }
    }
}

/// Mounted under `character_identity_scope` (no group required) - the plugin calls this once
/// per reconcile cycle regardless of confirm/group status, so a pending character's real RSN
/// reaches the site's confirm card instead of the account_hash placeholder it was created with.
pub async fn identify_character(
    identity: CharacterIdentified,
    body: web::Json<IdentifyCharacter>,
    db_pool: web::Data<Pool>,
) -> Result<HttpResponse, Error> {
    let rsn = body.rsn.trim().to_string();
    if !valid_name(&rsn) {
        return Ok(HttpResponse::BadRequest().body("Provided RSN is not valid"));
    }
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    db::update_character_display_rsn(
        &client,
        identity.character_id,
        &rsn,
        body.combat_level,
        body.total_level,
    )
    .await?;
    Ok(HttpResponse::Ok().finish())
}

/// Mounted under `character_identity_scope` (no group required) - portrait meshes are keyed by
/// `character_id` (`character_mesh`), not `(group_id, member_name)`, so a pending character can
/// have a live 3D render on its confirm card before it ever joins a group.
// Not decorated with #[post(...)] - registered manually in main.rs with its own larger
// PayloadConfig since the global 100KB cap rejects real meshes.
pub async fn update_character_portrait(
    identity: CharacterIdentified,
    // {account_hash} is consumed by the middleware; {member_name} is unused (kept only so the
    // plugin's URL shape mirrors the old `/update-portrait/{member_name}` route).
    _path: web::Path<(String, String)>,
    body: web::Bytes,
    db_pool: web::Data<Pool>,
) -> Result<HttpResponse, Error> {
    let client: Client = db_pool.get().await.map_err(ApiError::PoolError)?;
    db::upsert_character_mesh(&client, identity.character_id, &body).await?;
    Ok(HttpResponse::Ok().finish())
}
