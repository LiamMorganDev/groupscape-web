use crate::crypto::token_hash;
use crate::drop_rates::slugify_npc_name;
use crate::error::ApiError;
use crate::slayer_boss_tasks;
use crate::models::{
    ActivityEvent, AdminAccountCharacter, AdminAccountDetail, AdminAccountGroup,
    AdminAccountSession, AdminAccountSummary, AdminAuditLogEntry, AdminDashboard,
    AdminGroupDetail, AdminGroupMember, AdminGroupSummary, AggregateSkillData, BlockedMember,
    normalize_interacting, CombatStyleBonuses, CreateGroup, DiscordWebhookSettings, GameEvent,
    GroupMember, GroupMemberPermissions, GroupMetricData, GroupPermissions, GroupSession,
    GroupSkillData, ItemBonusesResponse, MemberMetricData, MemberSkillData, MetricDataPoint,
    PermissionFlags, PermissionFlagsPatch, PermissionKey, RaidCompletionPayload, RaidDifficulty,
    RaidType, SlayerTask, SlayerTaskHistoryEntry, SlayerTaskHistoryEvent, SlayerTaskHistoryPage,
    SlayerModifierLeader, SlayerTaskDurationLeader, SlayerTaskLeader, SlayerTaskStats,
    MEMBER_COLOR_PALETTE, RAID_GROUP_TOTAL_LABEL, SHARED_MEMBER,
};
use crate::validators::valid_name;
use chrono::{DateTime, Utc};
use deadpool_postgres::{Client, Transaction};
use rand_core::{OsRng, RngCore};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use tokio_postgres::Row;

const CURRENT_GROUP_VERSION: i32 = 2;

/// Regular-user login lockout: after this many consecutive bad passwords, the account is
/// locked out for `ACCOUNT_LOCKOUT_MINUTES`. Mirrors the shape (not the numbers) of the
/// existing admin-token rate limiter in `admin_auth_middleware.rs`.
const ACCOUNT_LOCKOUT_THRESHOLD: i32 = 5;
const ACCOUNT_LOCKOUT_MINUTES: i32 = 15;
pub async fn create_group(client: &mut Client, create_group: &CreateGroup) -> Result<(), ApiError> {
    let create_group_stmt = client.prepare_cached("INSERT INTO groupscape.groups (group_name, group_token_hash, version) VALUES($1, $2, $3) RETURNING group_id").await?;
    let create_member_stmt = client
        .prepare_cached(
            "INSERT INTO groupscape.members (group_id, member_name, color) VALUES($1, $2, $3)",
        )
        .await?;
    let transaction = client.transaction().await?;

    let hashed_token = token_hash(&create_group.token, &create_group.name);
    let group_id: i64 = transaction
        .query_one(
            &create_group_stmt,
            &[&create_group.name, &hashed_token, &CURRENT_GROUP_VERSION],
        )
        .await?
        .try_get(0)
        .map_err(ApiError::GroupCreationError)?;

    transaction
        .execute(
            &create_member_stmt,
            &[&group_id, &SHARED_MEMBER, &Option::<String>::None],
        )
        .await
        .map_err(ApiError::GroupCreationError)?;
    let colors = shuffled_color_palette();
    for (index, member_name) in create_group.member_names.iter().enumerate() {
        let color = colors[index % colors.len()];
        transaction
            .execute(&create_member_stmt, &[&group_id, &member_name, &color])
            .await
            .map_err(ApiError::GroupCreationError)?;
    }

    transaction
        .commit()
        .await
        .map_err(ApiError::GroupCreationError)
}

/// Fisher-Yates shuffle of [`MEMBER_COLOR_PALETTE`], for assigning distinct colours to a batch
/// of brand-new members (e.g. `create_group`) without a DB round-trip - a new group has no
/// existing colour rows to collide with.
fn shuffled_color_palette() -> Vec<&'static str> {
    let mut palette = MEMBER_COLOR_PALETTE.to_vec();
    for i in (1..palette.len()).rev() {
        let j = (OsRng.next_u32() as usize) % (i + 1);
        palette.swap(i, j);
    }
    palette
}

/// Picks a random colour from [`MEMBER_COLOR_PALETTE`] not already assigned to another member
/// of this group, for a single new member joining an existing group. Falls back to a uniformly
/// random colour from the full palette once every colour is taken (§ "helmet colour" ticket:
/// duplicates are an accepted fallback over leaving a member uncoloured).
async fn pick_unused_member_color(client: &Client, group_id: i64) -> Result<String, ApiError> {
    let stmt = client
        .prepare_cached("SELECT color FROM groupscape.members WHERE group_id=$1 AND color IS NOT NULL")
        .await?;
    let rows = client
        .query(&stmt, &[&group_id])
        .await
        .map_err(ApiError::GetMemberColorsError)?;
    let used: HashSet<String> = rows
        .iter()
        .map(|row| row.try_get::<_, String>(0))
        .collect::<Result<_, _>>()?;

    let unused: Vec<&str> = MEMBER_COLOR_PALETTE
        .iter()
        .filter(|color| !used.contains(**color))
        .copied()
        .collect();
    let pool: &[&str] = if unused.is_empty() {
        &MEMBER_COLOR_PALETTE
    } else {
        &unused
    };
    let index = (OsRng.next_u32() as usize) % pool.len();
    Ok(pool[index].to_string())
}

pub async fn add_group_member(
    client: &Client,
    group_id: i64,
    member_name: &str,
) -> Result<(), ApiError> {
    let member_count_stmt = client
        .prepare_cached(
            "SELECT COUNT(*) FROM groupscape.members WHERE group_id=$1 AND member_name!=$2",
        )
        .await?;
    let member_count: i64 = client
        .query_one(&member_count_stmt, &[&group_id, &SHARED_MEMBER])
        .await?
        .try_get(0)
        .map_err(ApiError::AddMemberError)?;

    if member_count >= 5 {
        return Err(ApiError::GroupFullError);
    }

    let color = pick_unused_member_color(client, group_id).await?;
    let create_member_stmt = client
        .prepare_cached(
            "INSERT INTO groupscape.members (group_id, member_name, color) VALUES($1, $2, $3)",
        )
        .await?;
    client
        .execute(&create_member_stmt, &[&group_id, &member_name, &color])
        .await
        .map_err(ApiError::AddMemberError)?;
    Ok(())
}

/// Resolves the canonical member row for a plugin-submitted `account_hash`, ported from
/// `groupscape-old`'s telemetry-ingestion pattern (`account_hash` is the persistent identity,
/// `display_rsn`/`member_name` is just a mutable label kept in sync with it) and adapted to this
/// schema's separate `members` table (`characters.display_rsn` is the source of truth; `members`
/// gains its own `account_hash` column so a row survives an in-game name change instead of being
/// orphaned under the old name).
///
/// Lookup order:
/// 1. A member row already tagged with this `account_hash` in this group - rename it in place if
///    `display_rsn` has since changed.
/// 2. A pre-existing untagged row matching `display_rsn` by name (e.g. typed in at group setup
///    before this character was linked) - claim it by tagging its `account_hash`.
/// 3. Neither exists - insert a new row, subject to the same 5-member cap as `add_group_member`.
///
/// Returns the member name the caller should use for this update (== `display_rsn`).
pub async fn ensure_member_for_linked_character(
    client: &Client,
    group_id: i64,
    account_hash: &str,
    display_rsn: &str,
) -> Result<String, ApiError> {
    let by_hash_stmt = client
        .prepare_cached(
            "SELECT member_name FROM groupscape.members WHERE group_id=$1 AND account_hash=$2",
        )
        .await?;
    if let Some(row) = client
        .query_opt(&by_hash_stmt, &[&group_id, &account_hash])
        .await
        .map_err(ApiError::AddMemberError)?
    {
        let existing_name: String = row.try_get("member_name")?;
        if existing_name == display_rsn {
            return Ok(existing_name);
        }
        let rename_stmt = client
            .prepare_cached(
                "UPDATE groupscape.members SET member_name=$1 WHERE group_id=$2 AND account_hash=$3",
            )
            .await?;
        return match client
            .execute(&rename_stmt, &[&display_rsn, &group_id, &account_hash])
            .await
        {
            Ok(_) => Ok(display_rsn.to_string()),
            // member_name collision with an unrelated row (e.g. a stale untagged member still
            // squatting on the new name) - keep serving the old name rather than failing the
            // telemetry update outright.
            Err(_) => Ok(existing_name),
        };
    }

    // Resets the progress-snapshot columns along with tagging account_hash: this row may have
    // been ticking along for a while under the legacy unauthenticated/name-matched heartbeat path
    // (see the `linked_to_this_group` comment in authed.rs), so whatever it's already carrying
    // isn't a meaningful "previous" to diff the newly-linked character's first heartbeat against.
    // Nulling them makes that first heartbeat behave like any other member's first-ever upload -
    // `progress_events::diff_progress` skips a `None` field entirely - so linking establishes a
    // fresh baseline instead of replaying the account's whole milestone history at once.
    let claim_stmt = client
        .prepare_cached(
            "UPDATE groupscape.members SET account_hash=$1, \
             skills=NULL, skills_last_update=NULL, \
             quests=NULL, quests_last_update=NULL, \
             diary_vars=NULL, diary_vars_last_update=NULL, \
             collection_log=NULL, collection_log_last_update=NULL, \
             combat_achievements=NULL, combat_achievements_last_update=NULL \
             WHERE group_id=$2 AND member_name=$3 AND account_hash IS NULL",
        )
        .await?;
    let claimed = client
        .execute(&claim_stmt, &[&account_hash, &group_id, &display_rsn])
        .await
        .map_err(ApiError::AddMemberError)?;
    if claimed > 0 {
        return Ok(display_rsn.to_string());
    }

    let member_count_stmt = client
        .prepare_cached(
            "SELECT COUNT(*) FROM groupscape.members WHERE group_id=$1 AND member_name!=$2",
        )
        .await?;
    let member_count: i64 = client
        .query_one(&member_count_stmt, &[&group_id, &SHARED_MEMBER])
        .await?
        .try_get(0)
        .map_err(ApiError::AddMemberError)?;
    if member_count >= 5 {
        return Err(ApiError::GroupFullError);
    }

    let color = pick_unused_member_color(client, group_id).await?;
    let create_member_stmt = client
        .prepare_cached(
            "INSERT INTO groupscape.members (group_id, member_name, account_hash, color) VALUES($1, $2, $3, $4)",
        )
        .await?;
    client
        .execute(
            &create_member_stmt,
            &[&group_id, &display_rsn, &account_hash, &color],
        )
        .await
        .map_err(ApiError::AddMemberError)?;
    Ok(display_rsn.to_string())
}

pub async fn delete_skills_data_for_member(
    transaction: &Transaction<'_>,
    period: AggregatePeriod,
    member_id: i64,
) -> Result<(), ApiError> {
    let s = format!(
        r#"
DELETE FROM groupscape.skills_{} WHERE member_id=$1
"#,
        match period {
            AggregatePeriod::Day => "day",
            AggregatePeriod::Month => "month",
            AggregatePeriod::Year => "year",
        }
    );
    let delete_skills_data_stmt = transaction.prepare_cached(&s).await?;
    transaction
        .execute(&delete_skills_data_stmt, &[&member_id])
        .await?;

    Ok(())
}

pub async fn delete_bank_value_data_for_member(
    transaction: &Transaction<'_>,
    period: AggregatePeriod,
    member_id: i64,
) -> Result<(), ApiError> {
    let s = format!(
        r#"
DELETE FROM groupscape.bank_value_{} WHERE member_id=$1
"#,
        match period {
            AggregatePeriod::Day => "day",
            AggregatePeriod::Month => "month",
            AggregatePeriod::Year => "year",
        }
    );
    let delete_bank_value_stmt = transaction.prepare_cached(&s).await?;
    transaction
        .execute(&delete_bank_value_stmt, &[&member_id])
        .await?;

    Ok(())
}

pub async fn get_member_id(
    client: &Client,
    group_id: i64,
    member_name: &str,
) -> Result<i64, ApiError> {
    let get_member_id_stmt = client
        .prepare_cached(
            "SELECT member_id FROM groupscape.members WHERE group_id=$1 AND member_name=$2",
        )
        .await?;
    let member_id: i64 = client
        .query_one(&get_member_id_stmt, &[&group_id, &member_name])
        .await
        .map_err(ApiError::DeleteGroupMemberError)?
        .try_get(0)?;
    Ok(member_id)
}

pub async fn delete_group_member(
    client: &mut Client,
    group_id: i64,
    member_name: &str,
) -> Result<(), ApiError> {
    let member_id = get_member_id(client, group_id, member_name).await?;
    let transaction = client.transaction().await?;
    delete_skills_data_for_member(&transaction, AggregatePeriod::Day, member_id).await?;
    delete_skills_data_for_member(&transaction, AggregatePeriod::Month, member_id).await?;
    delete_skills_data_for_member(&transaction, AggregatePeriod::Year, member_id).await?;
    delete_bank_value_data_for_member(&transaction, AggregatePeriod::Day, member_id).await?;
    delete_bank_value_data_for_member(&transaction, AggregatePeriod::Month, member_id).await?;
    delete_bank_value_data_for_member(&transaction, AggregatePeriod::Year, member_id).await?;

    let stmt = transaction
        .prepare_cached("DELETE FROM groupscape.members WHERE group_id=$1 AND member_name=$2")
        .await?;
    transaction
        .execute(&stmt, &[&group_id, &member_name])
        .await
        .map_err(ApiError::DeleteGroupMemberError)?;

    transaction
        .commit()
        .await
        .map_err(ApiError::DeleteGroupMemberError)?;

    Ok(())
}

pub async fn is_member_blocked(
    client: &Client,
    group_id: i64,
    member_name: &str,
) -> Result<bool, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT 1 FROM groupscape.blocked_members WHERE group_id=$1 AND member_name=$2",
        )
        .await?;
    let row = client
        .query_opt(&stmt, &[&group_id, &member_name])
        .await
        .map_err(ApiError::IsMemberBlockedError)?;
    Ok(row.is_some())
}

/// Auto-provisions a member the first time their telemetry arrives - there's no manual
/// "add member" step anymore, so joining the group happens implicitly on first update.
/// A previously-removed member rejoins the same way, since removal fully deletes their row.
/// A blocked member is rejected here instead, before any data is written or broadcast.
pub async fn ensure_group_member(
    client: &Client,
    group_id: i64,
    member_name: &str,
) -> Result<(), ApiError> {
    if is_member_blocked(client, group_id, member_name).await? {
        return Err(ApiError::MemberBlockedError);
    }

    if is_member_in_group(client, group_id, member_name).await? {
        return Ok(());
    }

    if !valid_name(member_name) {
        return Err(ApiError::GroupMemberValidationError(format!(
            "Member name {} is not valid",
            member_name
        )));
    }

    add_group_member(client, group_id, member_name).await
}

pub async fn get_blocked_members(
    client: &Client,
    group_id: i64,
) -> Result<Vec<BlockedMember>, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT member_name, blocked_at FROM groupscape.blocked_members WHERE group_id=$1 ORDER BY blocked_at DESC",
        )
        .await?;
    let rows = client
        .query(&stmt, &[&group_id])
        .await
        .map_err(ApiError::GetBlockedMembersError)?;
    rows.iter()
        .map(|row| {
            Ok(BlockedMember {
                member_name: row.try_get("member_name")?,
                blocked_at: row.try_get("blocked_at")?,
            })
        })
        .collect()
}

/// Blocks a member: wipes their tracked data the same way Remove does (if they're
/// currently in the group) and records the block so future telemetry under that name is
/// rejected until unblocked.
pub async fn block_group_member(
    client: &mut Client,
    group_id: i64,
    member_name: &str,
) -> Result<(), ApiError> {
    if is_member_in_group(client, group_id, member_name).await? {
        delete_group_member(client, group_id, member_name).await?;
    }

    let stmt = client
        .prepare_cached(
            "INSERT INTO groupscape.blocked_members (group_id, member_name) VALUES ($1, $2) ON CONFLICT (group_id, member_name) DO NOTHING",
        )
        .await?;
    client
        .execute(&stmt, &[&group_id, &member_name])
        .await
        .map_err(ApiError::BlockGroupMemberError)?;
    Ok(())
}

pub async fn unblock_group_member(
    client: &Client,
    group_id: i64,
    member_name: &str,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            "DELETE FROM groupscape.blocked_members WHERE group_id=$1 AND member_name=$2",
        )
        .await?;
    client
        .execute(&stmt, &[&group_id, &member_name])
        .await
        .map_err(ApiError::UnblockGroupMemberError)?;
    Ok(())
}

pub async fn rename_group(
    client: &Client,
    group_id: i64,
    new_name: &str,
) -> Result<String, ApiError> {
    let new_token = format!("{}|{}", new_name, uuid::Uuid::new_v4().hyphenated());
    let hashed_token = token_hash(&new_token, new_name);
    let stmt = client
        .prepare_cached(
            "UPDATE groupscape.groups SET group_name=$1, group_token_hash=$2 WHERE group_id=$3",
        )
        .await?;
    client
        .execute(&stmt, &[&new_name, &hashed_token, &group_id])
        .await
        .map_err(ApiError::RenameGroupError)?;
    Ok(new_token)
}

pub async fn reroll_group_token(
    client: &Client,
    group_id: i64,
    group_name: &str,
) -> Result<String, ApiError> {
    let new_token = format!("{}|{}", group_name, uuid::Uuid::new_v4().hyphenated());
    let hashed_token = token_hash(&new_token, group_name);
    let stmt = client
        .prepare_cached("UPDATE groupscape.groups SET group_token_hash=$1 WHERE group_id=$2")
        .await?;
    client
        .execute(&stmt, &[&hashed_token, &group_id])
        .await
        .map_err(ApiError::RerollGroupTokenError)?;
    Ok(new_token)
}

/// Groups always have a row (columns default `true`/`NULL` at creation via the migration), so
/// this is a plain `query_one`, unlike `get_group_permissions`' `Option` (no row until a
/// character links in).
pub async fn get_discord_webhook_settings(
    client: &Client,
    group_id: i64,
) -> Result<DiscordWebhookSettings, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT discord_webhook_url, discord_notify_kills, discord_notify_deaths, discord_notify_drops, \
             discord_drops_min_value, discord_drops_unique_only, discord_notify_raids, discord_notify_combat_achievements, \
             discord_notify_collection_log, discord_notify_quests, discord_notify_diaries, discord_notify_pets, \
             discord_notify_clues, discord_notify_level_ups, discord_level_up_interval \
             FROM groupscape.groups WHERE group_id=$1",
        )
        .await?;
    let row = client
        .query_one(&stmt, &[&group_id])
        .await
        .map_err(ApiError::GetDiscordWebhookSettingsError)?;
    Ok(DiscordWebhookSettings {
        webhook_url: row.try_get("discord_webhook_url")?,
        notify_kills: row.try_get("discord_notify_kills")?,
        notify_deaths: row.try_get("discord_notify_deaths")?,
        notify_drops: row.try_get("discord_notify_drops")?,
        drops_min_value: row.try_get("discord_drops_min_value")?,
        drops_unique_only: row.try_get("discord_drops_unique_only")?,
        notify_raids: row.try_get("discord_notify_raids")?,
        notify_combat_achievements: row.try_get("discord_notify_combat_achievements")?,
        notify_collection_log: row.try_get("discord_notify_collection_log")?,
        notify_quests: row.try_get("discord_notify_quests")?,
        notify_diaries: row.try_get("discord_notify_diaries")?,
        notify_pets: row.try_get("discord_notify_pets")?,
        notify_clues: row.try_get("discord_notify_clues")?,
        notify_level_ups: row.try_get("discord_notify_level_ups")?,
        level_up_interval: row.try_get("discord_level_up_interval")?,
    })
}

pub async fn update_discord_webhook_settings(
    client: &Client,
    group_id: i64,
    settings: &DiscordWebhookSettings,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            "UPDATE groupscape.groups SET \
             discord_webhook_url=$2, discord_notify_kills=$3, discord_notify_deaths=$4, discord_notify_drops=$5, \
             discord_drops_min_value=$6, discord_drops_unique_only=$7, discord_notify_raids=$8, \
             discord_notify_combat_achievements=$9, discord_notify_collection_log=$10, discord_notify_quests=$11, \
             discord_notify_diaries=$12, discord_notify_pets=$13, discord_notify_clues=$14, \
             discord_notify_level_ups=$15, discord_level_up_interval=$16 \
             WHERE group_id=$1",
        )
        .await?;
    client
        .execute(
            &stmt,
            &[
                &group_id,
                &settings.webhook_url,
                &settings.notify_kills,
                &settings.notify_deaths,
                &settings.notify_drops,
                &settings.drops_min_value,
                &settings.drops_unique_only,
                &settings.notify_raids,
                &settings.notify_combat_achievements,
                &settings.notify_collection_log,
                &settings.notify_quests,
                &settings.notify_diaries,
                &settings.notify_pets,
                &settings.notify_clues,
                &settings.notify_level_ups,
                &settings.level_up_interval,
            ],
        )
        .await
        .map_err(ApiError::UpdateDiscordWebhookSettingsError)?;
    Ok(())
}

/// Total kills recorded for `member_name` against `npc_name` in this group, all-time - the
/// "(KC: n)" suffix on the Discord boss-kill message. Reuses `list_kill_events`' JSON-payload
/// filtering rather than a new SQL predicate, matching `get_boss_kc_leaderboard`'s approach.
pub async fn count_kills_for_member_npc(
    client: &Client,
    group_id: i64,
    member_name: &str,
    npc_name: &str,
) -> Result<i64, ApiError> {
    let events = list_kill_events(client, group_id, Some(member_name), None, None, None).await?;
    let count = events
        .iter()
        .filter(|event| {
            serde_json::from_value::<GameEvent>(event.payload.clone())
                .is_ok_and(|event| matches!(event, GameEvent::Kill(kill) if kill.npc_name == npc_name))
        })
        .count();
    Ok(count as i64)
}

/// Total deaths recorded for `member_name` in this group with the given killer, all-time - the
/// "Deaths: n" field on a repeat Discord death message, mirroring `count_kills_for_member_npc`.
/// `killer_name: None` counts deaths with no identified killer (see `DeathEvent::killer_name`'s
/// doc comment) rather than every death regardless of killer.
pub async fn count_deaths_for_member_npc(
    client: &Client,
    group_id: i64,
    member_name: &str,
    killer_name: Option<&str>,
) -> Result<i64, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
SELECT payload
FROM groupscape.activity_events
WHERE group_id=$1
  AND event_type='death'
  AND member_name=$2
ORDER BY occurred_at DESC
LIMIT 5000
"#,
        )
        .await?;
    let rows = client
        .query(&stmt, &[&group_id, &member_name])
        .await
        .map_err(ApiError::ListKillEventsError)?;
    let count = rows
        .iter()
        .filter_map(|row| row.try_get::<_, serde_json::Value>("payload").ok())
        .filter(|payload| {
            serde_json::from_value::<GameEvent>(payload.clone()).is_ok_and(|event| {
                matches!(event, GameEvent::Death(death) if death.killer_name.as_deref() == killer_name)
            })
        })
        .count();
    Ok(count as i64)
}

pub struct HomepageStats {
    pub active_groups: i64,
    pub bound_accounts: i64,
    pub bound_characters: i64,
    pub online_characters: i64,
    pub telemetry_pushes_24h: i64,
}

const MEMBER_LAST_UPDATE_GREATEST: &str = r#"GREATEST(
    stats_last_update, coordinates_last_update, skills_last_update,
    quests_last_update, inventory_last_update, equipment_last_update, bank_last_update,
    rune_pouch_last_update, interacting_last_update, seed_vault_last_update, diary_vars_last_update,
    collection_log_last_update, potion_storage_last_update
)"#;

pub async fn get_homepage_stats(client: &Client) -> Result<HomepageStats, ApiError> {
    let active_groups: i64 = client
        .query_one("SELECT COUNT(*) FROM groupscape.groups", &[])
        .await?
        .try_get(0)?;

    let bound_accounts: i64 = client
        .query_one("SELECT COUNT(*) FROM groupscape.accounts", &[])
        .await?
        .try_get(0)?;

    let bound_characters: i64 = client
        .query_one(
            "SELECT COUNT(*) FROM groupscape.characters WHERE status = 'confirmed'",
            &[],
        )
        .await?
        .try_get(0)?;

    // Mirrors the 60s "online" threshold used for the live online badge in the group view.
    // Uses last_seen_at (stamped on every accepted telemetry update, changed or not) rather than
    // the per-field *_last_update columns, which only advance when that field's value actually
    // changes - an idle-but-connected player (unchanging stats/position/inventory) would
    // otherwise read as offline after 60s despite uploads continuing to arrive fine.
    let online_characters: i64 = client
        .query_one(
            "SELECT COUNT(*) FROM groupscape.members \
             WHERE member_name != $1 AND last_seen_at >= NOW() - INTERVAL '60 seconds'",
            &[&SHARED_MEMBER],
        )
        .await?
        .try_get(0)?;

    // Distinct characters that have pushed telemetry at all in the last 24h.
    let telemetry_pushes_24h: i64 = client
        .query_one(
            "SELECT COUNT(*) FROM groupscape.members \
             WHERE member_name != $1 AND last_seen_at >= NOW() - INTERVAL '24 hours'",
            &[&SHARED_MEMBER],
        )
        .await?
        .try_get(0)?;

    Ok(HomepageStats {
        active_groups,
        bound_accounts,
        bound_characters,
        online_characters,
        telemetry_pushes_24h,
    })
}

pub async fn is_member_in_group(
    client: &Client,
    group_id: i64,
    member_name: &str,
) -> Result<bool, ApiError> {
    let stmt = client.prepare_cached("SELECT COUNT(member_name) FROM groupscape.members WHERE group_id=$1 AND member_name=$2").await?;
    let member_count: i64 = client
        .query_one(&stmt, &[&group_id, &member_name])
        .await?
        .try_get(0)
        .map_err(ApiError::IsMemberInGroupError)?;
    Ok(member_count > 0)
}

pub async fn upsert_member_mesh(
    client: &Client,
    member_id: i64,
    mesh: &[u8],
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
INSERT INTO groupscape.member_mesh (member_id, mesh, mesh_last_update) VALUES ($1, $2, NOW())
ON CONFLICT (member_id) DO UPDATE SET mesh=excluded.mesh, mesh_last_update=excluded.mesh_last_update
"#,
        )
        .await?;
    client
        .execute(&stmt, &[&member_id, &mesh])
        .await
        .map_err(ApiError::UpsertMemberMeshError)?;
    Ok(())
}

/// Reads from `character_mesh`, not `member_mesh` - since the account-API-key rework, the
/// plugin only ever uploads portraits via `update_character_portrait` (account-hash-scoped,
/// writes `character_mesh`). `member_mesh`/`upsert_member_mesh` was the old group-token upload
/// path and nothing populates it anymore, so joining through it here left every group-panel
/// portrait permanently empty. `members.account_hash` is how a member row already ties back to
/// its account-linked character (see `update_group_member`), so join through that instead.
pub async fn get_member_mesh(
    client: &Client,
    group_id: i64,
    member_name: &str,
) -> Result<Option<Vec<u8>>, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
SELECT cm.mesh FROM groupscape.character_mesh cm
INNER JOIN groupscape.characters c ON c.character_id=cm.character_id
INNER JOIN groupscape.members m ON m.account_hash=c.account_hash
WHERE m.group_id=$1 AND m.member_name=$2
ORDER BY cm.mesh_last_update DESC
LIMIT 1
"#,
        )
        .await?;
    let row = client
        .query_opt(&stmt, &[&group_id, &member_name])
        .await
        .map_err(ApiError::GetMemberMeshError)?;
    match row {
        Some(row) => Ok(Some(row.try_get("mesh")?)),
        None => Ok(None),
    }
}

pub fn serialize_serde<T>(value: &Option<T>) -> Result<Option<String>, ApiError>
where
    T: Serialize,
{
    match value {
        Some(v) => {
            let result = serde_json::to_string(&v)?;
            Ok(Some(result))
        }
        None => Ok(None),
    }
}

/// Looks up a group's id by name alone, with no token check - used at startup to resolve the
/// seeded demo group's id for the read-only gate (see `crate::demo`), where there's no request
/// token to check against.
pub async fn get_group_id_by_name(
    client: &Client,
    group_name: &str,
) -> Result<Option<i64>, ApiError> {
    let stmt = client
        .prepare_cached("SELECT group_id FROM groupscape.groups WHERE group_name=$1")
        .await?;
    let row = client
        .query_opt(&stmt, &[&group_name])
        .await
        .map_err(ApiError::GetGroupError)?;
    Ok(row.map(|row| row.try_get(0)).transpose()?)
}

pub async fn get_group(client: &Client, group_name: &str, token: &str) -> Result<i64, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT group_id FROM groupscape.groups WHERE group_token_hash=$1 AND group_name=$2",
        )
        .await?;
    let hashed_token = token_hash(token, group_name);
    let group: Row = client
        .query_one(&stmt, &[&hashed_token, &group_name])
        .await
        .map_err(ApiError::GetGroupError)?;
    Ok(group.try_get(0)?)
}

fn try_deserialize_json_column<T>(row: &Row, column: &str) -> Result<Option<T>, ApiError>
where
    T: DeserializeOwned,
{
    match row.try_get(column) {
        Ok(column_data) => Ok(serde_json::from_str(column_data).ok()),
        Err(_) => Ok(None),
    }
}

pub async fn get_group_data(
    client: &Client,
    group_id: i64,
    timestamp: &DateTime<Utc>,
) -> Result<Vec<GroupMember>, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
SELECT member_name, color,
GREATEST(members.last_seen_at, character_mesh.mesh_last_update) as last_updated,
CASE WHEN stats_last_update >= $1::TIMESTAMPTZ THEN stats ELSE NULL END as stats,
CASE WHEN coordinates_last_update >= $1::TIMESTAMPTZ THEN coordinates ELSE NULL END as coordinates,
CASE WHEN skills_last_update >= $1::TIMESTAMPTZ THEN skills ELSE NULL END as skills,
CASE WHEN quests_last_update >= $1::TIMESTAMPTZ THEN quests ELSE NULL END as quests,
CASE WHEN inventory_last_update >= $1::TIMESTAMPTZ THEN inventory ELSE NULL END as inventory,
CASE WHEN equipment_last_update >= $1::TIMESTAMPTZ THEN equipment ELSE NULL END as equipment,
CASE WHEN bank_last_update >= $1::TIMESTAMPTZ THEN bank ELSE NULL END as bank,
CASE WHEN rune_pouch_last_update >= $1::TIMESTAMPTZ THEN rune_pouch ELSE NULL END as rune_pouch,
CASE WHEN interacting_last_update >= $1::TIMESTAMPTZ THEN interacting ELSE NULL END as interacting,
CASE WHEN seed_vault_last_update >= $1::TIMESTAMPTZ THEN seed_vault ELSE NULL END as seed_vault,
CASE WHEN diary_vars_last_update >= $1::TIMESTAMPTZ THEN diary_vars ELSE NULL END as diary_vars,
CASE WHEN collection_log_last_update >= $1::TIMESTAMPTZ THEN collection_log ELSE NULL END as collection_log,
CASE WHEN potion_storage_last_update >= $1::TIMESTAMPTZ THEN potion_storage ELSE NULL END as potion_storage,
CASE WHEN special_attack_last_update >= $1::TIMESTAMPTZ THEN special_attack ELSE NULL END as special_attack,
CASE WHEN active_prayers_last_update >= $1::TIMESTAMPTZ THEN active_prayers ELSE NULL END as active_prayers,
CASE WHEN rich_presence_last_update >= $1::TIMESTAMPTZ THEN rich_presence ELSE NULL END as rich_presence,
CASE WHEN combat_achievements_last_update >= $1::TIMESTAMPTZ THEN combat_achievements ELSE NULL END as combat_achievements,
CASE WHEN slayer_task_last_update >= $1::TIMESTAMPTZ THEN slayer_task ELSE NULL END as slayer_task,
CASE WHEN character_mesh.mesh_last_update >= $1::TIMESTAMPTZ THEN character_mesh.mesh_last_update ELSE NULL END as portrait_last_update
FROM groupscape.members
LEFT JOIN LATERAL (
  SELECT c.character_id
  FROM groupscape.characters c
  LEFT JOIN groupscape.character_mesh cm ON cm.character_id = c.character_id
  WHERE c.account_hash = members.account_hash
  ORDER BY cm.mesh_last_update DESC NULLS LAST
  LIMIT 1
) characters ON true
LEFT JOIN groupscape.character_mesh ON character_mesh.character_id = characters.character_id
WHERE group_id=$2
"#,
        )
        .await?;

    let rows = client
        .query(&stmt, &[&timestamp, &group_id])
        .await
        .map_err(ApiError::GetGroupDataError)?;
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let member_name = row.try_get("member_name")?;
        let last_updated: Option<DateTime<Utc>> = row.try_get("last_updated").ok();
        let group_member = GroupMember {
            group_id: Some(group_id),
            name: member_name,
            account_hash: None,
            color: row.try_get("color").ok(),
            last_updated,
            stats: row.try_get("stats").ok(),
            coordinates: row.try_get("coordinates").ok(),
            skills: row.try_get("skills").ok(),
            quests: row.try_get("quests")?,
            inventory: row.try_get("inventory").ok(),
            equipment: row.try_get("equipment").ok(),
            bank: row.try_get("bank").ok(),
            rune_pouch: row.try_get("rune_pouch").ok(),
            seed_vault: row.try_get("seed_vault").ok(),
            interacting: normalize_interacting(try_deserialize_json_column(&row, "interacting")?),
            diary_vars: row.try_get("diary_vars").ok(),
            shared_bank: Option::None,
            deposited: Option::None,
            collection_log_v2: row.try_get("collection_log").ok(),
            collection_log_sync: None,
            potion_storage: row.try_get("potion_storage").ok(),
            special_attack: row.try_get("special_attack").ok(),
            active_prayers: row.try_get("active_prayers").ok(),
            rich_presence: row.try_get("rich_presence").ok(),
            events: None,
            interactions: None,
            object_interactions: None,
            alerts: None,
            notable_drops: None,
            combat_achievements: try_deserialize_json_column(&row, "combat_achievements")?,
            slayer_task: try_deserialize_json_column(&row, "slayer_task")?,
            slayer_task_events: None,
            portrait_last_update: row.try_get("portrait_last_update").ok(),
            pending: false,
        };
        result.push(group_member);
    }

    result.extend(get_pending_group_members(client, group_id).await?);

    Ok(result)
}

/// Characters an admin has linked to the group (`admin_add_account_to_group`,
/// `link_character_to_group`) whose account_hash has no `groupscape.members` row yet - i.e.
/// their RuneLite plugin hasn't sent a first telemetry update, so `ensure_group_member` hasn't
/// provisioned them. Surfaced in `get_group_data` as pending placeholders so an added member
/// isn't simply absent from the roster until they log in.
async fn get_pending_group_members(client: &Client, group_id: i64) -> Result<Vec<GroupMember>, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT c.display_rsn FROM groupscape.characters c \
             JOIN groupscape.character_group_links l ON l.character_id = c.character_id \
             WHERE l.group_id = $1 \
             AND NOT EXISTS ( \
                 SELECT 1 FROM groupscape.members m WHERE m.group_id = $1 AND m.account_hash = c.account_hash \
             )",
        )
        .await?;
    let rows = client
        .query(&stmt, &[&group_id])
        .await
        .map_err(ApiError::GetGroupDataError)?;
    rows.iter()
        .map(|row| {
            Ok(GroupMember {
                group_id: Some(group_id),
                name: row.try_get("display_rsn")?,
                account_hash: None,
                color: None,
                last_updated: None,
                stats: None,
                coordinates: None,
                skills: None,
                quests: None,
                inventory: None,
                equipment: None,
                bank: None,
                rune_pouch: None,
                seed_vault: None,
                interacting: None,
                diary_vars: None,
                shared_bank: None,
                deposited: None,
                collection_log_v2: None,
                collection_log_sync: None,
                potion_storage: None,
                special_attack: None,
                active_prayers: None,
                rich_presence: None,
                events: None,
                interactions: None,
                object_interactions: None,
                alerts: None,
                notable_drops: None,
                combat_achievements: None,
                slayer_task: None,
                slayer_task_events: None,
                portrait_last_update: None,
                pending: true,
            })
        })
        .collect()
}

/// Fetches one member's currently persisted full row (unlike `get_group_data`, no
/// timestamp gating - every column comes back as its actual stored value). Used to merge
/// a just-received partial heartbeat onto the last known state before broadcasting a
/// `vitals_update`, so fields the plugin only sends on change (target, spec energy, active
/// prayers) don't flicker to blank on every heartbeat that doesn't happen to touch them.
pub async fn get_group_member(
    client: &Client,
    group_id: i64,
    member_name: &str,
) -> Result<Option<GroupMember>, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
SELECT color,
last_seen_at as last_updated,
stats, coordinates, skills, quests, inventory, equipment, bank, rune_pouch, interacting,
seed_vault, diary_vars, collection_log, potion_storage, special_attack, active_prayers,
rich_presence, combat_achievements, slayer_task
FROM groupscape.members
WHERE group_id=$1 AND member_name=$2
"#,
        )
        .await?;

    let row = client
        .query_opt(&stmt, &[&group_id, &member_name])
        .await
        .map_err(ApiError::GetGroupDataError)?;

    let row = match row {
        Some(row) => row,
        None => return Ok(None),
    };

    let last_updated: Option<DateTime<Utc>> = row.try_get("last_updated").ok();
    Ok(Some(GroupMember {
        group_id: Some(group_id),
        name: member_name.to_string(),
        account_hash: None,
        color: row.try_get("color").ok(),
        last_updated,
        stats: row.try_get("stats").ok(),
        coordinates: row.try_get("coordinates").ok(),
        skills: row.try_get("skills").ok(),
        quests: row.try_get("quests")?,
        inventory: row.try_get("inventory").ok(),
        equipment: row.try_get("equipment").ok(),
        bank: row.try_get("bank").ok(),
        rune_pouch: row.try_get("rune_pouch").ok(),
        seed_vault: row.try_get("seed_vault").ok(),
        interacting: normalize_interacting(try_deserialize_json_column(&row, "interacting")?),
        diary_vars: row.try_get("diary_vars").ok(),
        shared_bank: Option::None,
        deposited: Option::None,
        collection_log_v2: row.try_get("collection_log").ok(),
        collection_log_sync: None,
        potion_storage: row.try_get("potion_storage").ok(),
        special_attack: row.try_get("special_attack").ok(),
        active_prayers: row.try_get("active_prayers").ok(),
        rich_presence: row.try_get("rich_presence").ok(),
        events: None,
        interactions: None,
        object_interactions: None,
        alerts: None,
        notable_drops: None,
        combat_achievements: try_deserialize_json_column(&row, "combat_achievements")?,
        slayer_task: try_deserialize_json_column(&row, "slayer_task")?,
        slayer_task_events: None,
        portrait_last_update: None,
        pending: false,
    }))
}

/// Assigns each non-shared group member a stable display color by join order
/// (member_id ascending), matching groupscape-old's join-order palette so the
/// overlay's ownership stripe stays consistent across sessions.
pub async fn get_member_color_map(
    client: &Client,
    group_id: i64,
) -> Result<HashMap<String, String>, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT member_name FROM groupscape.members WHERE group_id=$1 AND member_name != $2 ORDER BY member_id ASC",
        )
        .await?;
    let rows = client
        .query(&stmt, &[&group_id, &SHARED_MEMBER])
        .await
        .map_err(ApiError::GetMemberColorsError)?;

    let mut colors = HashMap::new();
    for (index, row) in rows.into_iter().enumerate() {
        let member_name: String = row.try_get(0)?;
        colors.insert(member_name, crate::websocket::member_color(index));
    }
    Ok(colors)
}

#[derive(Clone, Copy)]
pub enum AggregatePeriod {
    Day,
    Month,
    Year,
}
async fn aggregate_skills_for_period(
    transaction: &Transaction<'_>,
    period: AggregatePeriod,
    last_aggregation: &DateTime<Utc>,
) -> Result<(), ApiError> {
    let s = format!(
        r#"
INSERT INTO groupscape.skills_{} (member_id, time, skills)
SELECT member_id, date_trunc('{}', skills_last_update), skills FROM groupscape.members
WHERE skills_last_update IS NOT NULL AND skills IS NOT NULL AND skills_last_update >= $1
ON CONFLICT (member_id, time)
DO UPDATE SET skills=excluded.skills;
"#,
        match period {
            AggregatePeriod::Day => "day",
            AggregatePeriod::Month => "month",
            AggregatePeriod::Year => "year",
        },
        match period {
            AggregatePeriod::Day => "hour",
            AggregatePeriod::Month => "day",
            AggregatePeriod::Year => "month",
        }
    );
    let aggregate_stmt = transaction.prepare_cached(&s).await?;
    transaction
        .execute(&aggregate_stmt, &[&last_aggregation])
        .await?;

    Ok(())
}

async fn apply_skills_retention_for_period(
    transaction: &Transaction<'_>,
    period: AggregatePeriod,
    last_aggregation: &DateTime<Utc>,
) -> Result<(), ApiError> {
    let s = format!(
        r#"
DELETE FROM groupscape.skills_{0}
WHERE time < ($1::timestamptz - interval '{1}') AND (member_id, time) NOT IN (
  SELECT member_id, max(time) FROM groupscape.skills_{0} WHERE time < ($1::timestamptz - interval '{1}') GROUP BY member_id
)
"#,
        match period {
            AggregatePeriod::Day => "day",
            AggregatePeriod::Month => "month",
            AggregatePeriod::Year => "year",
        },
        match period {
            AggregatePeriod::Day => "1 day",
            AggregatePeriod::Month => "1 month",
            AggregatePeriod::Year => "1 year",
        }
    );
    let delete_old_rows_stmt = transaction.prepare_cached(&s).await?;
    transaction
        .execute(&delete_old_rows_stmt, &[&last_aggregation])
        .await?;

    Ok(())
}

/// Reads `aggregation_info.last_aggregation` for a given `type` row (`'skills'`, `'bank_value'`,
/// ...) - the watermark each periodic aggregator uses to only pick up rows updated since its
/// last run.
async fn get_last_aggregation(client: &Client, aggregation_type: &str) -> Result<DateTime<Utc>, ApiError> {
    let last_aggregation_stmt = client
        .prepare_cached(
            r#"
SELECT last_aggregation FROM groupscape.aggregation_info WHERE type=$1"#,
        )
        .await?;
    let last_aggregation: DateTime<Utc> = client
        .query_one(&last_aggregation_stmt, &[&aggregation_type])
        .await?
        .try_get(0)?;

    Ok(last_aggregation)
}

pub async fn get_last_skills_aggregation(client: &Client) -> Result<DateTime<Utc>, ApiError> {
    get_last_aggregation(client, "skills").await
}

pub async fn aggregate_skills(client: &mut Client) -> Result<(), ApiError> {
    let last_aggregation = get_last_skills_aggregation(client).await?;

    let transaction = client.transaction().await?;
    let update_last_aggregation_stmt = transaction
        .prepare_cached(
            r#"
UPDATE groupscape.aggregation_info SET last_aggregation=NOW() WHERE type='skills'"#,
        )
        .await?;
    transaction
        .execute(&update_last_aggregation_stmt, &[])
        .await?;

    aggregate_skills_for_period(&transaction, AggregatePeriod::Day, &last_aggregation).await?;
    aggregate_skills_for_period(&transaction, AggregatePeriod::Month, &last_aggregation).await?;
    aggregate_skills_for_period(&transaction, AggregatePeriod::Year, &last_aggregation).await?;
    transaction.commit().await?;

    Ok(())
}

pub async fn apply_skills_retention(client: &mut Client) -> Result<(), ApiError> {
    let last_aggregation = get_last_skills_aggregation(client).await?;

    let transaction = client.transaction().await?;
    apply_skills_retention_for_period(&transaction, AggregatePeriod::Day, &last_aggregation)
        .await?;
    apply_skills_retention_for_period(&transaction, AggregatePeriod::Month, &last_aggregation)
        .await?;
    apply_skills_retention_for_period(&transaction, AggregatePeriod::Year, &last_aggregation)
        .await?;
    transaction.commit().await?;

    Ok(())
}

pub async fn get_skills_for_period(
    client: &Client,
    group_id: i64,
    period: AggregatePeriod,
) -> Result<GroupSkillData, ApiError> {
    let s = format!(
        r#"
SELECT member_name, time, s.skills
FROM groupscape.skills_{} s
INNER JOIN groupscape.members m ON m.member_id=s.member_id
WHERE m.group_id=$1 AND m.member_name != $2
"#,
        match period {
            AggregatePeriod::Day => "day",
            AggregatePeriod::Month => "month",
            AggregatePeriod::Year => "year",
        }
    );
    let get_skills_stmt = client.prepare_cached(&s).await?;
    let rows = client
        .query(&get_skills_stmt, &[&group_id, &SHARED_MEMBER])
        .await
        .map_err(ApiError::GetSkillsDataError)?;

    let mut member_data = HashMap::new();
    for row in rows {
        let member_name: String = row.try_get("member_name")?;
        let skill_data = AggregateSkillData {
            time: row.try_get("time")?,
            data: row.try_get("skills")?,
        };

        if !member_data.contains_key(&member_name) {
            member_data.insert(
                member_name.clone(),
                MemberSkillData {
                    name: member_name,
                    skill_data: vec![skill_data],
                },
            );
        } else if let Some(member_skill_data) = member_data.get_mut(&member_name) {
            member_skill_data.skill_data.push(skill_data);
        }
    }

    Ok(member_data.into_values().collect())
}

pub async fn has_migration_run(client: &mut Client, name: &str) -> Result<bool, ApiError> {
    let count: i64 = client
        .query_one(
            "SELECT COUNT(*) FROM groupscape.migrations WHERE name=$1",
            &[&name],
        )
        .await?
        .try_get(0)?;

    Ok(count > 0)
}

pub async fn commit_migration(transaction: &Transaction<'_>, name: &str) -> Result<(), ApiError> {
    transaction
        .execute(
            "INSERT INTO groupscape.migrations (name, date) VALUES($1, NOW())",
            &[&name],
        )
        .await?;

    Ok(())
}

async fn create_timestamp_trigger(
    transaction: &Transaction<'_>,
    name: &str,
) -> Result<(), ApiError> {
    let create_fn = format!(
        r#"
CREATE OR REPLACE FUNCTION groupscape.update_{0}_timestamp()
RETURNS TRIGGER AS $$
BEGIN
    NEW.{0}_last_update = now();
    RETURN NEW;
END;
$$ language 'plpgsql';
"#,
        name
    );
    transaction.execute(&create_fn, &[]).await?;

    let trigger_stmt = format!(
        r#"
DO
$$BEGIN
  CREATE TRIGGER set_{0}_timestamp
  BEFORE UPDATE ON groupscape.members
  FOR EACH ROW
  WHEN (OLD.{0} IS DISTINCT FROM NEW.{0})
  EXECUTE FUNCTION groupscape.update_{0}_timestamp();
EXCEPTION
  WHEN duplicate_object THEN
    NULL;
END;$$;
"#,
        name
    );
    transaction.execute(&trigger_stmt, &[]).await?;

    Ok(())
}

pub async fn update_schema(client: &mut Client) -> Result<(), ApiError> {
    client
        .execute(
            r#"
CREATE TABLE IF NOT EXISTS groupscape.migrations (
    name TEXT,
    date TIMESTAMPTZ
)
"#,
            &[],
        )
        .await?;

    if !has_migration_run(client, "add_groups_version_column").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.groups ADD COLUMN IF NOT EXISTS version INTEGER default 1
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_groups_version_column").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_members_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.members (
  member_id BIGSERIAL PRIMARY KEY,
  group_id BIGSERIAL REFERENCES groupscape.groups(group_id),
  member_name TEXT NOT NULL,

  stats_last_update TIMESTAMPTZ,
  stats INTEGER[7],

  coordinates_last_update TIMESTAMPTZ,
  coordinates INTEGER[3],

  skills_last_update TIMESTAMPTZ,
  skills INTEGER[24],

  quests_last_update TIMESTAMPTZ,
  quests bytea,

  inventory_last_update TIMESTAMPTZ,
  inventory INTEGER[56],

  equipment_last_update TIMESTAMPTZ,
  equipment INTEGER[28],

  rune_pouch_last_update TIMESTAMPTZ,
  rune_pouch INTEGER[8],

  bank_last_update TIMESTAMPTZ,
  bank INTEGER[],

  seed_vault_last_update TIMESTAMPTZ,
  seed_vault INTEGER[],

  interacting_last_update TIMESTAMPTZ,
  interacting TEXT
);
"#,
                &[],
            )
            .await?;

        transaction.execute(r#"
CREATE UNIQUE INDEX IF NOT EXISTS members_groupid_name_idx ON groupscape.members (group_id, member_name);
"#, &[]).await?;

        commit_migration(&transaction, "create_members_table").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_diary_vars").await? {
        let transaction = client.transaction().await?;
        // Adding new columns for new types of data
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.members
ADD COLUMN IF NOT EXISTS diary_vars_last_update TIMESTAMPTZ,
ADD COLUMN IF NOT EXISTS diary_vars INTEGER[62]
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_diary_vars").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_skill_periods").await? {
        let transaction = client.transaction().await?;

        let periods = vec!["day", "month", "year"];
        for period in periods {
            let create_skills_aggregate = format!(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.skills_{} (
    member_id BIGSERIAL REFERENCES groupscape.members(member_id),
    time TIMESTAMPTZ,
    skills INTEGER[24],

    PRIMARY KEY (member_id, time)
);
"#,
                period
            );
            transaction.execute(&create_skills_aggregate, &[]).await?;
        }

        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.aggregation_info (
    type TEXT PRIMARY KEY,
    last_aggregation TIMESTAMPTZ NOT NULL DEFAULT TIMESTAMP WITH TIME ZONE 'epoch'
);
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
INSERT INTO groupscape.aggregation_info (type) VALUES ('skills')
ON CONFLICT (type) DO NOTHING
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_skill_periods").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "member_name_citext").await? {
        let transaction = client.transaction().await?;

        // We need to rename members in groups which would violate the unique constraint after
        // we make the column case insensitive.
        let duplicates = transaction
            .query(
                r#"
SELECT a.group_id, a.member_id, a.member_name FROM groupscape.members a
INNER JOIN (
	SELECT group_id, lower(member_name) as member_name, COUNT(*) FROM groupscape.members
	GROUP BY group_id, lower(member_name)
	HAVING COUNT(*) > 1
) b
ON a.group_id=b.group_id AND lower(a.member_name)=lower(b.member_name)
ORDER BY GREATEST(
	stats_last_update,
	coordinates_last_update,
	skills_last_update,
	quests_last_update,
	inventory_last_update,
	equipment_last_update,
	bank_last_update,
	rune_pouch_last_update,
	interacting_last_update,
	seed_vault_last_update,
	diary_vars_last_update
) ASC;
"#,
                &[],
            )
            .await?;

        let mut already_encounted: HashSet<String> = HashSet::new();
        for row in duplicates {
            let group_id: i64 = row.try_get("group_id")?;
            let member_id: i64 = row.try_get("member_id")?;
            let member_name: String = row.try_get("member_name")?;
            let member_name_lower: String = member_name.to_lowercase();

            let key = format!("{}::{}", group_id, member_name_lower);
            // Skip the first encounter with the duplicate name since that is the entry
            // with the most recent update.
            if !already_encounted.insert(key) {
                log::info!(
                    "Renaming duplicate member name '{}' in group '{}'",
                    member_name,
                    group_id
                );

                for _ in 1..5 {
                    let uuid = uuid::Uuid::new_v4().hyphenated().to_string();
                    let new_name = &uuid[..uuid.find("-").unwrap()];
                    log::info!("Trying new name '{}'", new_name);
                    if transaction
                        .execute(
                            "UPDATE groupscape.members SET member_name=$1 WHERE member_id=$2",
                            &[&new_name, &member_id],
                        )
                        .await
                        .is_ok()
                    {
                        break;
                    }
                }
            }
        }

        transaction
            .execute(
                "CREATE EXTENSION IF NOT EXISTS citext WITH SCHEMA public",
                &[],
            )
            .await
            .ok();
        transaction
            .execute(
                "ALTER TABLE groupscape.members ALTER COLUMN member_name TYPE citext",
                &[],
            )
            .await?;

        commit_migration(&transaction, "member_name_citext").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_collection_log_member_column").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.members
ADD COLUMN IF NOT EXISTS collection_log_last_update TIMESTAMPTZ,
ADD COLUMN IF NOT EXISTS collection_log INTEGER[]
"#,
                &[],
            )
            .await?;
        commit_migration(&transaction, "add_collection_log_member_column").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "migrate_collection_log_v2").await?
        && has_migration_run(client, "add_collection_log").await?
    {
        println!("beginning migration migrate_collection_log_v2");
        let transaction = client.transaction().await?;

        // collect the data to migrate
        let rows = transaction
            .query("SELECT member_id, items FROM groupscape.collection_log WHERE cardinality(items) > 0", &[])
            .await
            .unwrap();
        let mut member_data: HashMap<i64, Vec<i32>> = HashMap::new();
        for row in rows {
            let member_id: i64 = row.try_get("member_id")?;
            let items: Vec<i32> = row.try_get("items")?;

            match member_data.get_mut(&member_id) {
                Some(collection_log) => {
                    collection_log.extend(items.iter());
                }
                None => {
                    member_data.insert(member_id, items);
                }
            };
        }
        println!("need to migrate {} members", member_data.len());

        // breakup into chunks
        let chunk_size = 100;
        let member_data_list: Vec<(i64, Vec<i32>)> = member_data.into_iter().collect();
        let mut chunks = Vec::new();
        for chunk_slice in member_data_list.chunks(chunk_size) {
            let chunk_map: HashMap<i64, Vec<i32>> = chunk_slice.iter().cloned().collect();
            chunks.push(chunk_map);
        }
        println!("split into {} chunks of size {}", chunks.len(), chunk_size);

        // update new collection log column
        for (i, chunk) in chunks.iter().enumerate() {
            println!(
                "migrating chunk {}/{} size {}",
                i + 1,
                chunks.len(),
                chunk.len()
            );
            let mut values_clause = String::new();
            for i in 0..chunk.len() {
                values_clause.push_str(&format!(
                    "(${}::BIGINT, ${}::INTEGER[])",
                    i * 2 + 1,
                    i * 2 + 2
                ));
                if i < chunk.len() - 1 {
                    values_clause.push_str(", ");
                }
            }
            let mut params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
            for (member_id, items) in chunk.iter() {
                params.push(member_id);
                params.push(items);
            }

            // timestamp is set to value that will return on the initial frontend request, but does not show the player as online
            let update_query = format!(
                r#"
UPDATE groupscape.members as a SET collection_log=b.collection_log, collection_log_last_update='epoch'::timestamptz + INTERVAL '5 days'
FROM (VALUES {}) AS b(member_id, collection_log)
WHERE a.member_id=b.member_id
"#,
                values_clause
            );

            transaction.execute(&update_query, &params).await?;
        }

        commit_migration(&transaction, "migrate_collection_log_v2").await?;
        transaction.commit().await?;
        println!("finished migration migrate_collection_log_v2");
    }

    if !has_migration_run(client, "update_timestamp_triggers").await? {
        let transaction = client.transaction().await?;

        let names = vec![
            "stats",
            "coordinates",
            "skills",
            "quests",
            "inventory",
            "equipment",
            "bank",
            "rune_pouch",
            "interacting",
            "seed_vault",
            "diary_vars",
            "collection_log",
        ];

        for name in names {
            create_timestamp_trigger(&transaction, name).await?;
        }

        commit_migration(&transaction, "update_timestamp_triggers").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_potion_storage").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.members
ADD COLUMN IF NOT EXISTS potion_storage_last_update TIMESTAMPTZ,
ADD COLUMN IF NOT EXISTS potion_storage INTEGER[]
"#,
                &[],
            )
            .await?;

        create_timestamp_trigger(&transaction, "potion_storage").await?;

        commit_migration(&transaction, "add_potion_storage").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_party_overlay_columns").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.members
ADD COLUMN IF NOT EXISTS special_attack_last_update TIMESTAMPTZ,
ADD COLUMN IF NOT EXISTS special_attack INTEGER,
ADD COLUMN IF NOT EXISTS active_prayers_last_update TIMESTAMPTZ,
ADD COLUMN IF NOT EXISTS active_prayers TEXT[],
ADD COLUMN IF NOT EXISTS rich_presence_last_update TIMESTAMPTZ,
ADD COLUMN IF NOT EXISTS rich_presence TEXT
"#,
                &[],
            )
            .await?;

        create_timestamp_trigger(&transaction, "special_attack").await?;
        create_timestamp_trigger(&transaction, "active_prayers").await?;
        create_timestamp_trigger(&transaction, "rich_presence").await?;

        commit_migration(&transaction, "add_party_overlay_columns").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_member_mesh_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.member_mesh (
  member_id BIGINT PRIMARY KEY REFERENCES groupscape.members(member_id) ON DELETE CASCADE,
  mesh BYTEA NOT NULL,
  mesh_last_update TIMESTAMPTZ NOT NULL
);
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_member_mesh_table").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_group_moderation_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.group_moderation (
  group_id BIGINT PRIMARY KEY REFERENCES groupscape.groups(group_id) ON DELETE CASCADE,
  status TEXT NOT NULL DEFAULT 'active',
  reason TEXT,
  actor TEXT NOT NULL DEFAULT 'admin',
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_group_moderation_table").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_feature_flags_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.feature_flags (
  flag_key TEXT PRIMARY KEY,
  enabled BOOLEAN NOT NULL DEFAULT false,
  description TEXT,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_feature_flags_table").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_admin_audit_log_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.admin_audit_log (
  id BIGSERIAL PRIMARY KEY,
  action TEXT NOT NULL,
  target_type TEXT,
  target_id TEXT,
  detail JSONB,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
CREATE INDEX IF NOT EXISTS admin_audit_log_created_at_idx ON groupscape.admin_audit_log(created_at DESC);
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_admin_audit_log_table").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_accounts_stub_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.accounts (
  id BIGSERIAL PRIMARY KEY,
  username CITEXT UNIQUE,
  disabled BOOLEAN NOT NULL DEFAULT false,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_accounts_stub_table").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "rename_account_email_to_username").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
DO $$
BEGIN
  IF EXISTS (
    SELECT 1 FROM information_schema.columns
    WHERE table_schema='groupscape' AND table_name='accounts' AND column_name='email'
  ) AND NOT EXISTS (
    SELECT 1 FROM information_schema.columns
    WHERE table_schema='groupscape' AND table_name='accounts' AND column_name='username'
  ) THEN
    ALTER TABLE groupscape.accounts RENAME COLUMN email TO username;
  END IF;
END$$;
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "rename_account_email_to_username").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_account_password_hash").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.accounts ADD COLUMN IF NOT EXISTS password_hash TEXT
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_account_password_hash").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_account_sessions_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.account_sessions (
  session_id BIGSERIAL PRIMARY KEY,
  account_id BIGINT NOT NULL REFERENCES groupscape.accounts(id) ON DELETE CASCADE,
  token_hash TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  expires_at TIMESTAMPTZ NOT NULL
);
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
CREATE UNIQUE INDEX IF NOT EXISTS account_sessions_token_hash_idx ON groupscape.account_sessions (token_hash);
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_account_sessions_table").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_account_discord_id").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.accounts ADD COLUMN IF NOT EXISTS discord_id TEXT
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
CREATE UNIQUE INDEX IF NOT EXISTS accounts_discord_id_idx ON groupscape.accounts (discord_id) WHERE discord_id IS NOT NULL;
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_account_discord_id").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_account_discord_name").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                "ALTER TABLE groupscape.accounts ADD COLUMN IF NOT EXISTS discord_name TEXT",
                &[],
            )
            .await?;
        commit_migration(&transaction, "add_account_discord_name").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_characters_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.characters (
  character_id BIGSERIAL PRIMARY KEY,
  account_id BIGINT NOT NULL REFERENCES groupscape.accounts(id) ON DELETE CASCADE,
  account_hash TEXT NOT NULL,
  display_rsn TEXT NOT NULL,
  bound_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
CREATE INDEX IF NOT EXISTS characters_account_id_idx ON groupscape.characters (account_id);
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_characters_table").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_character_group_links_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.character_group_links (
  character_id BIGINT PRIMARY KEY REFERENCES groupscape.characters(character_id) ON DELETE CASCADE,
  group_id BIGINT NOT NULL REFERENCES groupscape.groups(group_id),
  linked_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
CREATE INDEX IF NOT EXISTS character_group_links_group_id_idx ON groupscape.character_group_links (group_id);
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_character_group_links_table").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_blocked_members_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.blocked_members (
  group_id BIGINT NOT NULL REFERENCES groupscape.groups(group_id) ON DELETE CASCADE,
  member_name CITEXT NOT NULL,
  blocked_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (group_id, member_name)
);
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_blocked_members_table").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_groups_admin_account_id").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.groups
ADD COLUMN IF NOT EXISTS admin_account_id BIGINT REFERENCES groupscape.accounts(id)
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_groups_admin_account_id").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_members_account_hash_column").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.members ADD COLUMN IF NOT EXISTS account_hash TEXT
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
CREATE UNIQUE INDEX IF NOT EXISTS members_groupid_account_hash_idx ON groupscape.members (group_id, account_hash) WHERE account_hash IS NOT NULL
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_members_account_hash_column").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_group_permissions_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.group_permissions (
  group_id BIGINT NOT NULL REFERENCES groupscape.groups(group_id) ON DELETE CASCADE,
  account_id BIGINT NOT NULL REFERENCES groupscape.accounts(id) ON DELETE CASCADE,
  invite_members BOOLEAN NOT NULL DEFAULT false,
  regenerate_group_key BOOLEAN NOT NULL DEFAULT false,
  kick_members BOOLEAN NOT NULL DEFAULT false,
  manage_settings BOOLEAN NOT NULL DEFAULT false,
  manage_permissions BOOLEAN NOT NULL DEFAULT false,
  post_map_markers BOOLEAN NOT NULL DEFAULT false,
  post_callouts BOOLEAN NOT NULL DEFAULT false,
  manage_goals BOOLEAN NOT NULL DEFAULT false,
  manage_discord BOOLEAN NOT NULL DEFAULT false,
  manage_events BOOLEAN NOT NULL DEFAULT false,
  PRIMARY KEY (group_id, account_id)
);
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_group_permissions_table").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_sessions_and_activity_events_tables").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.sessions (
  session_id BIGSERIAL PRIMARY KEY,
  group_id BIGINT NOT NULL REFERENCES groupscape.groups(group_id) ON DELETE CASCADE,
  started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  ended_at TIMESTAMPTZ
);
"#,
                &[],
            )
            .await?;
        // Partial unique index (rather than a plain UNIQUE(group_id)) is what makes
        // `ensure_open_session`'s ON CONFLICT upsert atomic while still allowing a group to
        // accumulate many *closed* sessions over time.
        transaction
            .execute(
                r#"
CREATE UNIQUE INDEX IF NOT EXISTS sessions_one_open_per_group_idx ON groupscape.sessions (group_id) WHERE ended_at IS NULL
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.activity_events (
  event_id BIGSERIAL PRIMARY KEY,
  session_id BIGINT NOT NULL REFERENCES groupscape.sessions(session_id) ON DELETE CASCADE,
  group_id BIGINT NOT NULL REFERENCES groupscape.groups(group_id) ON DELETE CASCADE,
  member_name CITEXT NOT NULL,
  event_type TEXT NOT NULL,
  occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  payload JSONB NOT NULL
);
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
CREATE INDEX IF NOT EXISTS activity_events_group_occurred_idx ON groupscape.activity_events (group_id, occurred_at DESC)
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_sessions_and_activity_events_tables").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_location_samples_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.location_samples (
  sample_id BIGSERIAL PRIMARY KEY,
  group_id BIGINT NOT NULL REFERENCES groupscape.groups(group_id) ON DELETE CASCADE,
  member_name CITEXT NOT NULL,
  sampled_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  world INTEGER NOT NULL,
  plane INTEGER NOT NULL,
  world_x INTEGER NOT NULL,
  world_y INTEGER NOT NULL
)
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
CREATE INDEX IF NOT EXISTS location_samples_lookup_idx
  ON groupscape.location_samples (group_id, member_name, sampled_at DESC)
"#,
                &[],
            )
            .await?;
        commit_migration(&transaction, "create_location_samples_table").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_combat_achievements_column").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.members
ADD COLUMN IF NOT EXISTS combat_achievements_last_update TIMESTAMPTZ,
ADD COLUMN IF NOT EXISTS combat_achievements TEXT
"#,
                &[],
            )
            .await?;

        create_timestamp_trigger(&transaction, "combat_achievements").await?;

        commit_migration(&transaction, "add_combat_achievements_column").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_push_subscriptions_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.push_subscriptions (
  subscription_id BIGSERIAL PRIMARY KEY,
  account_id BIGINT NOT NULL REFERENCES groupscape.accounts(id) ON DELETE CASCADE,
  endpoint TEXT NOT NULL UNIQUE,
  p256dh TEXT NOT NULL,
  auth TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
CREATE INDEX IF NOT EXISTS push_subscriptions_account_id_idx ON groupscape.push_subscriptions (account_id)
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_push_subscriptions_table").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_groups_discord_webhook_columns").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.groups
ADD COLUMN IF NOT EXISTS discord_webhook_url TEXT,
ADD COLUMN IF NOT EXISTS discord_notify_kills BOOLEAN NOT NULL DEFAULT true,
ADD COLUMN IF NOT EXISTS discord_notify_deaths BOOLEAN NOT NULL DEFAULT true,
ADD COLUMN IF NOT EXISTS discord_notify_loot BOOLEAN NOT NULL DEFAULT true
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_groups_discord_webhook_columns").await?;
        transaction.commit().await?;
    }

    // Leaderboards (XP/KC/GP/loot) reuse the Graphs tab's existing `skills_day`/`skills_month`/
    // `skills_year` history for XP, and `activity_events` (already unbounded/retained) for
    // boss-KC and loot-value - no new snapshot table needed for those three. Bank *contents*
    // have no history anywhere in this schema though, so GP-earned needs one new lightweight
    // table; unlike the skills_* tables it's never pruned (one small row/member/day forever).
    if !has_migration_run(client, "create_bank_value_snapshots_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.bank_value_snapshots (
  id BIGSERIAL PRIMARY KEY,
  member_id BIGINT NOT NULL REFERENCES groupscape.members(member_id) ON DELETE CASCADE,
  snapshot_date DATE NOT NULL,
  captured_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  bank_value BIGINT NOT NULL DEFAULT 0
);
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
CREATE UNIQUE INDEX IF NOT EXISTS bank_value_snapshots_member_date_idx ON groupscape.bank_value_snapshots (member_id, snapshot_date)
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_bank_value_snapshots_table").await?;
        transaction.commit().await?;
    }

    // Purely-additive, parallel to `bank_value_snapshots` above: that table stays
    // daily-granularity and keeps serving the GP-earned *leaderboard* metric unchanged. This is
    // history for the Graphs tab's GP-earned *chart* instead, at the same hour/day/month
    // granularity the skills_day/month/year tables use for the XP chart, following the exact
    // same shape (see "add_skill_periods" above) - except `member_id` is BIGINT here, not
    // BIGSERIAL like skills_day/month/year's column (that's a foreign key referencing an
    // existing member, not an autoincrementing identity, so BIGSERIAL there is a latent typo
    // this migration does not repeat).
    if !has_migration_run(client, "add_bank_value_periods").await? {
        let transaction = client.transaction().await?;

        let periods = vec!["day", "month", "year"];
        for period in periods {
            let create_bank_value_aggregate = format!(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.bank_value_{} (
    member_id BIGINT REFERENCES groupscape.members(member_id),
    time TIMESTAMPTZ,
    bank_value BIGINT,

    PRIMARY KEY (member_id, time)
);
"#,
                period
            );
            transaction.execute(&create_bank_value_aggregate, &[]).await?;
        }

        transaction
            .execute(
                r#"
INSERT INTO groupscape.aggregation_info (type) VALUES ('bank_value')
ON CONFLICT (type) DO NOTHING
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_bank_value_periods").await?;
        transaction.commit().await?;
    }

    // Plugin auth redesign: the account API key replaces group_name/group_token as the
    // plugin's credential. Nullable rather than NOT NULL UNIQUE at the column level since
    // pre-existing accounts have none yet; `register`/Discord account-creation always set it
    // going forward, and the partial unique index only enforces uniqueness once a value exists.
    if !has_migration_run(client, "add_account_api_key_hash").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.accounts ADD COLUMN IF NOT EXISTS api_key_hash TEXT
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
CREATE UNIQUE INDEX IF NOT EXISTS accounts_api_key_hash_idx ON groupscape.accounts (api_key_hash) WHERE api_key_hash IS NOT NULL
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_account_api_key_hash").await?;
        transaction.commit().await?;
    }

    // A character auto-created from an unattended plugin request starts 'pending' until the
    // account owner confirms it on the site; the existing manual `link_character` fallback
    // still inserts 'confirmed' directly, same as before this column existed.
    if !has_migration_run(client, "add_character_status_column").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.characters ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'confirmed'
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.characters DROP CONSTRAINT IF EXISTS characters_status_check
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.characters ADD CONSTRAINT characters_status_check CHECK (status IN ('pending', 'confirmed'))
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_character_status_column").await?;
        transaction.commit().await?;
    }

    // Modeled on `blocked_members`: a per-account (not global) denylist. Removing a pending
    // character permanently blocks that RuneScape account from re-linking to *this* GroupScape
    // account - a real decision, not something the next plugin heartbeat silently undoes.
    if !has_migration_run(client, "create_character_denylist_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.character_denylist (
  account_id BIGINT NOT NULL REFERENCES groupscape.accounts(id) ON DELETE CASCADE,
  account_hash TEXT NOT NULL,
  denied_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (account_id, account_hash)
);
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_character_denylist_table").await?;
        transaction.commit().await?;
    }

    // Portrait meshes for a character with no group yet (pending confirmation) can't be keyed
    // through `member_mesh` (which requires a `members` row, itself group-scoped) - a separate
    // table keyed directly on `character_id` lets the identify/portrait routes work regardless
    // of group-link status.
    if !has_migration_run(client, "create_character_mesh_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.character_mesh (
  character_id BIGINT PRIMARY KEY REFERENCES groupscape.characters(character_id) ON DELETE CASCADE,
  mesh BYTEA NOT NULL,
  mesh_last_update TIMESTAMPTZ NOT NULL
);
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_character_mesh_table").await?;
        transaction.commit().await?;
    }

    // Lets the onboarding "confirm this character" card show a rough sense of the character
    // before it's in a group (full per-skill data is only tracked once a character has a group
    // to store it against). Reported by the plugin alongside RSN via `identify_character`;
    // both nullable since older plugin builds won't send them.
    if !has_migration_run(client, "add_character_summary_stats_columns").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.characters ADD COLUMN IF NOT EXISTS combat_level SMALLINT
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.characters ADD COLUMN IF NOT EXISTS total_level INTEGER
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_character_summary_stats_columns").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_members_color_column").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.members ADD COLUMN IF NOT EXISTS color TEXT
"#,
                &[],
            )
            .await?;

        // Backfill every pre-existing real member (excluding the synthetic shared-bank row) with
        // a colour, group by group, so "every member has a colour" stays a true invariant instead
        // of needing a NULL fallback throughout the read paths added for the helmet-colour
        // feature. New members from here on get one at insert time (see
        // `pick_unused_member_color`/`shuffled_color_palette` in this module).
        let group_ids_stmt = transaction
            .prepare_cached(
                "SELECT DISTINCT group_id FROM groupscape.members WHERE color IS NULL AND member_name != $1",
            )
            .await?;
        let group_ids: Vec<i64> = transaction
            .query(&group_ids_stmt, &[&SHARED_MEMBER])
            .await?
            .iter()
            .map(|row| row.try_get(0))
            .collect::<Result<_, _>>()?;

        let members_stmt = transaction
            .prepare_cached(
                "SELECT member_id FROM groupscape.members WHERE group_id=$1 AND member_name != $2 ORDER BY member_id ASC",
            )
            .await?;
        let set_color_stmt = transaction
            .prepare_cached("UPDATE groupscape.members SET color=$1 WHERE member_id=$2")
            .await?;
        for group_id in group_ids {
            let member_ids: Vec<i64> = transaction
                .query(&members_stmt, &[&group_id, &SHARED_MEMBER])
                .await?
                .iter()
                .map(|row| row.try_get(0))
                .collect::<Result<_, _>>()?;
            let colors = shuffled_color_palette();
            for (index, member_id) in member_ids.into_iter().enumerate() {
                let color = colors[index % colors.len()];
                transaction
                    .execute(&set_color_stmt, &[&color, &member_id])
                    .await?;
            }
        }

        commit_migration(&transaction, "add_members_color_column").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_account_status_columns").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.accounts ADD COLUMN IF NOT EXISTS status TEXT NOT NULL DEFAULT 'active'
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.accounts ADD CONSTRAINT accounts_status_check CHECK (status IN ('active', 'suspended', 'banned', 'deleted'))
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.accounts ADD COLUMN IF NOT EXISTS must_change_password BOOLEAN NOT NULL DEFAULT false
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.accounts ADD COLUMN IF NOT EXISTS last_login_at TIMESTAMPTZ
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.accounts ADD COLUMN IF NOT EXISTS failed_login_attempts INT NOT NULL DEFAULT 0
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.accounts ADD COLUMN IF NOT EXISTS locked_until TIMESTAMPTZ
"#,
                &[],
            )
            .await?;
        // Backfill: the old `disabled` flag becomes `status = 'banned'` - `disabled` itself is
        // kept around (unread from here on) rather than dropped, since dropping a column is a
        // one-way door and nothing in this migration set needs the space back.
        transaction
            .execute(
                r#"
UPDATE groupscape.accounts SET status = 'banned' WHERE disabled = true
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_account_status_columns").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_accounts_last_visit_at_column").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.accounts ADD COLUMN IF NOT EXISTS last_visit_at TIMESTAMPTZ
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_accounts_last_visit_at_column").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_account_sessions_ip_ua").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.account_sessions ADD COLUMN IF NOT EXISTS ip TEXT
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.account_sessions ADD COLUMN IF NOT EXISTS user_agent TEXT
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_account_sessions_ip_ua").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_item_bonuses_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.item_bonuses (
  item_id INT PRIMARY KEY,
  attack_stab INT NOT NULL,
  attack_slash INT NOT NULL,
  attack_crush INT NOT NULL,
  attack_magic INT NOT NULL,
  attack_ranged INT NOT NULL,
  defence_stab INT NOT NULL,
  defence_slash INT NOT NULL,
  defence_crush INT NOT NULL,
  defence_magic INT NOT NULL,
  defence_ranged INT NOT NULL,
  melee_strength INT NOT NULL,
  ranged_strength INT NOT NULL,
  magic_damage INT NOT NULL,
  prayer INT NOT NULL,
  attack_speed INT,
  fetched_at TIMESTAMPTZ NOT NULL
);
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_item_bonuses_table").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "allow_character_multi_account_link").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                "ALTER TABLE groupscape.characters DROP CONSTRAINT IF EXISTS characters_account_hash_key",
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
CREATE UNIQUE INDEX IF NOT EXISTS characters_account_id_account_hash_idx ON groupscape.characters (account_id, account_hash)
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "allow_character_multi_account_link").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_activity_events_npc_slug").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                "ALTER TABLE groupscape.activity_events ADD COLUMN IF NOT EXISTS npc_slug TEXT",
                &[],
            )
            .await?;
        // Composite indexes for the member/type-filtered branches of list_activity_events' cursor
        // query - the existing (group_id, occurred_at DESC) index only covers the unfiltered case.
        transaction
            .execute(
                r#"
CREATE INDEX IF NOT EXISTS activity_events_group_member_occurred_idx ON groupscape.activity_events (group_id, member_name, occurred_at DESC)
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
CREATE INDEX IF NOT EXISTS activity_events_group_type_occurred_idx ON groupscape.activity_events (group_id, event_type, occurred_at DESC)
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_activity_events_npc_slug").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "backfill_activity_events_npc_slug").await? {
        backfill_activity_events_npc_slug(client).await?;

        let transaction = client.transaction().await?;
        commit_migration(&transaction, "backfill_activity_events_npc_slug").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_groups_discord_notify_notable_drops_column").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.groups
ADD COLUMN IF NOT EXISTS discord_notify_notable_drops BOOLEAN NOT NULL DEFAULT true
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_groups_discord_notify_notable_drops_column").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "drop_farming_timers_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute("DROP TABLE IF EXISTS groupscape.farming_timers", &[])
            .await?;

        commit_migration(&transaction, "drop_farming_timers_table").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_members_last_seen_at_column").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.members
ADD COLUMN IF NOT EXISTS last_seen_at TIMESTAMPTZ
"#,
                &[],
            )
            .await?;
        // Backfill so already-online members don't read as stale the instant this ships.
        transaction
            .execute(
                &format!(
                    "UPDATE groupscape.members SET last_seen_at = {MEMBER_LAST_UPDATE_GREATEST} \
                     WHERE last_seen_at IS NULL"
                ),
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_members_last_seen_at_column").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_groups_discord_notify_raids_column").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.groups
ADD COLUMN IF NOT EXISTS discord_notify_raids BOOLEAN NOT NULL DEFAULT true
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_groups_discord_notify_raids_column").await?;
        transaction.commit().await?;
    }

    // Merges the old `discord_notify_loot`/`discord_notify_notable_drops` toggles into one
    // `discord_notify_drops` switch plus a gp-value floor (see `DiscordWebhookSettings`) - a
    // group that had either of the old toggles on keeps posting drops after this runs.
    if !has_migration_run(client, "merge_groups_discord_drops_columns").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.groups
ADD COLUMN IF NOT EXISTS discord_notify_drops BOOLEAN NOT NULL DEFAULT true,
ADD COLUMN IF NOT EXISTS discord_drops_min_value BIGINT NOT NULL DEFAULT 250000
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
UPDATE groupscape.groups
SET discord_notify_drops = (discord_notify_loot OR discord_notify_notable_drops)
"#,
                &[],
            )
            .await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.groups
DROP COLUMN IF EXISTS discord_notify_loot,
DROP COLUMN IF EXISTS discord_notify_notable_drops
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "merge_groups_discord_drops_columns").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_groups_discord_achievement_notify_columns").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.groups
ADD COLUMN IF NOT EXISTS discord_notify_combat_achievements BOOLEAN NOT NULL DEFAULT true,
ADD COLUMN IF NOT EXISTS discord_notify_collection_log BOOLEAN NOT NULL DEFAULT true,
ADD COLUMN IF NOT EXISTS discord_notify_quests BOOLEAN NOT NULL DEFAULT true,
ADD COLUMN IF NOT EXISTS discord_notify_diaries BOOLEAN NOT NULL DEFAULT true
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_groups_discord_achievement_notify_columns").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_groups_discord_pets_clues_notify_columns").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.groups
ADD COLUMN IF NOT EXISTS discord_notify_pets BOOLEAN NOT NULL DEFAULT true,
ADD COLUMN IF NOT EXISTS discord_notify_clues BOOLEAN NOT NULL DEFAULT true
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_groups_discord_pets_clues_notify_columns").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_groups_discord_level_ups_notify_column").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.groups
ADD COLUMN IF NOT EXISTS discord_notify_level_ups BOOLEAN NOT NULL DEFAULT true
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_groups_discord_level_ups_notify_column").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_groups_discord_level_up_interval_column").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.groups
ADD COLUMN IF NOT EXISTS discord_level_up_interval INTEGER NOT NULL DEFAULT 10
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_groups_discord_level_up_interval_column").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_activity_events_client_event_id").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                "ALTER TABLE groupscape.activity_events ADD COLUMN IF NOT EXISTS client_event_id TEXT",
                &[],
            )
            .await?;
        // Partial (NULLs, e.g. progress/raid events, never conflict with each other or anything
        // else) so a resent kill/death/loot event - identical `client_event_id`, same group - is
        // rejected by insert_activity_event_payload's ON CONFLICT DO NOTHING instead of landing
        // as a second row. See insert_activity_event_payload's doc comment for why this exists.
        transaction
            .execute(
                r#"
CREATE UNIQUE INDEX IF NOT EXISTS activity_events_group_client_event_id_idx ON groupscape.activity_events (group_id, client_event_id) WHERE client_event_id IS NOT NULL
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_activity_events_client_event_id").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_slayer_task_column").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.members
ADD COLUMN IF NOT EXISTS slayer_task_last_update TIMESTAMPTZ,
ADD COLUMN IF NOT EXISTS slayer_task TEXT
"#,
                &[],
            )
            .await?;

        create_timestamp_trigger(&transaction, "slayer_task").await?;

        commit_migration(&transaction, "add_slayer_task_column").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "add_groups_discord_drops_unique_only_column").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.groups
ADD COLUMN IF NOT EXISTS discord_drops_unique_only BOOLEAN NOT NULL DEFAULT false
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_groups_discord_drops_unique_only_column").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_slayer_task_history_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.slayer_task_history (
  history_id BIGSERIAL PRIMARY KEY,
  group_id BIGINT NOT NULL REFERENCES groupscape.groups(group_id) ON DELETE CASCADE,
  member_name CITEXT NOT NULL,
  client_event_id TEXT NOT NULL,
  task_name TEXT NOT NULL,
  master_name TEXT NOT NULL,
  status TEXT NOT NULL,
  amount_done INT NOT NULL DEFAULT 0,
  amount_total INT NOT NULL DEFAULT 0,
  points INT,
  assigned_at TIMESTAMPTZ NOT NULL,
  closed_at TIMESTAMPTZ
);
"#,
                &[],
            )
            .await?;
        // Lets the assignment event and that same task's later close event upsert onto one row
        // (see `upsert_slayer_task_history_event`) instead of the pair landing as two rows.
        transaction
            .execute(
                r#"
CREATE UNIQUE INDEX IF NOT EXISTS slayer_task_history_group_member_event_idx ON groupscape.slayer_task_history (group_id, member_name, client_event_id)
"#,
                &[],
            )
            .await?;
        // The History tab's page query and the Stats tab's aggregate query both filter/order on
        // this pair.
        transaction
            .execute(
                r#"
CREATE INDEX IF NOT EXISTS slayer_task_history_group_member_assigned_idx ON groupscape.slayer_task_history (group_id, member_name, assigned_at DESC)
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_slayer_task_history_table").await?;
        transaction.commit().await?;
    }

    // Group-wide toggle for the activity feed's like/comment reactions feature (§ likes/comments
    // ticket) - default true, following the same "on unless an admin turns it off" convention as
    // the `discord_notify_*` columns above rather than a separate group_settings table, since this
    // is the only non-Discord group-wide boolean the app has needed so far.
    if !has_migration_run(client, "add_groups_activity_reactions_enabled_column").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.groups
ADD COLUMN IF NOT EXISTS activity_reactions_enabled BOOLEAN NOT NULL DEFAULT true
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_groups_activity_reactions_enabled_column").await?;
        transaction.commit().await?;
    }

    // One reaction per (event, account) - reacting again with the same type deletes the row
    // (toggle off), a different type replaces it via the ON CONFLICT upsert in
    // `toggle_activity_reaction`. `reaction` is deliberately TEXT + CHECK rather than a Postgres
    // enum so adding a 6th reaction type later is just another migration, not an enum-alter dance.
    if !has_migration_run(client, "create_activity_event_reactions_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.activity_event_reactions (
  event_id BIGINT NOT NULL REFERENCES groupscape.activity_events(event_id) ON DELETE CASCADE,
  account_id BIGINT NOT NULL REFERENCES groupscape.accounts(id) ON DELETE CASCADE,
  reaction TEXT NOT NULL CHECK (reaction IN ('like', 'gg', 'lol', 'rare', 'f')),
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (event_id, account_id)
);
"#,
                &[],
            )
            .await?;
        // Covers both `get_reactions_summary_for_events`' `event_id = ANY($1)` batch lookup and
        // the per-event summary query's `GROUP BY reaction` - the PK above already covers
        // (event_id, account_id) point lookups.
        transaction
            .execute(
                r#"
CREATE INDEX IF NOT EXISTS activity_event_reactions_event_idx ON groupscape.activity_event_reactions (event_id)
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_activity_event_reactions_table").await?;
        transaction.commit().await?;
    }

    // `member_name` is denormalized at comment time (the commenting account's resolved member
    // name in this group, via `resolve_member_name_for_account`) rather than joined live - a
    // later in-game rename shouldn't rewrite history under an old comment, matching how
    // `activity_events.member_name` itself is a point-in-time snapshot, not a live FK.
    if !has_migration_run(client, "create_activity_event_comments_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.activity_event_comments (
  comment_id BIGSERIAL PRIMARY KEY,
  event_id BIGINT NOT NULL REFERENCES groupscape.activity_events(event_id) ON DELETE CASCADE,
  account_id BIGINT NOT NULL REFERENCES groupscape.accounts(id) ON DELETE CASCADE,
  member_name CITEXT NOT NULL,
  comment_text TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
"#,
                &[],
            )
            .await?;
        // Oldest-first listing plus the cap check (`COUNT(*) ... WHERE event_id=$1`) both key off
        // `event_id` alone; `created_at` in the index lets the listing query satisfy its ORDER BY
        // from the index instead of an extra sort.
        transaction
            .execute(
                r#"
CREATE INDEX IF NOT EXISTS activity_event_comments_event_idx ON groupscape.activity_event_comments (event_id, created_at ASC)
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_activity_event_comments_table").await?;
        transaction.commit().await?;
    }

    // Mortimer's task modifier - see `SlayerTask::modifier_type`'s doc comment. Nullable/no
    // backfill: only tasks assigned after this ships carry a modifier, existing rows just read
    // back with these columns null.
    if !has_migration_run(client, "add_slayer_task_history_modifier_columns").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
ALTER TABLE groupscape.slayer_task_history
ADD COLUMN IF NOT EXISTS modifier_type TEXT,
ADD COLUMN IF NOT EXISTS modifier_value INT,
ADD COLUMN IF NOT EXISTS modifier_negative BOOLEAN
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "add_slayer_task_history_modifier_columns").await?;
        transaction.commit().await?;
    }

    // One-time correction for a since-fixed plugin bug (GroupScapeTrackerPlugin#closeSlayerTask):
    // a task closed with `completed` whenever its kill count reached 0 remaining, even when the
    // player actually blocked/cancelled it (paying a fee) after finishing the kills but before
    // turning it in - so the fee's negative points landed on a row mislabeled "completed" instead
    // of "cancelled"/"blocked". Recognizable after the fact because a genuinely completed task
    // never has negative points (every master's own task reward is >= 0), so any "completed" row
    // whose points exactly match the flat cancel fee (30) or that master's known block price is
    // one of these - reclassify it rather than leave the mislabeled row around now that new events
    // are classified correctly going forward.
    if !has_migration_run(client, "fix_misclassified_slayer_cancel_block_rows").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
UPDATE groupscape.slayer_task_history
SET status = CASE WHEN points = -30 THEN 'cancelled' ELSE 'blocked' END
WHERE status = 'completed'
  AND amount_done = amount_total
  AND points < 0
  AND (
    points = -30
    OR (LOWER(master_name) IN ('turael', 'aya', 'spria') AND points = -40)
    OR (LOWER(master_name) IN ('mazchna', 'achtryn') AND points = -50)
    OR (LOWER(master_name) = 'vannaka' AND points = -60)
    OR (LOWER(master_name) = 'chaeldar' AND points = -70)
    OR (LOWER(master_name) = 'konar quo maten' AND points = -80)
    OR (LOWER(master_name) IN ('nieve', 'steve') AND points = -90)
    OR (LOWER(master_name) IN ('duradel', 'kuradal', 'krystilia') AND points = -100)
    OR (LOWER(master_name) = 'mortimer' AND points = -120)
  )
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "fix_misclassified_slayer_cancel_block_rows").await?;
        transaction.commit().await?;
    }

    if !has_migration_run(client, "create_chat_messages_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.chat_messages (
  message_id BIGSERIAL PRIMARY KEY,
  group_id BIGINT NOT NULL REFERENCES groupscape.groups(group_id) ON DELETE CASCADE,
  account_id BIGINT NOT NULL REFERENCES groupscape.accounts(id) ON DELETE CASCADE,
  member_name CITEXT,
  message_text TEXT NOT NULL,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
"#,
                &[],
            )
            .await?;
        // Backfill (message_id > cursor) and delivery ordering both key off group_id alone;
        // message_id DESC lets the "most recent N" backfill query satisfy its ORDER BY from
        // the index instead of an extra sort, same rationale as activity_event_comments' index.
        transaction
            .execute(
                r#"
CREATE INDEX IF NOT EXISTS chat_messages_group_id_idx ON groupscape.chat_messages (group_id, message_id DESC)
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_chat_messages_table").await?;
        transaction.commit().await?;
    }

    // Read cursor - distinct from the delivery cursor (§6's "since" backfill param). Own table
    // rather than a column on `accounts`: read state is per-(group, account), not account-global,
    // matching `chat_messages` itself being group-scoped.
    if !has_migration_run(client, "create_chat_read_cursors_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.chat_read_cursors (
  group_id BIGINT NOT NULL REFERENCES groupscape.groups(group_id) ON DELETE CASCADE,
  account_id BIGINT NOT NULL REFERENCES groupscape.accounts(id) ON DELETE CASCADE,
  last_read_message_id BIGINT NOT NULL DEFAULT 0,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (group_id, account_id)
);
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_chat_read_cursors_table").await?;
        transaction.commit().await?;
    }

    // Delivery cursor - per-(group, account), server-side, distinct from the read cursor above.
    // See the "!gs" chat spec §6: "Cursor is per-account, server-side (not per-device) - switching
    // devices doesn't look like a first-ever connect."
    if !has_migration_run(client, "create_chat_delivery_cursors_table").await? {
        let transaction = client.transaction().await?;
        transaction
            .execute(
                r#"
CREATE TABLE IF NOT EXISTS groupscape.chat_delivery_cursors (
  group_id BIGINT NOT NULL REFERENCES groupscape.groups(group_id) ON DELETE CASCADE,
  account_id BIGINT NOT NULL REFERENCES groupscape.accounts(id) ON DELETE CASCADE,
  last_delivered_message_id BIGINT NOT NULL DEFAULT 0,
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (group_id, account_id)
);
"#,
                &[],
            )
            .await?;

        commit_migration(&transaction, "create_chat_delivery_cursors_table").await?;
        transaction.commit().await?;
    }

    Ok(())
}

/// One-time backfill for existing `kill` rows written before `npc_slug` existed - computes the
/// slug the same way [`insert_activity_event_payload`] does going forward, so
/// `list_activity_events`' `npc_slug = ANY(...)` filter sees a consistent column for old and new
/// rows alike. Runs in small batches (rather than one giant transaction) since this walks
/// potentially every historical kill event.
async fn backfill_activity_events_npc_slug(client: &mut Client) -> Result<(), ApiError> {
    loop {
        let rows = client
            .query(
                r#"
SELECT event_id, payload FROM groupscape.activity_events
WHERE event_type = 'kill' AND npc_slug IS NULL
LIMIT 500
"#,
                &[],
            )
            .await?;
        if rows.is_empty() {
            return Ok(());
        }

        for row in &rows {
            let event_id: i64 = row.try_get("event_id")?;
            let payload: serde_json::Value = row.try_get("payload")?;
            let npc_name = payload
                .get("npcName")
                .or_else(|| payload.get("npc_name"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let slug = slugify_npc_name(npc_name);
            client
                .execute(
                    "UPDATE groupscape.activity_events SET npc_slug = $1 WHERE event_id = $2",
                    &[&slug, &event_id],
                )
                .await?;
        }
    }
}

/// Fresh (< 30-day-old) cached equipment bonuses for `item_id`, or `None` on a cache miss/expiry
/// - the caller (`item_bonuses::get_item_bonuses`) is responsible for re-scraping and upserting
/// in that case.
pub async fn get_cached_item_bonuses(
    client: &Client,
    item_id: i32,
) -> Result<Option<ItemBonusesResponse>, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
SELECT attack_stab, attack_slash, attack_crush, attack_magic, attack_ranged,
       defence_stab, defence_slash, defence_crush, defence_magic, defence_ranged,
       melee_strength, ranged_strength, magic_damage, prayer, attack_speed
FROM groupscape.item_bonuses
WHERE item_id=$1 AND fetched_at >= NOW() - INTERVAL '30 days'
"#,
        )
        .await?;
    let row = client
        .query_opt(&stmt, &[&item_id])
        .await
        .map_err(ApiError::GetItemBonusesError)?;
    let Some(row) = row else {
        return Ok(None);
    };

    Ok(Some(ItemBonusesResponse {
        item_id,
        attack: CombatStyleBonuses {
            stab: row.try_get("attack_stab")?,
            slash: row.try_get("attack_slash")?,
            crush: row.try_get("attack_crush")?,
            magic: row.try_get("attack_magic")?,
            ranged: row.try_get("attack_ranged")?,
        },
        defence: CombatStyleBonuses {
            stab: row.try_get("defence_stab")?,
            slash: row.try_get("defence_slash")?,
            crush: row.try_get("defence_crush")?,
            magic: row.try_get("defence_magic")?,
            ranged: row.try_get("defence_ranged")?,
        },
        melee_strength: row.try_get("melee_strength")?,
        ranged_strength: row.try_get("ranged_strength")?,
        magic_damage: row.try_get("magic_damage")?,
        prayer: row.try_get("prayer")?,
        attack_speed: row.try_get("attack_speed")?,
    }))
}

pub async fn upsert_item_bonuses(
    client: &Client,
    bonuses: &ItemBonusesResponse,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
INSERT INTO groupscape.item_bonuses (
  item_id, attack_stab, attack_slash, attack_crush, attack_magic, attack_ranged,
  defence_stab, defence_slash, defence_crush, defence_magic, defence_ranged,
  melee_strength, ranged_strength, magic_damage, prayer, attack_speed, fetched_at
) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16, NOW())
ON CONFLICT (item_id) DO UPDATE SET
  attack_stab=excluded.attack_stab, attack_slash=excluded.attack_slash, attack_crush=excluded.attack_crush,
  attack_magic=excluded.attack_magic, attack_ranged=excluded.attack_ranged,
  defence_stab=excluded.defence_stab, defence_slash=excluded.defence_slash, defence_crush=excluded.defence_crush,
  defence_magic=excluded.defence_magic, defence_ranged=excluded.defence_ranged,
  melee_strength=excluded.melee_strength, ranged_strength=excluded.ranged_strength,
  magic_damage=excluded.magic_damage, prayer=excluded.prayer, attack_speed=excluded.attack_speed,
  fetched_at=excluded.fetched_at
"#,
        )
        .await?;
    client
        .execute(
            &stmt,
            &[
                &bonuses.item_id,
                &bonuses.attack.stab,
                &bonuses.attack.slash,
                &bonuses.attack.crush,
                &bonuses.attack.magic,
                &bonuses.attack.ranged,
                &bonuses.defence.stab,
                &bonuses.defence.slash,
                &bonuses.defence.crush,
                &bonuses.defence.magic,
                &bonuses.defence.ranged,
                &bonuses.melee_strength,
                &bonuses.ranged_strength,
                &bonuses.magic_damage,
                &bonuses.prayer,
                &bonuses.attack_speed,
            ],
        )
        .await
        .map_err(ApiError::UpsertItemBonusesError)?;
    Ok(())
}

pub struct AccountForAuth {
    pub id: i64,
    pub username: Option<String>,
    pub discord_name: Option<String>,
    pub password_hash: Option<String>,
    pub disabled: bool,
    pub created_at: DateTime<Utc>,
    pub status: String,
    pub must_change_password: bool,
    pub failed_login_attempts: i32,
    pub locked_until: Option<DateTime<Utc>>,
    pub last_login_at: Option<DateTime<Utc>>,
}

const ACCOUNT_FOR_AUTH_COLUMNS: &str = "id, username, discord_name, password_hash, disabled, created_at, status, must_change_password, failed_login_attempts, locked_until, last_login_at";

fn account_for_auth_from_row(row: Row) -> Result<AccountForAuth, ApiError> {
    Ok(AccountForAuth {
        id: row.try_get("id")?,
        username: row.try_get("username")?,
        discord_name: row.try_get("discord_name")?,
        password_hash: row.try_get("password_hash")?,
        disabled: row.try_get("disabled")?,
        created_at: row.try_get("created_at")?,
        status: row.try_get("status")?,
        must_change_password: row.try_get("must_change_password")?,
        failed_login_attempts: row.try_get("failed_login_attempts")?,
        locked_until: row.try_get("locked_until")?,
        last_login_at: row.try_get("last_login_at")?,
    })
}

impl From<AccountForAuth> for crate::models::Account {
    fn from(account: AccountForAuth) -> Self {
        crate::models::Account {
            id: account.id,
            username: account.username,
            discord_name: account.discord_name,
            created_at: account.created_at,
            must_change_password: account.must_change_password,
        }
    }
}

/// One account per user - `username` is a case-insensitive (citext) UNIQUE column, so a duplicate
/// registration surfaces as a Postgres unique-violation (SQLSTATE 23505) rather than needing a
/// separate existence check that would race with a concurrent registration of the same username.
/// Creates a Discord-only account (`username`/`password_hash` both left `NULL`) - matches
/// `groupscape-old`'s OAuth-first decision (grilled during that project's Slice 29): a Discord
/// id with no matching account auto-creates one rather than requiring a prior username signup.
pub async fn create_account_with_discord_id(
    client: &Client,
    discord_id: &str,
) -> Result<i64, ApiError> {
    let stmt = client
        .prepare_cached("INSERT INTO groupscape.accounts (discord_id) VALUES ($1) RETURNING id")
        .await?;
    match client.query_one(&stmt, &[&discord_id]).await {
        Ok(row) => Ok(row.try_get(0)?),
        Err(err) => {
            if err.as_db_error().is_some_and(|db_err| {
                db_err.code() == &tokio_postgres::error::SqlState::UNIQUE_VIOLATION
            }) {
                Err(ApiError::DiscordIdAlreadyLinkedError)
            } else {
                Err(ApiError::CreateAccountError(err))
            }
        }
    }
}

/// Attaches a Discord identity to an *already-existing* account (the "link Discord" flow for a
/// user who registered with a username/password first) rather than creating a second, empty
/// account the way `create_account_with_discord_id` does for a brand-new Discord login. The
/// partial unique index on `discord_id` still enforces one account per Discord identity, so a
/// Discord account already linked elsewhere surfaces as `DiscordIdAlreadyLinkedError`.
pub async fn link_discord_id_to_account(
    client: &Client,
    account_id: i64,
    discord_id: &str,
    discord_name: &str,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached("UPDATE groupscape.accounts SET discord_id=$2, discord_name=$3 WHERE id=$1")
        .await?;
    client
        .execute(&stmt, &[&account_id, &discord_id, &discord_name])
        .await
        .map_err(|err| {
            if err.as_db_error().is_some_and(|db_err| {
                db_err.code() == &tokio_postgres::error::SqlState::UNIQUE_VIOLATION
            }) {
                ApiError::DiscordIdAlreadyLinkedError
            } else {
                ApiError::GetAccountError(err)
            }
        })?;
    Ok(())
}

pub async fn update_account_discord_name(
    client: &Client,
    discord_id: &str,
    discord_name: &str,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached("UPDATE groupscape.accounts SET discord_name=$2 WHERE discord_id=$1")
        .await?;
    client
        .execute(&stmt, &[&discord_id, &discord_name])
        .await
        .map_err(ApiError::GetAccountError)?;
    Ok(())
}

pub async fn get_account_by_discord_id(
    client: &Client,
    discord_id: &str,
) -> Result<Option<AccountForAuth>, ApiError> {
    let stmt = client
        .prepare_cached(&format!(
            "SELECT {ACCOUNT_FOR_AUTH_COLUMNS} FROM groupscape.accounts WHERE discord_id=$1"
        ))
        .await?;
    let row = client
        .query_opt(&stmt, &[&discord_id])
        .await
        .map_err(ApiError::GetAccountError)?;
    match row {
        Some(row) => Ok(Some(account_for_auth_from_row(row)?)),
        None => Ok(None),
    }
}

pub async fn create_account(
    client: &Client,
    username: &str,
    password_hash: &str,
) -> Result<i64, ApiError> {
    let stmt = client
        .prepare_cached(
            "INSERT INTO groupscape.accounts (username, password_hash) VALUES ($1, $2) RETURNING id",
        )
        .await?;
    match client.query_one(&stmt, &[&username, &password_hash]).await {
        Ok(row) => Ok(row.try_get(0)?),
        Err(err) => {
            if err.as_db_error().is_some_and(|db_err| {
                db_err.code() == &tokio_postgres::error::SqlState::UNIQUE_VIOLATION
            }) {
                Err(ApiError::UsernameAlreadyRegisteredError)
            } else {
                Err(ApiError::CreateAccountError(err))
            }
        }
    }
}

pub async fn get_account_by_username(
    client: &Client,
    username: &str,
) -> Result<Option<AccountForAuth>, ApiError> {
    let stmt = client
        .prepare_cached(&format!(
            "SELECT {ACCOUNT_FOR_AUTH_COLUMNS} FROM groupscape.accounts WHERE username=$1"
        ))
        .await?;
    let row = client
        .query_opt(&stmt, &[&username])
        .await
        .map_err(ApiError::GetAccountError)?;
    match row {
        Some(row) => Ok(Some(account_for_auth_from_row(row)?)),
        None => Ok(None),
    }
}

pub async fn get_account_by_id(
    client: &Client,
    account_id: i64,
) -> Result<Option<AccountForAuth>, ApiError> {
    let stmt = client
        .prepare_cached(&format!(
            "SELECT {ACCOUNT_FOR_AUTH_COLUMNS} FROM groupscape.accounts WHERE id=$1"
        ))
        .await?;
    let row = client
        .query_opt(&stmt, &[&account_id])
        .await
        .map_err(ApiError::GetAccountError)?;
    match row {
        Some(row) => Ok(Some(account_for_auth_from_row(row)?)),
        None => Ok(None),
    }
}

/// Increments the failed-login counter and, once it crosses the threshold, sets `locked_until`
/// 15 minutes out. Returns the row's post-update `locked_until` so the caller can tell whether
/// this specific attempt is the one that tripped the lock.
pub async fn record_failed_login(
    client: &Client,
    account_id: i64,
) -> Result<Option<DateTime<Utc>>, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
UPDATE groupscape.accounts
SET failed_login_attempts = failed_login_attempts + 1,
    locked_until = CASE
      WHEN failed_login_attempts + 1 >= $2 THEN now() + ($3 || ' minutes')::interval
      ELSE locked_until
    END
WHERE id = $1
RETURNING locked_until
"#,
        )
        .await?;
    let row = client
        .query_one(
            &stmt,
            &[
                &account_id,
                &ACCOUNT_LOCKOUT_THRESHOLD,
                &ACCOUNT_LOCKOUT_MINUTES.to_string(),
            ],
        )
        .await
        .map_err(ApiError::GetAccountError)?;
    Ok(row.try_get("locked_until")?)
}

/// Clears the lockout counters and records a successful login timestamp - called on a
/// successful password check. Also stamps `last_visit_at`, since a login is itself a visit.
pub async fn reset_login_lockout_and_record_login(
    client: &Client,
    account_id: i64,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            "UPDATE groupscape.accounts SET failed_login_attempts = 0, locked_until = NULL, last_login_at = now(), last_visit_at = now() WHERE id = $1",
        )
        .await?;
    client
        .execute(&stmt, &[&account_id])
        .await
        .map_err(ApiError::GetAccountError)?;
    Ok(())
}

/// Stamps `last_visit_at` for an authenticated account - called from the account-session auth
/// middleware on a cache miss, which naturally throttles writes to once per
/// `ACCOUNT_AUTH_CACHE_TTL` per session token rather than on every request.
pub async fn record_account_visit(client: &Client, account_id: i64) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached("UPDATE groupscape.accounts SET last_visit_at = now() WHERE id = $1")
        .await?;
    client
        .execute(&stmt, &[&account_id])
        .await
        .map_err(ApiError::GetAccountError)?;
    Ok(())
}

/// `username` is `citext UNIQUE`, so a duplicate update surfaces as a unique-violation same as
/// `create_account` above.
pub async fn update_account_username(
    client: &Client,
    account_id: i64,
    username: &str,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached("UPDATE groupscape.accounts SET username=$1 WHERE id=$2")
        .await?;
    match client.execute(&stmt, &[&username, &account_id]).await {
        Ok(_) => Ok(()),
        Err(err) => {
            if err.as_db_error().is_some_and(|db_err| {
                db_err.code() == &tokio_postgres::error::SqlState::UNIQUE_VIOLATION
            }) {
                Err(ApiError::UsernameAlreadyRegisteredError)
            } else {
                Err(ApiError::UpdateAccountUsernameError(err))
            }
        }
    }
}

pub async fn update_account_password(
    client: &Client,
    account_id: i64,
    password_hash: &str,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached("UPDATE groupscape.accounts SET password_hash=$1 WHERE id=$2")
        .await?;
    client
        .execute(&stmt, &[&password_hash, &account_id])
        .await
        .map_err(ApiError::UpdateAccountPasswordError)?;
    Ok(())
}

/// Admin-triggered password reset: sets a fresh (temp) password hash and flips
/// `must_change_password` so the account is forced through the change-password gate on its
/// next authed request. Existing sessions are revoked separately via
/// `admin_revoke_all_account_sessions`.
pub async fn admin_reset_account_password(
    client: &Client,
    account_id: i64,
    password_hash: &str,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            "UPDATE groupscape.accounts SET password_hash=$1, must_change_password=true WHERE id=$2",
        )
        .await?;
    client
        .execute(&stmt, &[&password_hash, &account_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminResetAccountPasswordError".to_string(), e))?;
    Ok(())
}

/// Clears the forced-password-change flag - set by `change_password` once the account has
/// actually changed it (whether that change was self-service or in response to an admin reset).
pub async fn clear_must_change_password(client: &Client, account_id: i64) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            "UPDATE groupscape.accounts SET must_change_password=false WHERE id=$1",
        )
        .await?;
    client
        .execute(&stmt, &[&account_id])
        .await
        .map_err(ApiError::UpdateAccountPasswordError)?;
    Ok(())
}

/// Hard delete - `characters` (`ON DELETE CASCADE` from `accounts`), `character_group_links`
/// (cascades again from `characters`), and `account_sessions` (`ON DELETE CASCADE` from
/// `accounts`) all clean up via existing FK constraints, so a single row delete is enough.
/// Groups themselves are untouched: group ownership isn't tracked against accounts yet.
pub async fn delete_account(client: &Client, account_id: i64) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached("DELETE FROM groupscape.accounts WHERE id=$1")
        .await?;
    client
        .execute(&stmt, &[&account_id])
        .await
        .map_err(ApiError::DeleteAccountError)?;
    Ok(())
}

pub async fn create_account_session(
    client: &Client,
    account_id: i64,
    token_hash: &str,
    expires_at: &DateTime<Utc>,
) -> Result<(), ApiError> {
    create_account_session_with_meta(client, account_id, token_hash, expires_at, None, None).await
}

/// Same as `create_account_session`, capturing the request's IP/user-agent for the admin
/// per-account session list. Both are best-effort (nullable) - kept as a separate function
/// rather than changing `create_account_session`'s signature so the many existing test call
/// sites that don't care about IP/UA don't need touching.
pub async fn create_account_session_with_meta(
    client: &Client,
    account_id: i64,
    token_hash: &str,
    expires_at: &DateTime<Utc>,
    ip: Option<&str>,
    user_agent: Option<&str>,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            "INSERT INTO groupscape.account_sessions (account_id, token_hash, expires_at, ip, user_agent) VALUES ($1, $2, $3, $4, $5)",
        )
        .await?;
    client
        .execute(&stmt, &[&account_id, &token_hash, expires_at, &ip, &user_agent])
        .await
        .map_err(ApiError::CreateAccountSessionError)?;
    Ok(())
}

pub async fn get_account_by_session_token_hash(
    client: &Client,
    token_hash: &str,
) -> Result<Option<crate::models::Account>, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
SELECT a.id, a.username, a.discord_name, a.created_at, a.must_change_password
FROM groupscape.account_sessions s
INNER JOIN groupscape.accounts a ON a.id = s.account_id
WHERE s.token_hash = $1 AND s.expires_at > NOW() AND a.status = 'active'
"#,
        )
        .await?;
    let row = client
        .query_opt(&stmt, &[&token_hash])
        .await
        .map_err(ApiError::GetAccountError)?;
    match row {
        Some(row) => Ok(Some(crate::models::Account {
            id: row.try_get("id")?,
            username: row.try_get("username")?,
            discord_name: row.try_get("discord_name")?,
            created_at: row.try_get("created_at")?,
            must_change_password: row.try_get("must_change_password")?,
        })),
        None => Ok(None),
    }
}

/// Ported from `groupscape-old`'s `characters` repository - a per-account cap keeps one
/// account from squatting on an unbounded number of RuneScape accounts.
pub const CHARACTER_CAP_PER_ACCOUNT: i64 = 5;

pub struct Character {
    pub id: i64,
    pub account_id: i64,
    pub account_hash: String,
    pub display_rsn: String,
    pub bound_at: DateTime<Utc>,
    pub status: String,
    pub combat_level: Option<i16>,
    pub total_level: Option<i32>,
}

const CHARACTER_COLUMNS: &str =
    "character_id, account_id, account_hash, display_rsn, bound_at, status, combat_level, total_level";

fn character_from_row(row: Row) -> Result<Character, ApiError> {
    Ok(Character {
        id: row.try_get("character_id")?,
        account_id: row.try_get("account_id")?,
        account_hash: row.try_get("account_hash")?,
        display_rsn: row.try_get("display_rsn")?,
        bound_at: row.try_get("bound_at")?,
        status: row.try_get("status")?,
        combat_level: row.try_get("combat_level")?,
        total_level: row.try_get("total_level")?,
    })
}

/// Any account's row for this `account_hash` - since a character can now be linked to more than
/// one account, this returns an arbitrary match and should only be used where the caller doesn't
/// care which account it belongs to (e.g. denylist-adjacent checks). Prefer
/// `find_character_by_account_and_hash` (single account) or
/// `find_character_by_account_hash_and_group` (this group's linked account) wherever the caller
/// needs a specific one.
pub async fn find_character_by_account_hash(
    client: &Client,
    account_hash: &str,
) -> Result<Option<Character>, ApiError> {
    let stmt = client
        .prepare_cached(&format!(
            "SELECT {CHARACTER_COLUMNS} FROM groupscape.characters WHERE account_hash=$1"
        ))
        .await?;
    let row = client
        .query_opt(&stmt, &[&account_hash])
        .await
        .map_err(ApiError::GetCharacterError)?;
    match row {
        Some(row) => Ok(Some(character_from_row(row)?)),
        None => Ok(None),
    }
}

/// This account's own row for `account_hash`, if it has linked this character - distinct from
/// `find_character_by_account_hash`, since the same `account_hash` can now have a row under
/// multiple accounts.
pub async fn find_character_by_account_and_hash(
    client: &Client,
    account_id: i64,
    account_hash: &str,
) -> Result<Option<Character>, ApiError> {
    let stmt = client
        .prepare_cached(&format!(
            "SELECT {CHARACTER_COLUMNS} FROM groupscape.characters WHERE account_id=$1 AND account_hash=$2"
        ))
        .await?;
    let row = client
        .query_opt(&stmt, &[&account_id, &account_hash])
        .await
        .map_err(ApiError::GetCharacterError)?;
    match row {
        Some(row) => Ok(Some(character_from_row(row)?)),
        None => Ok(None),
    }
}

/// The specific account's character row for `account_hash` that is linked to `group_id` - used
/// where multiple accounts may have linked the same `account_hash` and the caller needs the one
/// tied to this particular group's roster.
pub async fn find_character_by_account_hash_and_group(
    client: &Client,
    account_hash: &str,
    group_id: i64,
) -> Result<Option<Character>, ApiError> {
    let stmt = client
        .prepare_cached(&format!(
            "SELECT c.character_id, c.account_id, c.account_hash, c.display_rsn, c.bound_at, c.status, c.combat_level, c.total_level \
             FROM groupscape.characters c \
             JOIN groupscape.character_group_links cgl ON cgl.character_id = c.character_id \
             WHERE c.account_hash=$1 AND cgl.group_id=$2"
        ))
        .await?;
    let row = client
        .query_opt(&stmt, &[&account_hash, &group_id])
        .await
        .map_err(ApiError::GetCharacterError)?;
    match row {
        Some(row) => Ok(Some(character_from_row(row)?)),
        None => Ok(None),
    }
}

pub async fn count_characters_for_account(
    client: &Client,
    account_id: i64,
) -> Result<i64, ApiError> {
    let stmt = client
        .prepare_cached("SELECT COUNT(*) FROM groupscape.characters WHERE account_id=$1")
        .await?;
    let row = client
        .query_one(&stmt, &[&account_id])
        .await
        .map_err(ApiError::GetCharacterError)?;
    Ok(row.try_get(0)?)
}

pub async fn create_character(
    client: &Client,
    account_id: i64,
    account_hash: &str,
    display_rsn: &str,
) -> Result<Character, ApiError> {
    let stmt = client
        .prepare_cached(&format!(
            "INSERT INTO groupscape.characters (account_id, account_hash, display_rsn, status) VALUES ($1, $2, $3, 'confirmed') RETURNING {CHARACTER_COLUMNS}"
        ))
        .await?;
    let row = client
        .query_one(&stmt, &[&account_id, &account_hash, &display_rsn])
        .await
        .map_err(ApiError::CreateCharacterError)?;
    character_from_row(row)
}

/// Auto-created by the plugin-facing auth middleware the first time it sees an unrecognized
/// `account_hash` under a valid API key - starts `pending` (no RSN known yet; the real RSN
/// arrives via `identify_character`, called independently of group-link status) until the
/// account owner confirms it on the site.
pub async fn create_pending_character(
    client: &Client,
    account_id: i64,
    account_hash: &str,
) -> Result<Character, ApiError> {
    let stmt = client
        .prepare_cached(&format!(
            "INSERT INTO groupscape.characters (account_id, account_hash, display_rsn, status) VALUES ($1, $2, $2, 'pending') RETURNING {CHARACTER_COLUMNS}"
        ))
        .await?;
    let row = client
        .query_one(&stmt, &[&account_id, &account_hash])
        .await
        .map_err(ApiError::CreateCharacterError)?;
    character_from_row(row)
}

pub async fn update_character_display_rsn(
    client: &Client,
    character_id: i64,
    display_rsn: &str,
    combat_level: Option<i16>,
    total_level: Option<i32>,
) -> Result<Character, ApiError> {
    let stmt = client
        .prepare_cached(&format!(
            "UPDATE groupscape.characters SET display_rsn=$1, combat_level=COALESCE($3, combat_level), total_level=COALESCE($4, total_level) WHERE character_id=$2 RETURNING {CHARACTER_COLUMNS}"
        ))
        .await?;
    let row = client
        .query_one(
            &stmt,
            &[&display_rsn, &character_id, &combat_level, &total_level],
        )
        .await
        .map_err(ApiError::GetCharacterError)?;
    character_from_row(row)
}

/// Confirms a pending character (no-op'd to nothing if it's already confirmed or doesn't
/// exist - the caller distinguishes those via `find_character_by_id`/ownership checks first).
pub async fn confirm_character(client: &Client, character_id: i64) -> Result<Character, ApiError> {
    let stmt = client
        .prepare_cached(&format!(
            "UPDATE groupscape.characters SET status='confirmed' WHERE character_id=$1 AND status='pending' RETURNING {CHARACTER_COLUMNS}"
        ))
        .await?;
    let row = client
        .query_opt(&stmt, &[&character_id])
        .await
        .map_err(ApiError::GetCharacterError)?
        .ok_or(ApiError::CharacterNotFoundError)?;
    character_from_row(row)
}

pub async fn find_character_by_id(
    client: &Client,
    character_id: i64,
) -> Result<Option<Character>, ApiError> {
    let stmt = client
        .prepare_cached(&format!(
            "SELECT {CHARACTER_COLUMNS} FROM groupscape.characters WHERE character_id=$1"
        ))
        .await?;
    let row = client
        .query_opt(&stmt, &[&character_id])
        .await
        .map_err(ApiError::GetCharacterError)?;
    match row {
        Some(row) => Ok(Some(character_from_row(row)?)),
        None => Ok(None),
    }
}

/// A confirmed character's row plus whether it already has a group, single query via
/// `LEFT JOIN` - lets the onboarding flow tell "needs a group" apart from "already has one"
/// without an extra round trip per character.
pub struct CharacterWithGroupStatus {
    pub character: Character,
    pub group_id: Option<i64>,
    /// Needed by the site to actually navigate into the group (its dashboard routes are keyed
    /// by name, not id) - see `find_group_id_for_account_character` for why name alone can't
    /// be trusted the other direction (name -> id).
    pub group_name: Option<String>,
}

pub async fn list_characters_for_account_with_group_status(
    client: &Client,
    account_id: i64,
) -> Result<Vec<CharacterWithGroupStatus>, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT c.character_id, c.account_id, c.account_hash, c.display_rsn, c.bound_at, \
             c.status, c.combat_level, c.total_level, l.group_id AS link_group_id, \
             g.group_name AS link_group_name \
             FROM groupscape.characters c \
             LEFT JOIN groupscape.character_group_links l ON l.character_id = c.character_id \
             LEFT JOIN groupscape.groups g ON g.group_id = l.group_id \
             WHERE c.account_id=$1 ORDER BY c.bound_at ASC",
        )
        .await?;
    let rows = client
        .query(&stmt, &[&account_id])
        .await
        .map_err(ApiError::GetCharacterError)?;
    rows.into_iter()
        .map(|row| {
            let group_id: Option<i64> = row.try_get("link_group_id")?;
            let group_name: Option<String> = row.try_get("link_group_name")?;
            Ok(CharacterWithGroupStatus {
                character: character_from_row(row)?,
                group_id,
                group_name,
            })
        })
        .collect()
}

/// Unlinks a character from its account. `character_group_links` references `character_id`
/// with `ON DELETE CASCADE`, so this also drops any group membership the character held -
/// unlike `groupscape-old`, which lacked that cascade and had to delete the link row itself
/// first.
pub async fn delete_character(client: &Client, character_id: i64) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached("DELETE FROM groupscape.characters WHERE character_id=$1")
        .await?;
    client
        .execute(&stmt, &[&character_id])
        .await
        .map_err(ApiError::DeleteCharacterError)?;
    Ok(())
}

pub async fn is_character_denylisted(
    client: &Client,
    account_id: i64,
    account_hash: &str,
) -> Result<bool, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM groupscape.character_denylist WHERE account_id=$1 AND account_hash=$2)",
        )
        .await?;
    let row = client
        .query_one(&stmt, &[&account_id, &account_hash])
        .await
        .map_err(ApiError::GetCharacterError)?;
    Ok(row.try_get(0)?)
}

pub async fn denylist_character(
    client: &Client,
    account_id: i64,
    account_hash: &str,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            "INSERT INTO groupscape.character_denylist (account_id, account_hash) VALUES ($1, $2) ON CONFLICT (account_id, account_hash) DO NOTHING",
        )
        .await?;
    client
        .execute(&stmt, &[&account_id, &account_hash])
        .await
        .map_err(ApiError::DeleteCharacterError)?;
    Ok(())
}

/// Removes a *pending* character and permanently denylists its account_hash in one transaction,
/// so the next plugin heartbeat can't silently recreate it. Confirmed-character removal stays on
/// the plain `delete_character` (no denylist) - only a pending-card "Remove" denylists.
pub async fn remove_pending_character(
    client: &mut Client,
    account_id: i64,
    character_id: i64,
    account_hash: &str,
) -> Result<(), ApiError> {
    let transaction = client.transaction().await?;

    let delete_stmt = transaction
        .prepare_cached("DELETE FROM groupscape.characters WHERE character_id=$1")
        .await?;
    transaction
        .execute(&delete_stmt, &[&character_id])
        .await
        .map_err(ApiError::DeleteCharacterError)?;

    let denylist_stmt = transaction
        .prepare_cached(
            "INSERT INTO groupscape.character_denylist (account_id, account_hash) VALUES ($1, $2) ON CONFLICT (account_id, account_hash) DO NOTHING",
        )
        .await?;
    transaction
        .execute(&denylist_stmt, &[&account_id, &account_hash])
        .await
        .map_err(ApiError::DeleteCharacterError)?;

    transaction.commit().await?;
    Ok(())
}

pub async fn get_account_by_api_key_hash(
    client: &Client,
    api_key_hash: &str,
) -> Result<Option<crate::models::Account>, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT id, username, discord_name, created_at, must_change_password FROM groupscape.accounts WHERE api_key_hash=$1 AND status='active'",
        )
        .await?;
    let row = client
        .query_opt(&stmt, &[&api_key_hash])
        .await
        .map_err(ApiError::GetAccountError)?;
    match row {
        Some(row) => Ok(Some(crate::models::Account {
            id: row.try_get("id")?,
            username: row.try_get("username")?,
            discord_name: row.try_get("discord_name")?,
            created_at: row.try_get("created_at")?,
            must_change_password: row.try_get("must_change_password")?,
        })),
        None => Ok(None),
    }
}

pub async fn set_account_api_key_hash(
    client: &Client,
    account_id: i64,
    api_key_hash: &str,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached("UPDATE groupscape.accounts SET api_key_hash=$1 WHERE id=$2")
        .await?;
    client
        .execute(&stmt, &[&api_key_hash, &account_id])
        .await
        .map_err(ApiError::GetAccountError)?;
    Ok(())
}

/// Portrait mesh for a character, keyed directly on `character_id` rather than `(group_id,
/// member_name)` like `member_mesh` - so it works for a pending character with no group yet.
pub async fn upsert_character_mesh(
    client: &Client,
    character_id: i64,
    mesh: &[u8],
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
INSERT INTO groupscape.character_mesh (character_id, mesh, mesh_last_update) VALUES ($1, $2, NOW())
ON CONFLICT (character_id) DO UPDATE SET mesh=excluded.mesh, mesh_last_update=excluded.mesh_last_update
"#,
        )
        .await?;
    client
        .execute(&stmt, &[&character_id, &mesh])
        .await
        .map_err(ApiError::UpsertMemberMeshError)?;
    Ok(())
}

pub async fn get_character_mesh(
    client: &Client,
    character_id: i64,
) -> Result<Option<Vec<u8>>, ApiError> {
    let stmt = client
        .prepare_cached("SELECT mesh FROM groupscape.character_mesh WHERE character_id=$1")
        .await?;
    let row = client
        .query_opt(&stmt, &[&character_id])
        .await
        .map_err(ApiError::GetMemberMeshError)?;
    match row {
        Some(row) => Ok(Some(row.try_get("mesh")?)),
        None => Ok(None),
    }
}

pub struct PushSubscription {
    pub id: i64,
    pub account_id: i64,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

/// Upserts on `endpoint` conflict, matching `groupscape-old`'s design: re-subscribing the same
/// endpoint (e.g. the browser rotated its keys) replaces the stored keys in place rather than
/// erroring or accumulating duplicates. Per-device, not per-account-singleton - one account can
/// hold many rows, one per browser/device it subscribed from.
pub async fn upsert_push_subscription(
    client: &Client,
    account_id: i64,
    endpoint: &str,
    p256dh: &str,
    auth: &str,
) -> Result<PushSubscription, ApiError> {
    let stmt = client
        .prepare_cached(
            "INSERT INTO groupscape.push_subscriptions (account_id, endpoint, p256dh, auth) VALUES ($1, $2, $3, $4)
             ON CONFLICT (endpoint) DO UPDATE SET account_id=EXCLUDED.account_id, p256dh=EXCLUDED.p256dh, auth=EXCLUDED.auth
             RETURNING subscription_id, account_id, endpoint, p256dh, auth",
        )
        .await?;
    let row = client
        .query_one(&stmt, &[&account_id, &endpoint, &p256dh, &auth])
        .await
        .map_err(ApiError::UpsertPushSubscriptionError)?;
    Ok(PushSubscription {
        id: row.try_get("subscription_id")?,
        account_id: row.try_get("account_id")?,
        endpoint: row.try_get("endpoint")?,
        p256dh: row.try_get("p256dh")?,
        auth: row.try_get("auth")?,
    })
}

/// Ownership-scoped: only removes the row if it belongs to `account_id`, mirroring
/// `unlink_character`'s ownership check on the caller side without a separate lookup query.
pub async fn delete_push_subscription(
    client: &Client,
    account_id: i64,
    endpoint: &str,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            "DELETE FROM groupscape.push_subscriptions WHERE account_id=$1 AND endpoint=$2",
        )
        .await?;
    client
        .execute(&stmt, &[&account_id, &endpoint])
        .await
        .map_err(ApiError::DeletePushSubscriptionError)?;
    Ok(())
}

/// No account_id scoping - a `410 Gone`/`404 Not Found` response from the push service means the
/// endpoint itself is dead, regardless of which account currently owns the row.
pub async fn delete_push_subscription_by_endpoint(
    client: &Client,
    endpoint: &str,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached("DELETE FROM groupscape.push_subscriptions WHERE endpoint=$1")
        .await?;
    client
        .execute(&stmt, &[&endpoint])
        .await
        .map_err(ApiError::DeletePushSubscriptionError)?;
    Ok(())
}

pub async fn list_push_subscriptions_for_account(
    client: &Client,
    account_id: i64,
) -> Result<Vec<PushSubscription>, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT subscription_id, account_id, endpoint, p256dh, auth FROM groupscape.push_subscriptions WHERE account_id=$1",
        )
        .await?;
    let rows = client
        .query(&stmt, &[&account_id])
        .await
        .map_err(ApiError::ListPushSubscriptionsError)?;
    rows.into_iter()
        .map(|row| {
            Ok(PushSubscription {
                id: row.try_get("subscription_id")?,
                account_id: row.try_get("account_id")?,
                endpoint: row.try_get("endpoint")?,
                p256dh: row.try_get("p256dh")?,
                auth: row.try_get("auth")?,
            })
        })
        .collect()
}

pub struct CharacterGroupLink {
    pub character_id: i64,
    pub group_id: i64,
    pub linked_at: DateTime<Utc>,
}

pub async fn find_character_group_link(
    client: &Client,
    character_id: i64,
) -> Result<Option<CharacterGroupLink>, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT character_id, group_id, linked_at FROM groupscape.character_group_links WHERE character_id=$1",
        )
        .await?;
    let row = client
        .query_opt(&stmt, &[&character_id])
        .await
        .map_err(ApiError::GetCharacterGroupLinkError)?;
    match row {
        Some(row) => Ok(Some(CharacterGroupLink {
            character_id: row.try_get("character_id")?,
            group_id: row.try_get("group_id")?,
            linked_at: row.try_get("linked_at")?,
        })),
        None => Ok(None),
    }
}

/// Enforces one-group-per-character: `character_id` is the table's primary key, so a
/// character can hold at most one link row. Re-linking to the same group is an idempotent
/// no-op (returns the existing row); linking to a different group is a conflict the caller
/// must resolve by leaving the current group first - ported from `groupscape-old`'s
/// `character_group_links` invariant (PK on `character_id`).
///
/// The account that owns the first character ever linked into a group becomes that group's
/// admin (`groups.admin_account_id`, set once via `WHERE admin_account_id IS NULL` and left
/// alone afterward). `groupscape-old` sets `owner_account_id` at group *creation* instead,
/// since its `create_group` route already requires a logged-in account; this codebase's
/// `unauthed::create_group` predates accounts and stays fully anonymous (ported as-is, out of
/// this ticket's scope), so linking - the only point an authenticated account ever meets a
/// group - is where "first user" is actually observable.
pub async fn link_character_to_group(
    client: &mut Client,
    character_id: i64,
    account_id: i64,
    group_id: i64,
) -> Result<CharacterGroupLink, ApiError> {
    if let Some(existing) = find_character_group_link(client, character_id).await? {
        if existing.group_id != group_id {
            return Err(ApiError::CharacterAlreadyInGroupError);
        }
        return Ok(existing);
    }

    let transaction = client.transaction().await?;

    let insert_stmt = transaction
        .prepare_cached(
            "INSERT INTO groupscape.character_group_links (character_id, group_id) VALUES ($1, $2) RETURNING character_id, group_id, linked_at",
        )
        .await?;
    let row = transaction
        .query_one(&insert_stmt, &[&character_id, &group_id])
        .await
        .map_err(ApiError::LinkCharacterToGroupError)?;

    let admin_stmt = transaction
        .prepare_cached(
            "UPDATE groupscape.groups SET admin_account_id=$1 WHERE group_id=$2 AND admin_account_id IS NULL",
        )
        .await?;
    transaction
        .execute(&admin_stmt, &[&account_id, &group_id])
        .await
        .map_err(ApiError::LinkCharacterToGroupError)?;

    // New member default: every flag off (ported from `groupscape-old`'s `createMembership`,
    // §6) - the group admin's implicit all-permissions is computed from
    // `groups.admin_account_id` at read time, not stored as a row here.
    let permissions_stmt = transaction
        .prepare_cached(
            "INSERT INTO groupscape.group_permissions (group_id, account_id) VALUES ($1, $2) ON CONFLICT (group_id, account_id) DO NOTHING",
        )
        .await?;
    transaction
        .execute(&permissions_stmt, &[&group_id, &account_id])
        .await
        .map_err(ApiError::LinkCharacterToGroupError)?;

    transaction
        .commit()
        .await
        .map_err(ApiError::LinkCharacterToGroupError)?;

    Ok(CharacterGroupLink {
        character_id: row.try_get("character_id")?,
        group_id: row.try_get("group_id")?,
        linked_at: row.try_get("linked_at")?,
    })
}

/// Removes a character's group membership without touching the character itself (unlike
/// `delete_character`, which cascades this same row as a side effect of deleting the whole
/// character). Lets the account owner re-run `link_character_to_group` against a different
/// group afterward - `character_group_links.character_id` is a primary key, so a character
/// can't hold two memberships at once and `link_character_to_group` rejects a switch outright
/// while an old link still exists.
pub async fn unlink_character_from_group(
    client: &Client,
    character_id: i64,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached("DELETE FROM groupscape.character_group_links WHERE character_id=$1")
        .await?;
    client
        .execute(&stmt, &[&character_id])
        .await
        .map_err(ApiError::LinkCharacterToGroupError)?;
    Ok(())
}

/// Resolves a group for account-session-based dashboard access (see `auth_middleware`'s
/// account-session fallback): does this account have a confirmed character already linked to
/// a group with this name? `group_name` alone isn't globally unique (see `groups`' composite
/// primary key), so on the rare case of a genuine collision this just takes the most recently
/// linked match rather than erroring - good enough since it only needs to agree with *this*
/// account's own memberships, not resolve the name globally.
pub async fn find_group_id_for_account_character(
    client: &Client,
    account_id: i64,
    group_name: &str,
) -> Result<Option<i64>, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT l.group_id FROM groupscape.character_group_links l \
             JOIN groupscape.characters c ON c.character_id = l.character_id \
             JOIN groupscape.groups g ON g.group_id = l.group_id \
             WHERE c.account_id = $1 AND c.status = 'confirmed' AND g.group_name = $2 \
             ORDER BY l.linked_at DESC LIMIT 1",
        )
        .await?;
    let row = client
        .query_opt(&stmt, &[&account_id, &group_name])
        .await
        .map_err(ApiError::GetCharacterGroupLinkError)?;
    match row {
        Some(row) => Ok(Some(row.try_get("group_id")?)),
        None => Ok(None),
    }
}

/// The account of the first character ever linked into `group_id`, or `None` for a group
/// nobody with an account has joined yet. Feeds "admin has all permissions by default"
/// (the permission model this ticket unblocks).
pub async fn get_group_admin_account_id(
    client: &Client,
    group_id: i64,
) -> Result<Option<i64>, ApiError> {
    let stmt = client
        .prepare_cached("SELECT admin_account_id FROM groupscape.groups WHERE group_id=$1")
        .await?;
    let row = client
        .query_opt(&stmt, &[&group_id])
        .await
        .map_err(ApiError::GetGroupAdminError)?;
    match row {
        Some(row) => Ok(row.try_get("admin_account_id")?),
        None => Ok(None),
    }
}

const GROUP_PERMISSION_COLUMNS: &str = "invite_members, regenerate_group_key, kick_members, manage_settings, manage_permissions, post_map_markers, post_callouts, manage_goals, manage_discord, manage_events";

fn group_permissions_from_row(row: &tokio_postgres::Row) -> Result<GroupPermissions, ApiError> {
    Ok(GroupPermissions {
        group_id: row.try_get("group_id")?,
        account_id: row.try_get("account_id")?,
        flags: PermissionFlags {
            invite_members: row.try_get("invite_members")?,
            regenerate_group_key: row.try_get("regenerate_group_key")?,
            kick_members: row.try_get("kick_members")?,
            manage_settings: row.try_get("manage_settings")?,
            manage_permissions: row.try_get("manage_permissions")?,
            post_map_markers: row.try_get("post_map_markers")?,
            post_callouts: row.try_get("post_callouts")?,
            manage_goals: row.try_get("manage_goals")?,
            manage_discord: row.try_get("manage_discord")?,
            manage_events: row.try_get("manage_events")?,
        },
    })
}

/// `None` when the account has no permissions row for this group (never joined via
/// `link_character_to_group`) - callers that need a default-false view for a group member
/// should treat that the same as [`PermissionFlags::default`].
pub async fn get_group_permissions(
    client: &Client,
    group_id: i64,
    account_id: i64,
) -> Result<Option<GroupPermissions>, ApiError> {
    let stmt = client
        .prepare_cached(&format!(
            "SELECT group_id, account_id, {GROUP_PERMISSION_COLUMNS} FROM groupscape.group_permissions WHERE group_id=$1 AND account_id=$2"
        ))
        .await?;
    let row = client
        .query_opt(&stmt, &[&group_id, &account_id])
        .await
        .map_err(ApiError::GetGroupPermissionsError)?;
    row.as_ref().map(group_permissions_from_row).transpose()
}

pub async fn list_group_permissions(
    client: &Client,
    group_id: i64,
) -> Result<Vec<GroupPermissions>, ApiError> {
    let stmt = client
        .prepare_cached(&format!(
            "SELECT group_id, account_id, {GROUP_PERMISSION_COLUMNS} FROM groupscape.group_permissions WHERE group_id=$1"
        ))
        .await?;
    let rows = client
        .query(&stmt, &[&group_id])
        .await
        .map_err(ApiError::GetGroupPermissionsError)?;
    rows.iter().map(group_permissions_from_row).collect()
}

/// One row per account currently linked into `group_id`, joined to that account's most
/// recently bound character for a display name - feeds the permission-management UI, which
/// needs to show *who* a toggle belongs to, not just an opaque `account_id`. `DISTINCT ON`
/// collapses an account with multiple characters in the group down to one row (permissions are
/// per-account, not per-character).
pub async fn list_group_member_permissions(
    client: &Client,
    group_id: i64,
) -> Result<Vec<GroupMemberPermissions>, ApiError> {
    let admin_account_id = get_group_admin_account_id(client, group_id).await?;
    let stmt = client
        .prepare_cached(&format!(
            "SELECT DISTINCT ON (c.account_id) c.account_id, c.display_rsn, m.color, {GROUP_PERMISSION_COLUMNS} \
             FROM groupscape.character_group_links cgl \
             JOIN groupscape.characters c ON c.character_id = cgl.character_id \
             JOIN groupscape.group_permissions gp ON gp.group_id = cgl.group_id AND gp.account_id = c.account_id \
             LEFT JOIN groupscape.members m ON m.group_id = cgl.group_id AND m.account_hash = c.account_hash \
             WHERE cgl.group_id = $1 \
             ORDER BY c.account_id, c.bound_at DESC"
        ))
        .await?;
    let rows = client
        .query(&stmt, &[&group_id])
        .await
        .map_err(ApiError::GetGroupPermissionsError)?;
    rows.iter()
        .map(|row| {
            let account_id: i64 = row.try_get("account_id")?;
            Ok(GroupMemberPermissions {
                account_id,
                display_rsn: row.try_get("display_rsn")?,
                is_admin: admin_account_id == Some(account_id),
                color: row.try_get("color").ok(),
                flags: PermissionFlags {
                    invite_members: row.try_get("invite_members")?,
                    regenerate_group_key: row.try_get("regenerate_group_key")?,
                    kick_members: row.try_get("kick_members")?,
                    manage_settings: row.try_get("manage_settings")?,
                    manage_permissions: row.try_get("manage_permissions")?,
                    post_map_markers: row.try_get("post_map_markers")?,
                    post_callouts: row.try_get("post_callouts")?,
                    manage_goals: row.try_get("manage_goals")?,
                    manage_discord: row.try_get("manage_discord")?,
                    manage_events: row.try_get("manage_events")?,
                },
            })
        })
        .collect()
}

/// Resolves `account_id`'s member row within `group_id` via the same account-hash join
/// `list_group_member_permissions` and `get_member_mesh` use, picking the most recently bound
/// character if the account has linked more than one. `None` means the account has no member
/// row here yet (e.g. never connected the plugin since linking).
pub async fn resolve_member_name_for_account(
    client: &Client,
    group_id: i64,
    account_id: i64,
) -> Result<Option<String>, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT m.member_name FROM groupscape.character_group_links cgl \
             JOIN groupscape.characters c ON c.character_id = cgl.character_id \
             JOIN groupscape.members m ON m.group_id = cgl.group_id AND m.account_hash = c.account_hash \
             WHERE cgl.group_id = $1 AND c.account_id = $2 \
             ORDER BY c.bound_at DESC LIMIT 1",
        )
        .await?;
    let row = client
        .query_opt(&stmt, &[&group_id, &account_id])
        .await
        .map_err(ApiError::ResolveMemberForAccountError)?;
    match row {
        Some(row) => Ok(Some(row.try_get(0)?)),
        None => Ok(None),
    }
}

/// Sets `account_id`'s member colour within `group_id`, enforcing "blocked, not swapped" on a
/// conflict (see the helmet-colour ticket): a colour already worn by a different member is
/// rejected outright rather than reassigning that member's colour out from under them.
pub async fn update_member_color(
    client: &Client,
    group_id: i64,
    account_id: i64,
    color: &str,
) -> Result<(String, GroupMemberPermissions), ApiError> {
    if !MEMBER_COLOR_PALETTE.contains(&color) {
        return Err(ApiError::InvalidMemberColorError);
    }

    let member_name = resolve_member_name_for_account(client, group_id, account_id)
        .await?
        .ok_or(ApiError::MemberColorTargetNotFoundError)?;

    let conflict_stmt = client
        .prepare_cached(
            "SELECT member_name FROM groupscape.members WHERE group_id=$1 AND color=$2 AND member_name != $3",
        )
        .await?;
    if let Some(row) = client
        .query_opt(&conflict_stmt, &[&group_id, &color, &member_name])
        .await
        .map_err(ApiError::UpdateMemberColorError)?
    {
        let taken_by: String = row.try_get(0)?;
        return Err(ApiError::MemberColorTakenError(taken_by));
    }

    let update_stmt = client
        .prepare_cached("UPDATE groupscape.members SET color=$1 WHERE group_id=$2 AND member_name=$3")
        .await?;
    client
        .execute(&update_stmt, &[&color, &group_id, &member_name])
        .await
        .map_err(ApiError::UpdateMemberColorError)?;

    let permissions = list_group_member_permissions(client, group_id)
        .await?
        .into_iter()
        .find(|permission| permission.account_id == account_id)
        .ok_or(ApiError::MemberColorTargetNotFoundError)?;
    Ok((member_name, permissions))
}

/// Partial update - each `None` field leaves its current DB value untouched (COALESCE).
/// Returns `None` if the account has no permissions row for this group (not a member).
///
/// The group admin's permission row is never writable through this path - their all-permissions
/// access is implicit (see [`has_group_permission`]) and not stored as toggles, so a patch
/// here could only ever *appear* to demote them while doing nothing (`has_group_permission`
/// would keep overriding it back to `true`). Rejecting the write up front keeps that
/// impossibility visible to callers instead of silently no-op'ing.
pub async fn update_group_permissions(
    client: &Client,
    group_id: i64,
    account_id: i64,
    patch: PermissionFlagsPatch,
) -> Result<Option<GroupPermissions>, ApiError> {
    if get_group_admin_account_id(client, group_id).await? == Some(account_id) {
        return Err(ApiError::CannotModifyGroupAdminPermissionsError);
    }
    let stmt = client
        .prepare_cached(&format!(
            r#"
UPDATE groupscape.group_permissions SET
  invite_members = COALESCE($3, invite_members),
  regenerate_group_key = COALESCE($4, regenerate_group_key),
  kick_members = COALESCE($5, kick_members),
  manage_settings = COALESCE($6, manage_settings),
  manage_permissions = COALESCE($7, manage_permissions),
  post_map_markers = COALESCE($8, post_map_markers),
  post_callouts = COALESCE($9, post_callouts),
  manage_goals = COALESCE($10, manage_goals),
  manage_discord = COALESCE($11, manage_discord),
  manage_events = COALESCE($12, manage_events)
WHERE group_id=$1 AND account_id=$2
RETURNING group_id, account_id, {GROUP_PERMISSION_COLUMNS}
"#
        ))
        .await?;
    let row = client
        .query_opt(
            &stmt,
            &[
                &group_id,
                &account_id,
                &patch.invite_members,
                &patch.regenerate_group_key,
                &patch.kick_members,
                &patch.manage_settings,
                &patch.manage_permissions,
                &patch.post_map_markers,
                &patch.post_callouts,
                &patch.manage_goals,
                &patch.manage_discord,
                &patch.manage_events,
            ],
        )
        .await
        .map_err(ApiError::UpdateGroupPermissionsError)?;
    row.as_ref().map(group_permissions_from_row).transpose()
}

/// Owner is an implicit all-permissions holder, not a toggle-holder (ported from
/// `groupscape-old`'s `hasPermission`, §6) - `account_id` matching `groups.admin_account_id`
/// short-circuits to all-true before any flag is consulted. A member with no permissions row
/// (never linked into the group) falls back to [`PermissionFlags::default`], i.e. every flag
/// off.
pub async fn get_effective_permission_flags(
    client: &Client,
    group_id: i64,
    account_id: i64,
) -> Result<PermissionFlags, ApiError> {
    if get_group_admin_account_id(client, group_id).await? == Some(account_id) {
        return Ok(PermissionFlags::all_true());
    }
    let flags = get_group_permissions(client, group_id, account_id)
        .await?
        .map(|permissions| permissions.flags)
        .unwrap_or_default();
    Ok(flags)
}

pub async fn has_group_permission(
    client: &Client,
    group_id: i64,
    account_id: i64,
    key: PermissionKey,
) -> Result<bool, ApiError> {
    let flags = get_effective_permission_flags(client, group_id, account_id).await?;
    Ok(flags.get(key))
}

/// Find-or-create-and-refresh the group's currently open session in one atomic upsert (the
/// partial unique index on `(group_id) WHERE ended_at IS NULL` is what makes this race-safe
/// under concurrent heartbeats from multiple group members) - mirrors `groupscape-old`'s
/// `ensureOpenSession`, called on every heartbeat that carries at least one event.
pub async fn ensure_open_session(client: &Client, group_id: i64) -> Result<i64, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
INSERT INTO groupscape.sessions (group_id) VALUES ($1)
ON CONFLICT (group_id) WHERE ended_at IS NULL
DO UPDATE SET last_seen_at = now()
RETURNING session_id
"#,
        )
        .await?;
    let row = client
        .query_one(&stmt, &[&group_id])
        .await
        .map_err(ApiError::EnsureOpenSessionError)?;
    Ok(row.try_get("session_id")?)
}

/// Closes every session that's gone quiet for `idle_after`, stamping `ended_at` with the
/// session's own `last_seen_at` (the time of its last heartbeat) rather than "now" - mirrors
/// `groupscape-old`'s `closeIdleSessions` recurring job. Global sweep, not scoped to one
/// group, since it's meant to run periodically across every group.
pub async fn close_idle_sessions(
    client: &Client,
    idle_after: chrono::Duration,
) -> Result<u64, ApiError> {
    let stmt = client
        .prepare_cached(
            "UPDATE groupscape.sessions SET ended_at = last_seen_at WHERE ended_at IS NULL AND last_seen_at < $1",
        )
        .await?;
    let cutoff = Utc::now() - idle_after;
    let rows_affected = client
        .execute(&stmt, &[&cutoff])
        .await
        .map_err(ApiError::CloseIdleSessionsError)?;
    Ok(rows_affected)
}

pub async fn prune_old_sessions(client: &Client) -> Result<u64, ApiError> {
    client
        .execute(
            "DELETE FROM groupscape.sessions WHERE ended_at IS NOT NULL AND ended_at < now() - interval '90 days'",
            &[],
        )
        .await
        .map_err(ApiError::PGError)
}

/// Purges loot log entries older than 28 days. Note: boss-KC and loot-value leaderboards
/// (see the "--- Leaderboards ---" comment above) read their AllTime window live from these
/// same `activity_events` rows, so once this job has run, AllTime there is effectively capped
/// at the last 28 days too.
pub async fn prune_old_loot_events(client: &Client) -> Result<u64, ApiError> {
    client
        .execute(
            "DELETE FROM groupscape.activity_events WHERE event_type = 'loot' AND occurred_at < now() - interval '28 days'",
            &[],
        )
        .await
        .map_err(ApiError::PGError)
}

/// Shared insert behind [`insert_activity_event`]/[`insert_progress_event`] -
/// `groupscape.activity_events`' `(event_type, payload)` columns are already generic enough to
/// hold any discrete event kind, so the quest/diary/combat-task/collection-log milestones reuse
/// this table and its `GET /get-activity-events` endpoint rather than getting a dedicated table.
///
/// `client_event_id`, when present, is the plugin-generated id from `GameEvent::event_id` -
/// stamped once at capture time so a client resend of an already-processed kill/death/loot event
/// (e.g. the connection dropping mid-request during a server restart, after the server already
/// committed the first insert but before its response reached the client) carries the exact same
/// id. The `ON CONFLICT DO NOTHING` against `activity_events_group_client_event_id_idx` makes
/// that resend a no-op instead of a duplicate row; the returned bool tells the caller whether a
/// row actually landed, so it can skip firing a second Discord message for a rejected duplicate.
/// `None` (progress events, older plugin builds, raid completions) always inserts, matching the
/// pre-dedup behavior.
pub async fn insert_activity_event_payload(
    client: &Client,
    group_id: i64,
    session_id: i64,
    member_name: &str,
    event_type: &str,
    payload: serde_json::Value,
    client_event_id: Option<&str>,
    occurred_at: Option<DateTime<Utc>>,
) -> Result<bool, ApiError> {
    // Precomputed so `list_activity_events`' `npc_slug = ANY(...)` notable-kill gate can run in
    // SQL instead of filtering rows after fetching them (see that function's doc comment).
    let npc_slug = (event_type == "kill")
        .then(|| {
            payload
                .get("npcName")
                .or_else(|| payload.get("npc_name"))
                .and_then(|v| v.as_str())
                .map(slugify_npc_name)
        })
        .flatten();
    // Falls back to insert time only when the plugin didn't send one (older builds, or event
    // types that don't carry it) - a genuine `occurred_at` must win so a delayed/buffered event
    // is stored under when it actually happened, not when it happened to reach the server (see
    // `GameEvent::occurred_at`'s doc comment).
    let occurred_at = occurred_at.unwrap_or_else(Utc::now);
    let stmt = client
        .prepare_cached(
            "INSERT INTO groupscape.activity_events (session_id, group_id, member_name, event_type, occurred_at, payload, npc_slug, client_event_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT (group_id, client_event_id) WHERE client_event_id IS NOT NULL DO NOTHING",
        )
        .await?;
    let inserted = client
        .execute(
            &stmt,
            &[
                &session_id,
                &group_id,
                &member_name,
                &event_type,
                &occurred_at,
                &payload,
                &npc_slug,
                &client_event_id,
            ],
        )
        .await
        .map_err(ApiError::InsertActivityEventError)?;
    Ok(inserted > 0)
}

/// Inserts the first reporting member's raid completion, returning the new row's `event_id` so
/// `raid_merge` can append later reporters to it. Concurrent callers for the same raid+difficulty
/// are already serialized by `raid_merge`'s in-process `Mutex` (see `authed.rs`), so this doesn't
/// need its own row-level locking.
pub async fn insert_raid_completion(
    client: &Client,
    group_id: i64,
    session_id: i64,
    member_name: &str,
    event: &crate::models::RaidCompletionEvent,
    ge_prices: &crate::models::GEPrices,
) -> Result<i64, ApiError> {
    let payload = crate::models::RaidCompletionPayload::first(member_name, event, ge_prices);
    let payload = serde_json::to_value(&payload).map_err(ApiError::SerdeJsonError)?;
    let stmt = client
        .prepare_cached(
            "INSERT INTO groupscape.activity_events (session_id, group_id, member_name, event_type, payload) \
             VALUES ($1, $2, $3, 'raid', $4) RETURNING event_id",
        )
        .await?;
    let row = client
        .query_one(&stmt, &[&session_id, &group_id, &member_name, &payload])
        .await
        .map_err(ApiError::InsertActivityEventError)?;
    Ok(row.try_get("event_id")?)
}

/// Folds another reporting member's completion into an already-inserted raid row (see
/// [`insert_raid_completion`]). Silently no-ops if the row is gone (finalized+swept between the
/// merge-registry lookup and this call - vanishingly rare given both happen under the same lock).
pub async fn append_raid_participant(
    client: &Client,
    event_id: i64,
    member_name: &str,
    event: &crate::models::RaidCompletionEvent,
    ge_prices: &crate::models::GEPrices,
) -> Result<(), ApiError> {
    let select_stmt = client
        .prepare_cached("SELECT payload FROM groupscape.activity_events WHERE event_id=$1")
        .await?;
    let Some(row) = client
        .query_opt(&select_stmt, &[&event_id])
        .await
        .map_err(ApiError::PGError)?
    else {
        return Ok(());
    };
    let payload_value: serde_json::Value = row.try_get("payload")?;
    let mut payload: crate::models::RaidCompletionPayload =
        serde_json::from_value(payload_value).map_err(ApiError::SerdeJsonError)?;
    payload.append(member_name, event, ge_prices);
    let payload = serde_json::to_value(&payload).map_err(ApiError::SerdeJsonError)?;
    let update_stmt = client
        .prepare_cached("UPDATE groupscape.activity_events SET payload=$2 WHERE event_id=$1")
        .await?;
    client
        .execute(&update_stmt, &[&event_id, &payload])
        .await
        .map_err(ApiError::PGError)?;
    Ok(())
}

/// Closes a raid completion's 5-minute merge window, returning the final merged payload for the
/// one-time websocket/Discord relay - `None` if the row is somehow already gone.
pub async fn finalize_raid_completion(
    client: &Client,
    event_id: i64,
) -> Result<Option<crate::models::RaidCompletionPayload>, ApiError> {
    let stmt = client
        .prepare_cached(
            "UPDATE groupscape.activity_events SET payload = jsonb_set(payload, '{mergeOpen}', 'false') \
             WHERE event_id=$1 RETURNING payload",
        )
        .await?;
    let Some(row) = client
        .query_opt(&stmt, &[&event_id])
        .await
        .map_err(ApiError::PGError)?
    else {
        return Ok(None);
    };
    let payload_value: serde_json::Value = row.try_get("payload")?;
    Ok(Some(
        serde_json::from_value(payload_value).map_err(ApiError::SerdeJsonError)?,
    ))
}

pub async fn record_location_sample(
    client: &Client,
    group_id: i64,
    member_name: &str,
    sampled_at: DateTime<Utc>,
    world: i32,
    plane: i32,
    world_x: i32,
    world_y: i32,
) -> Result<(), ApiError> {
    client
        .execute(
            "INSERT INTO groupscape.location_samples (group_id, member_name, sampled_at, world, plane, world_x, world_y) VALUES ($1, $2, $3, $4, $5, $6, $7)",
            &[&group_id, &member_name, &sampled_at, &world, &plane, &world_x, &world_y],
        )
        .await
        .map_err(ApiError::PGError)?;
    client
        .execute(
            "DELETE FROM groupscape.location_samples WHERE sampled_at < now() - interval '120 seconds'",
            &[],
        )
        .await
        .map_err(ApiError::PGError)?;
    Ok(())
}

pub async fn nearby_members_for_kill(
    client: &Client,
    group_id: i64,
    occurred_at: DateTime<Utc>,
    world: i32,
    plane: i32,
    world_x: i32,
    world_y: i32,
    reporter: &str,
) -> Result<Vec<String>, ApiError> {
    let rows = client
        .query(
            "SELECT DISTINCT ON (member_name) member_name FROM groupscape.location_samples WHERE group_id=$1 AND sampled_at BETWEEN $2::timestamptz - interval '120 seconds' AND $2::timestamptz AND world=$3 AND plane=$4 AND ((world_x-$5)*(world_x-$5) + (world_y-$6)*(world_y-$6)) <= 4096 ORDER BY member_name, sampled_at DESC",
            &[&group_id, &occurred_at, &world, &plane, &world_x, &world_y],
        )
        .await
        .map_err(ApiError::PGError)?;
    let mut participants = rows
        .iter()
        .map(|row| row.try_get::<_, String>("member_name"))
        .collect::<Result<Vec<_>, _>>()?;
    if !participants.iter().any(|name| name == reporter) {
        participants.push(reporter.to_string());
    }
    participants.sort();
    Ok(participants)
}

/// Stores one discrete kill/death event. Scoped by `member_name` (the roster identity used
/// throughout this server) rather than `groupscape-old`'s `actor_character_ids` co-attribution
/// array - this server doesn't track other members' live world/position with enough recency to
/// attribute a kill to more than the reporting member, so multi-actor credit is left for a
/// follow-up rather than modeled here.
///
/// Returns whether the event was actually inserted (`false` when `event.event_id()` matched an
/// already-stored row and was rejected as a duplicate) - see
/// [`insert_activity_event_payload`]'s doc comment.
pub async fn insert_activity_event(
    client: &Client,
    group_id: i64,
    session_id: i64,
    member_name: &str,
    event: &GameEvent,
) -> Result<bool, ApiError> {
    let payload = serde_json::to_value(event).map_err(ApiError::SerdeJsonError)?;
    insert_activity_event_payload(
        client,
        group_id,
        session_id,
        member_name,
        event.event_type(),
        payload,
        event.event_id(),
        event.occurred_at(),
    )
    .await
}

/// Stores one milestone event derived by diffing a heartbeat against the member's stored
/// progress columns (see [`crate::progress_events`]).
pub async fn insert_progress_event(
    client: &Client,
    group_id: i64,
    session_id: i64,
    member_name: &str,
    event: &crate::progress_events::ProgressEvent,
) -> Result<(), ApiError> {
    insert_activity_event_payload(
        client,
        group_id,
        session_id,
        member_name,
        event.event_type,
        event.payload.clone(),
        None,
        None,
    )
    .await?;
    Ok(())
}

/// The member's currently-stored progress columns, read before the batcher overwrites them so the
/// update handler has an "old" side to diff against.
///
/// Returns `None` when the member row doesn't exist yet.
pub async fn get_progress_snapshot(
    client: &Client,
    group_id: i64,
    member_name: &str,
) -> Result<Option<crate::progress_events::ProgressSnapshot>, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT quests, diary_vars, collection_log, combat_achievements, skills \
             FROM groupscape.members WHERE group_id=$1 AND member_name=$2",
        )
        .await?;
    let row = client
        .query_opt(&stmt, &[&group_id, &member_name])
        .await
        .map_err(ApiError::GetProgressSnapshotError)?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(crate::progress_events::ProgressSnapshot {
        quests: row.try_get("quests").ok().flatten(),
        diary_vars: row.try_get("diary_vars").ok().flatten(),
        collection_log: row.try_get("collection_log").ok().flatten(),
        combat_achievements: try_deserialize_json_column(&row, "combat_achievements")?,
        skills: row.try_get("skills").ok().flatten(),
    }))
}

/// Clears a member's stored skills snapshot so their next heartbeat starts a fresh baseline
/// instead of diffing against it - used when `progress_events::looks_like_stale_skills_baseline`
/// flags a heartbeat's level-up batch as implausibly large, which means `previous` itself is bad
/// rather than that the player genuinely crossed that many thresholds at once. This trades a
/// silent gap in that one member's skill-progress history for guaranteeing the flood can't repeat
/// on their next upload.
pub async fn reset_member_skills_snapshot(
    client: &Client,
    group_id: i64,
    member_name: &str,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            "UPDATE groupscape.members SET skills=NULL, skills_last_update=NULL \
             WHERE group_id=$1 AND member_name=$2",
        )
        .await?;
    client
        .execute(&stmt, &[&group_id, &member_name])
        .await
        .map_err(|e| ApiError::AdminDbError("ResetMemberSkillsSnapshotError".to_string(), e))?;
    Ok(())
}

fn activity_event_from_row(row: &Row) -> Result<ActivityEvent, ApiError> {
    Ok(ActivityEvent {
        id: row.try_get("event_id")?,
        session_id: row.try_get("session_id")?,
        member_name: row.try_get("member_name")?,
        event_type: row.try_get("event_type")?,
        occurred_at: row.try_get("occurred_at")?,
        payload: row.try_get("payload")?,
        reactions: None,
        my_reaction: None,
        comment_count: None,
    })
}

/// Paginated, newest-first activity feed for a group, optionally filtered by member and/or
/// event type, with a `before` cursor (mirrors `groupscape-old`'s `GET /groups/:id/activity`).
///
/// The notable-kill gate (bosses/major quest bosses, see [`crate::notable_npcs`]) is applied in
/// SQL via `npc_slug = ANY(...)` rather than as a post-fetch filter, so `LIMIT` operates on the
/// already-filtered set - a post-fetch filter would let a page come back short of `limit` even
/// though more matching rows exist further back, which breaks a cursor-paginated caller's
/// "was that the last page?" check. The other allowlisted types (death, quest, diary, combat_task,
/// collection_log) are already milestone-scoped at insert time and need no extra gate.
#[allow(clippy::too_many_arguments)]
pub async fn list_activity_events(
    client: &Client,
    group_id: i64,
    member_name: Option<&str>,
    event_type: Option<&str>,
    before: Option<DateTime<Utc>>,
    limit: i64,
) -> Result<Vec<ActivityEvent>, ApiError> {
    let notable_slugs: Vec<String> = crate::notable_npcs::names()
        .into_iter()
        .map(|(slug, _)| slug)
        .collect();
    let stmt = client
        .prepare_cached(
            r#"
SELECT event_id, session_id, member_name, event_type, occurred_at, payload
FROM groupscape.activity_events
WHERE group_id=$1
  AND (
    event_type IN ('kill', 'death', 'quest', 'diary', 'combat_task', 'collection_log', 'raid', 'level_up')
    OR (event_type = 'loot' AND payload->>'clueTier' IS NOT NULL)
  )
  AND (event_type != 'kill' OR npc_slug = ANY($6))
  AND ($2::text IS NULL OR member_name = $2)
  AND (
    $3::text IS NULL
    OR ($3 = 'clue' AND event_type = 'loot' AND payload->>'clueTier' IS NOT NULL)
    OR ($3 != 'clue' AND event_type = $3)
  )
  AND ($4::timestamptz IS NULL OR occurred_at < $4)
ORDER BY occurred_at DESC
LIMIT $5
"#,
        )
        .await?;
    let rows = client
        .query(
            &stmt,
            &[
                &group_id,
                &member_name,
                &event_type,
                &before,
                &limit.clamp(1, 200),
                &notable_slugs,
            ],
        )
        .await
        .map_err(ApiError::ListActivityEventsError)?;
    rows.iter().map(activity_event_from_row).collect()
}

/// `groups.activity_reactions_enabled` - defaults to `true` for any group predating the column
/// (`ALTER ... DEFAULT true` backfills existing rows at migration time, so this only matters for
/// the theoretical case of the column being missing entirely).
pub async fn get_activity_settings(
    client: &Client,
    group_id: i64,
) -> Result<crate::models::ActivitySettings, ApiError> {
    let stmt = client
        .prepare_cached("SELECT activity_reactions_enabled FROM groupscape.groups WHERE group_id=$1")
        .await?;
    let row = client
        .query_opt(&stmt, &[&group_id])
        .await
        .map_err(ApiError::GetActivitySettingsError)?;
    Ok(crate::models::ActivitySettings {
        reactions_enabled: row.and_then(|r| r.try_get(0).ok()).unwrap_or(true),
    })
}

pub async fn update_activity_settings(
    client: &Client,
    group_id: i64,
    settings: crate::models::ActivitySettings,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            "UPDATE groupscape.groups SET activity_reactions_enabled=$1 WHERE group_id=$2",
        )
        .await?;
    client
        .execute(&stmt, &[&settings.reactions_enabled, &group_id])
        .await
        .map_err(ApiError::UpdateActivitySettingsError)?;
    Ok(())
}

/// `(group_id, member_name)` for `event_id`, used by the react/comment handlers to (a) confirm
/// the event actually belongs to the caller's group before touching it and (b) know whose item
/// this is for the "can't react to your own activity" check.
pub async fn get_activity_event_group_and_member(
    client: &Client,
    event_id: i64,
) -> Result<Option<(i64, String)>, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT group_id, member_name FROM groupscape.activity_events WHERE event_id=$1",
        )
        .await?;
    let row = client
        .query_opt(&stmt, &[&event_id])
        .await
        .map_err(ApiError::GetActivityReactionsError)?;
    match row {
        Some(row) => Ok(Some((row.try_get(0)?, row.try_get(1)?))),
        None => Ok(None),
    }
}

fn activity_reaction_counts_from_rows(
    rows: &[Row],
) -> Result<Vec<crate::models::ActivityReactionCount>, ApiError> {
    rows.iter()
        .map(|row| {
            Ok(crate::models::ActivityReactionCount {
                reaction: row.try_get("reaction")?,
                count: row.try_get("count")?,
            })
        })
        .collect()
}

/// Toggles `account_id`'s reaction on `event_id`: reacting with the same type it already has
/// removes it, any other type (including having none yet) replaces it. Runs inside its own
/// transaction so the read-then-write can't race itself, then returns the fresh summary for the
/// caller to hand straight back to the client without a second round trip.
pub async fn toggle_activity_reaction(
    client: &mut Client,
    event_id: i64,
    account_id: i64,
    reaction: &str,
) -> Result<crate::models::ActivityReactionsSummary, ApiError> {
    let transaction = client.transaction().await?;

    let existing: Option<String> = {
        let stmt = transaction
            .prepare_cached(
                "SELECT reaction FROM groupscape.activity_event_reactions \
                 WHERE event_id=$1 AND account_id=$2 FOR UPDATE",
            )
            .await?;
        transaction
            .query_opt(&stmt, &[&event_id, &account_id])
            .await
            .map_err(ApiError::ToggleActivityReactionError)?
            .map(|row| row.try_get(0))
            .transpose()?
    };

    if existing.as_deref() == Some(reaction) {
        let stmt = transaction
            .prepare_cached(
                "DELETE FROM groupscape.activity_event_reactions WHERE event_id=$1 AND account_id=$2",
            )
            .await?;
        transaction
            .execute(&stmt, &[&event_id, &account_id])
            .await
            .map_err(ApiError::ToggleActivityReactionError)?;
    } else {
        let stmt = transaction
            .prepare_cached(
                "INSERT INTO groupscape.activity_event_reactions (event_id, account_id, reaction) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (event_id, account_id) DO UPDATE SET reaction=$3, created_at=now()",
            )
            .await?;
        transaction
            .execute(&stmt, &[&event_id, &account_id, &reaction])
            .await
            .map_err(ApiError::ToggleActivityReactionError)?;
    }

    let counts_stmt = transaction
        .prepare_cached(
            "SELECT reaction, COUNT(*) AS count FROM groupscape.activity_event_reactions \
             WHERE event_id=$1 GROUP BY reaction",
        )
        .await?;
    let count_rows = transaction
        .query(&counts_stmt, &[&event_id])
        .await
        .map_err(ApiError::ToggleActivityReactionError)?;
    let reactions = activity_reaction_counts_from_rows(&count_rows)?;
    let my_reaction = if existing.as_deref() == Some(reaction) {
        None
    } else {
        Some(reaction.to_string())
    };

    transaction.commit().await?;
    Ok(crate::models::ActivityReactionsSummary {
        reactions,
        my_reaction,
        comment_count: 0,
    })
}

/// Batch reaction summary + comment count for a page of activity events (`get-activity-reactions`
/// and, indirectly, whatever embeds these into `get_activity_events`). `account_id` is `None` for
/// a request with no logged-in account behind it - `my_reaction` is then always `None` for every
/// event rather than erroring, since reading the feed never requires being logged in.
pub async fn get_reactions_summary_for_events(
    client: &Client,
    event_ids: &[i64],
    account_id: Option<i64>,
) -> Result<
    std::collections::HashMap<i64, crate::models::ActivityReactionsSummary>,
    ApiError,
> {
    let mut result: std::collections::HashMap<i64, crate::models::ActivityReactionsSummary> =
        event_ids
            .iter()
            .map(|id| {
                (
                    *id,
                    crate::models::ActivityReactionsSummary {
                        reactions: Vec::new(),
                        my_reaction: None,
                        comment_count: 0,
                    },
                )
            })
            .collect();
    if event_ids.is_empty() {
        return Ok(result);
    }

    let stmt = client
        .prepare_cached(
            "SELECT event_id, reaction, COUNT(*) AS count FROM groupscape.activity_event_reactions \
             WHERE event_id = ANY($1) GROUP BY event_id, reaction",
        )
        .await?;
    let rows = client
        .query(&stmt, &[&event_ids])
        .await
        .map_err(ApiError::GetActivityReactionsError)?;
    for row in &rows {
        let event_id: i64 = row.try_get("event_id")?;
        let reaction: String = row.try_get("reaction")?;
        let count: i64 = row.try_get("count")?;
        if let Some(summary) = result.get_mut(&event_id) {
            summary.reactions.push(crate::models::ActivityReactionCount { reaction, count });
        }
    }

    if let Some(account_id) = account_id {
        let mine_stmt = client
            .prepare_cached(
                "SELECT event_id, reaction FROM groupscape.activity_event_reactions \
                 WHERE event_id = ANY($1) AND account_id=$2",
            )
            .await?;
        let mine_rows = client
            .query(&mine_stmt, &[&event_ids, &account_id])
            .await
            .map_err(ApiError::GetActivityReactionsError)?;
        for row in &mine_rows {
            let event_id: i64 = row.try_get("event_id")?;
            let reaction: String = row.try_get(1)?;
            if let Some(summary) = result.get_mut(&event_id) {
                summary.my_reaction = Some(reaction);
            }
        }
    }

    let comment_counts = get_comment_counts_for_events(client, event_ids).await?;
    for (event_id, count) in comment_counts {
        if let Some(summary) = result.get_mut(&event_id) {
            summary.comment_count = count;
        }
    }

    Ok(result)
}

pub async fn get_comment_counts_for_events(
    client: &Client,
    event_ids: &[i64],
) -> Result<std::collections::HashMap<i64, i64>, ApiError> {
    let mut result: std::collections::HashMap<i64, i64> =
        event_ids.iter().map(|id| (*id, 0)).collect();
    if event_ids.is_empty() {
        return Ok(result);
    }
    let stmt = client
        .prepare_cached(
            "SELECT event_id, COUNT(*) AS count FROM groupscape.activity_event_comments \
             WHERE event_id = ANY($1) GROUP BY event_id",
        )
        .await?;
    let rows = client
        .query(&stmt, &[&event_ids])
        .await
        .map_err(ApiError::ListActivityCommentsError)?;
    for row in &rows {
        let event_id: i64 = row.try_get(0)?;
        let count: i64 = row.try_get(1)?;
        result.insert(event_id, count);
    }
    Ok(result)
}

pub async fn list_activity_comments(
    client: &Client,
    event_id: i64,
) -> Result<Vec<crate::models::ActivityComment>, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT comment_id, member_name, comment_text, created_at \
             FROM groupscape.activity_event_comments WHERE event_id=$1 ORDER BY created_at ASC",
        )
        .await?;
    let rows = client
        .query(&stmt, &[&event_id])
        .await
        .map_err(ApiError::ListActivityCommentsError)?;
    rows.iter()
        .map(|row| {
            Ok(crate::models::ActivityComment {
                comment_id: row.try_get("comment_id")?,
                member_name: row.try_get("member_name")?,
                comment_text: row.try_get("comment_text")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect()
}

/// Reads the caller's server-side delivery cursor (§6) - `0` for an account that's never
/// backfilled this group before, same "brand-new" semantics `list_chat_messages_since` already
/// gives `since_message_id = 0`.
pub async fn get_chat_delivery_cursor(
    client: &Client,
    group_id: i64,
    account_id: i64,
) -> Result<i64, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
SELECT last_delivered_message_id FROM groupscape.chat_delivery_cursors
WHERE group_id = $1 AND account_id = $2
"#,
        )
        .await?;
    let row = client
        .query_opt(&stmt, &[&group_id, &account_id])
        .await
        .map_err(ApiError::AdvanceChatDeliveryCursorError)?;
    Ok(match row {
        Some(row) => row.try_get("last_delivered_message_id")?,
        None => 0,
    })
}

/// Advances the caller's delivery cursor to `message_id`, never backwards - same `GREATEST`
/// upsert pattern as `advance_chat_read_cursor`, guarding against a stale/out-of-order call (e.g.
/// two of the account's sessions backfilling at once) rewinding it. See spec §6.
pub async fn advance_chat_delivery_cursor(
    client: &Client,
    group_id: i64,
    account_id: i64,
    message_id: i64,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
INSERT INTO groupscape.chat_delivery_cursors (group_id, account_id, last_delivered_message_id)
VALUES ($1, $2, $3)
ON CONFLICT (group_id, account_id) DO UPDATE
SET last_delivered_message_id = GREATEST(chat_delivery_cursors.last_delivered_message_id, EXCLUDED.last_delivered_message_id),
    updated_at = now()
"#,
        )
        .await?;
    client
        .execute(&stmt, &[&group_id, &account_id, &message_id])
        .await
        .map_err(ApiError::AdvanceChatDeliveryCursorError)?;
    Ok(())
}

/// Backfills messages strictly newer than `since_message_id`, oldest-first, capped at
/// `CHAT_BACKFILL_CAP` - see the "Chat history and backfill behavior" spec ticket. `since_message_id
/// = 0` (a brand-new account's cursor) returns the most recent `CHAT_BACKFILL_CAP` messages rather
/// than the group's entire history.
pub async fn list_chat_messages_since(
    client: &Client,
    group_id: i64,
    since_message_id: i64,
) -> Result<Vec<crate::models::ChatMessage>, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
SELECT message_id, member_name, message_text, created_at FROM (
  SELECT message_id, member_name, message_text, created_at
  FROM groupscape.chat_messages
  WHERE group_id = $1 AND message_id > $2
  ORDER BY message_id DESC
  LIMIT $3
) capped
ORDER BY message_id ASC
"#,
        )
        .await?;
    let rows = client
        .query(&stmt, &[&group_id, &since_message_id, &crate::models::CHAT_BACKFILL_CAP])
        .await
        .map_err(ApiError::ListChatMessagesError)?;
    rows.iter()
        .map(|row| {
            Ok(crate::models::ChatMessage {
                message_id: row.try_get("message_id")?,
                member_name: row.try_get("member_name")?,
                message_text: row.try_get("message_text")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect()
}

/// Inserts a chat message. `member_name` is a point-in-time display snapshot (nullable, matching
/// the column) - the caller resolves it once via `resolve_member_name_for_account` before calling
/// this, same division of responsibility as `add_activity_comment`.
pub async fn add_chat_message(
    client: &Client,
    group_id: i64,
    account_id: i64,
    member_name: Option<&str>,
    message_text: &str,
) -> Result<crate::models::ChatMessage, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
INSERT INTO groupscape.chat_messages (group_id, account_id, member_name, message_text)
VALUES ($1, $2, $3, $4)
RETURNING message_id, member_name, message_text, created_at
"#,
        )
        .await?;
    let row = client
        .query_one(&stmt, &[&group_id, &account_id, &member_name, &message_text])
        .await
        .map_err(ApiError::AddChatMessageError)?;
    Ok(crate::models::ChatMessage {
        message_id: row.try_get("message_id")?,
        member_name: row.try_get("member_name")?,
        message_text: row.try_get("message_text")?,
        created_at: row.try_get("created_at")?,
    })
}

/// Deletes a chat message. Scoped by `group_id` as well as `message_id` so a caller can never
/// delete another group's message even if it somehow guessed the id. Returns whether a row was
/// actually removed, so the handler can 404 rather than broadcasting a delete for a message that
/// never existed (or was already deleted by a racing admin session).
pub async fn delete_chat_message(
    client: &Client,
    group_id: i64,
    message_id: i64,
) -> Result<bool, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
DELETE FROM groupscape.chat_messages
WHERE group_id = $1 AND message_id = $2
"#,
        )
        .await?;
    let deleted = client
        .execute(&stmt, &[&group_id, &message_id])
        .await
        .map_err(ApiError::DeleteChatMessageError)?;
    Ok(deleted > 0)
}

/// Advances the caller's read cursor to `message_id`, never backwards - `GREATEST` guards against
/// a stale/out-of-order request (e.g. two of the account's sessions racing) rewinding a cursor
/// another session already pushed further. Returns the resulting cursor value (which may be higher
/// than `message_id` if another session got there first), so the caller broadcasts and echoes back
/// what's actually true rather than what was merely requested. See the "Chat history and backfill
/// behavior" spec ticket's read-cursor section (§6).
pub async fn advance_chat_read_cursor(
    client: &Client,
    group_id: i64,
    account_id: i64,
    message_id: i64,
) -> Result<i64, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
INSERT INTO groupscape.chat_read_cursors (group_id, account_id, last_read_message_id)
VALUES ($1, $2, $3)
ON CONFLICT (group_id, account_id) DO UPDATE
SET last_read_message_id = GREATEST(chat_read_cursors.last_read_message_id, EXCLUDED.last_read_message_id),
    updated_at = now()
RETURNING last_read_message_id
"#,
        )
        .await?;
    let row = client
        .query_one(&stmt, &[&group_id, &account_id, &message_id])
        .await
        .map_err(ApiError::AdvanceChatReadCursorError)?;
    Ok(row.try_get("last_read_message_id")?)
}

/// Inserts a comment, enforcing the 10-per-event cap atomically via a `WHERE COUNT(*) < 10`
/// sub-select rather than a separate check-then-insert - closes the race where two comments
/// submitted at once could both pass a prior "count < 10" check and land as the 11th/12th.
/// Returns `None` when the cap was already reached (no row inserted).
pub async fn add_activity_comment(
    client: &Client,
    event_id: i64,
    account_id: i64,
    member_name: &str,
    comment_text: &str,
) -> Result<Option<crate::models::ActivityComment>, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
INSERT INTO groupscape.activity_event_comments (event_id, account_id, member_name, comment_text)
SELECT $1, $2, $3, $4
WHERE (SELECT COUNT(*) FROM groupscape.activity_event_comments WHERE event_id=$1) < $5
RETURNING comment_id, member_name, comment_text, created_at
"#,
        )
        .await?;
    let row = client
        .query_opt(
            &stmt,
            &[
                &event_id,
                &account_id,
                &member_name,
                &comment_text,
                &crate::models::ACTIVITY_COMMENT_CAP,
            ],
        )
        .await
        .map_err(ApiError::AddActivityCommentError)?;
    match row {
        Some(row) => Ok(Some(crate::models::ActivityComment {
            comment_id: row.try_get("comment_id")?,
            member_name: row.try_get("member_name")?,
            comment_text: row.try_get("comment_text")?,
            created_at: row.try_get("created_at")?,
        })),
        None => Ok(None),
    }
}

/// All `kill` events for a group in an optional `[since, until]` range, uncapped by cursor
/// pagination (bounded by a hard safety limit) since callers aggregate the full result rather
/// than paging it - mirrors `groupscape-old`'s `listBossKillEventsForGroup` query-time pivot
/// that the loot summary/split endpoints are built on.
pub async fn list_kill_events(
    client: &Client,
    group_id: i64,
    member_name: Option<&str>,
    session_id: Option<i64>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Result<Vec<ActivityEvent>, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
SELECT event_id, session_id, member_name, event_type, occurred_at, payload
FROM groupscape.activity_events
WHERE group_id=$1
  AND event_type='kill'
  AND ($2::text IS NULL OR member_name = $2)
    AND ($3::bigint IS NULL OR session_id = $3)
    AND ($4::timestamptz IS NULL OR occurred_at >= $4)
    AND ($5::timestamptz IS NULL OR occurred_at <= $5)
ORDER BY occurred_at DESC
LIMIT 5000
"#,
        )
        .await?;
    let rows = client
        .query(&stmt, &[&group_id, &member_name, &session_id, &since, &until])
        .await
        .map_err(ApiError::ListKillEventsError)?;
    rows.iter().map(activity_event_from_row).collect()
}

/// One page of raw `kill`/`loot` activity events, newest-first, cursor-paginated on `before` -
/// the Loot Log's source. Unlike the deleted `list_loot_and_kill_events` (which returned an
/// unbounded batch for query-time pre-aggregation), this returns one row per event so the caller
/// can derive 45-minute farming-session entries from real per-event timestamps and page through
/// arbitrarily long history instead of hitting a hard 5000-row ceiling.
pub async fn list_loot_and_kill_events_page(
    client: &Client,
    group_id: i64,
    before: Option<DateTime<Utc>>,
    limit: i64,
) -> Result<Vec<ActivityEvent>, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
SELECT event_id, session_id, member_name, event_type, occurred_at, payload
FROM groupscape.activity_events
WHERE group_id=$1
  AND event_type IN ('kill', 'loot')
  AND ($2::timestamptz IS NULL OR occurred_at < $2)
ORDER BY occurred_at DESC
LIMIT $3
"#,
        )
        .await?;
    let rows = client
        .query(&stmt, &[&group_id, &before, &limit.clamp(1, 1000)])
        .await
        .map_err(ApiError::ListKillEventsError)?;
    rows.iter().map(activity_event_from_row).collect()
}

/// Writes one slayer-task assignment/close event, upserting onto the same row when a later
/// close event for the same task (`client_event_id`) arrives - see the unique index in
/// `create_slayer_task_history_table`. `status`/`amount_done`/`amount_total`/`points`/
/// `closed_at` all move to whatever the incoming event carries (a close event's values replace
/// the assignment event's), `task_name`/`master_name`/`assigned_at` stay fixed at whatever the
/// first (assignment) event wrote - a close event always resends the same values for these, so
/// `EXCLUDED` would be equivalent, but pinning them to the original row is the more honest
/// intent (this row's identity is "the task assigned at `assigned_at`", not "whatever the most
/// recent event happened to say").
///
/// An assignment event (`status` "not_started"/"in_progress") first force-closes any *other* row
/// for this member still sitting open, as "superseded" - the plugin's own transition detection
/// (see `GroupScapeTrackerPlugin#handleSlayerTaskTransition`) already guarantees a close event
/// precedes the next assignment, but `list_slayer_task_history_page`'s live-progress overlay and
/// `get_slayer_task_stats`' completion rate both lean on "at most one open row per member" - this
/// makes that a real server-side invariant instead of one only the client is trusted to uphold
/// (a lost close event, an out-of-order upload, or a future plugin bug would otherwise leave a
/// stale open row parked in the table forever).
/// Masters who'll swap the current task for a new one at no points cost (the "Turael skip") -
/// mirrors `GroupScapeTrackerPlugin#SLAYER_RESET_MASTERS` (Aya/Spria are Turael's While Guthix
/// Sleeps/pre-quest reskins). Used here to recognize a dangling-row cleanup as a reset rather
/// than guessing from live state - see this fn's doc comment.
const SLAYER_RESET_MASTERS: [&str; 3] = ["turael", "aya", "spria"];

pub async fn upsert_slayer_task_history_event(
    client: &Client,
    group_id: i64,
    member_name: &str,
    event: &SlayerTaskHistoryEvent,
) -> Result<(), ApiError> {
    if event.status == "in_progress" || event.status == "not_started" {
        let dangling_stmt = client
            .prepare_cached(
                r#"
SELECT client_event_id, task_name, master_name, amount_total
FROM groupscape.slayer_task_history
WHERE group_id=$1 AND member_name=$2 AND client_event_id <> $3
  AND status IN ('in_progress', 'not_started')
"#,
            )
            .await?;
        let dangling_rows = client
            .query(
                &dangling_stmt,
                &[&group_id, &member_name, &event.client_event_id],
            )
            .await?;

        if !dangling_rows.is_empty() {
            // The plugin's transition detection is supposed to guarantee a close event always
            // precedes the next assignment (see this fn's doc comment), but a lost close event
            // does happen in practice - most often a Turael/Aya/Spria skip of a task assigned by
            // some other master, where the plugin-side close raced the reassignment. Zero'th
            // signal, checked before either of the below: if the dangling row already matches
            // this assignment's task/master/amount_total, it isn't a lost close at all - it's a
            // resend of the same still-open task (e.g. the member's client switched PC/mobile
            // mid-task), so the row is left open and the incoming event is dropped rather than
            // closed into a duplicate row. Otherwise, two signals recover the real outcome
            // instead of a blind "superseded" guess:
            //   1. If *this* assignment's master is one of the free-skip-granting three, the only
            //      way OSRS lets that happen while another task is still open is that skip - so
            //      every dangling row for this member is a "reset", full stop (mirrors
            //      GroupScapeTrackerPlugin#closeSlayerTask's own reset check, which looks at
            //      whichever master is granting the *new* task, not the old one).
            //   2. Otherwise, if the dangling row's own task/master still matches the live
            //      snapshot this member last reported and that snapshot shows the task fully
            //      killed, treat it as the completion it actually was (mirroring
            //      `list_slayer_task_history_page`'s read-time overlay).
            // A row that matches neither is genuinely ambiguous (most likely a plugin/client
            // restart mid-task with no surviving snapshot - e.g. the player finished the task on
            // a different client than the one that reported the assignment) - default that to
            // "completed" with a full kill count rather than "superseded" or a stale/zero
            // amount_done, since finishing normally is by far the more common way to lose a close
            // event than a paid cancel/block, which the plugin closes synchronously anyway.
            //
            // Signal 1 is checked *after* signal 2, not before: Turael/Aya/Spria are also normal
            // low-level Slayer masters (not just skip-granters), so a member can legitimately
            // finish a Turael task and get handed another Turael task next with no skip involved.
            // Trusting a live snapshot that shows the dangling task fully killed always wins over
            // the master-based guess; only fall back to "reset" when that snapshot doesn't show
            // completion.
            let incoming_master_is_reset_grantor = SLAYER_RESET_MASTERS
                .contains(&event.master_name.trim().to_lowercase().as_str());

            let live_task = get_live_slayer_task(client, group_id, member_name).await?;

            let reset_stmt = client
                .prepare_cached(
                    r#"
UPDATE groupscape.slayer_task_history
SET status = 'reset', closed_at = COALESCE(closed_at, now())
WHERE group_id=$1 AND member_name=$2 AND client_event_id = $3
"#,
                )
                .await?;
            let completed_stmt = client
                .prepare_cached(
                    r#"
UPDATE groupscape.slayer_task_history
SET status = 'completed', amount_done = $4, closed_at = COALESCE(closed_at, now())
WHERE group_id=$1 AND member_name=$2 AND client_event_id = $3
"#,
                )
                .await?;
            let mut resumed_existing_task = false;

            for row in &dangling_rows {
                let dangling_event_id: String = row.try_get("client_event_id")?;
                let dangling_task_name: String = row.try_get("task_name")?;
                let dangling_master_name: String = row.try_get("master_name")?;
                let dangling_amount_total: i32 = row.try_get("amount_total")?;

                // Same task/master/kill-count as this still-open row: the incoming assignment
                // is a resend of the task already in progress (e.g. the member's client
                // switched PC/mobile mid-task and re-reported the same assignment), not a new
                // task starting. Leave the row open and skip inserting a duplicate below,
                // rather than force-closing it into a bogus "completed" row.
                if event.task_name == dangling_task_name
                    && event.master_name.eq_ignore_ascii_case(&dangling_master_name)
                    && event.amount_total == dangling_amount_total
                {
                    resumed_existing_task = true;
                    continue;
                }

                // master_name intentionally not checked here either - same restart gap as
                // `list_slayer_task_history_page`'s overlay above.
                let recovered_amount_done = live_task.as_ref().filter(|live| {
                    live.task_name.as_deref() == Some(dangling_task_name.as_str())
                        && live.amount_remaining.is_some_and(|remaining| remaining <= 0)
                }).and_then(live_slayer_task_amount_done);

                if recovered_amount_done.is_none() && incoming_master_is_reset_grantor {
                    client
                        .execute(&reset_stmt, &[&group_id, &member_name, &dangling_event_id])
                        .await?;
                    continue;
                }

                let amount_done = recovered_amount_done.unwrap_or(dangling_amount_total);

                client
                    .execute(
                        &completed_stmt,
                        &[&group_id, &member_name, &dangling_event_id, &amount_done],
                    )
                    .await?;
            }

            if resumed_existing_task {
                return Ok(());
            }
        }
    }

    let stmt = client
        .prepare_cached(
            r#"
INSERT INTO groupscape.slayer_task_history
  (group_id, member_name, client_event_id, task_name, master_name, status, amount_done, amount_total, points, assigned_at, closed_at, modifier_type, modifier_value, modifier_negative)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
ON CONFLICT (group_id, member_name, client_event_id) DO UPDATE SET
  status = EXCLUDED.status,
  amount_done = EXCLUDED.amount_done,
  amount_total = EXCLUDED.amount_total,
  points = EXCLUDED.points,
  closed_at = EXCLUDED.closed_at
"#,
        )
        .await?;
    client
        .execute(
            &stmt,
            &[
                &group_id,
                &member_name,
                &event.client_event_id,
                &event.task_name,
                &event.master_name,
                &event.status,
                &event.amount_done,
                &event.amount_total,
                &event.points,
                &event.assigned_at,
                &event.closed_at,
                &event.modifier_type,
                &event.modifier_value,
                &event.modifier_negative,
            ],
        )
        .await?;
    Ok(())
}

/// The member's live in-progress task, straight off `groupscape.members.slayer_task` (the same
/// source the Current tab reads) - `None` if the member has no task assigned or the plugin
/// hasn't sent amount data yet. Used to overlay a fresh kill count onto the one
/// `slayer_task_history` row that's still open, since that table only gets written at task
/// assignment (always 0 done) and at close - see `upsert_slayer_task_history_event`'s doc
/// comment. Never persisted back to `slayer_task_history`; this stays a read-time overlay so no
/// extra writes are needed per kill.
async fn get_live_slayer_task(
    client: &Client,
    group_id: i64,
    member_name: &str,
) -> Result<Option<SlayerTask>, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT slayer_task FROM groupscape.members WHERE group_id=$1 AND member_name=$2",
        )
        .await?;
    let row = client.query_opt(&stmt, &[&group_id, &member_name]).await?;
    let Some(row) = row else { return Ok(None) };
    Ok(try_deserialize_json_column(&row, "slayer_task")?)
}

/// `initial_amount - amount_remaining` off a live [`SlayerTask`], if the plugin has reported
/// both - `None` while a task is assigned but neither has been observed yet.
fn live_slayer_task_amount_done(task: &SlayerTask) -> Option<i32> {
    if !task.has_task {
        return None;
    }
    let initial = task.initial_amount?;
    let remaining = task.amount_remaining?;
    Some((initial - remaining).max(0))
}

fn slayer_task_history_entry_from_row(row: &Row) -> Result<SlayerTaskHistoryEntry, ApiError> {
    Ok(SlayerTaskHistoryEntry {
        task_name: row.try_get("task_name")?,
        master_name: row.try_get("master_name")?,
        status: row.try_get("status")?,
        amount_done: row.try_get("amount_done")?,
        amount_total: row.try_get("amount_total")?,
        points: row.try_get("points")?,
        assigned_at: row.try_get("assigned_at")?,
        closed_at: row.try_get("closed_at")?,
        modifier_type: row.try_get("modifier_type")?,
        modifier_value: row.try_get("modifier_value")?,
        modifier_negative: row.try_get("modifier_negative")?,
    })
}

/// One page of a member's slayer task history, newest-first, offset-paginated (page/page_size)
/// so the History tab can offer first/prev/next/last controls over a known total - same
/// list-then-count shape as [`admin_list_groups`]. `status`/`master_name` are optional
/// exact-match filters for the History tab's dropdowns and apply to both queries so the total
/// reflects the filtered set, not the member's whole history.
pub async fn list_slayer_task_history_page(
    client: &Client,
    group_id: i64,
    member_name: &str,
    page: i64,
    page_size: i64,
    status: Option<&str>,
    // A slice of exact `master_name` values rather than one, since the site's master filter
    // groups NPCs that are really the same slayer master role (Nieve/Steve, Duradel/Kuradal,
    // ...) into one dropdown option - see slayer-history-tab.js's MASTER_GROUPS. `None`/empty
    // means no filter, matching `status` above.
    master_names: Option<&[String]>,
) -> Result<SlayerTaskHistoryPage, ApiError> {
    let page_size = page_size.clamp(1, 100);
    let master_names = master_names.filter(|names| !names.is_empty());

    let count_stmt = client
        .prepare_cached(
            r#"
SELECT COUNT(*) FROM groupscape.slayer_task_history
WHERE group_id=$1
  AND member_name=$2
  AND ($3::text IS NULL OR status = $3)
  AND ($4::text[] IS NULL OR master_name = ANY($4))
"#,
        )
        .await?;
    let total_entries: i64 = client
        .query_one(&count_stmt, &[&group_id, &member_name, &status, &master_names])
        .await?
        .try_get(0)?;

    let total_pages = ((total_entries - 1) / page_size + 1).max(1);
    let page = page.clamp(1, total_pages);
    let offset = (page - 1) * page_size;

    let list_stmt = client
        .prepare_cached(
            r#"
SELECT task_name, master_name, status, amount_done, amount_total, points, assigned_at, closed_at, modifier_type, modifier_value, modifier_negative
FROM groupscape.slayer_task_history
WHERE group_id=$1
  AND member_name=$2
  AND ($3::text IS NULL OR status = $3)
  AND ($4::text[] IS NULL OR master_name = ANY($4))
ORDER BY assigned_at DESC
LIMIT $5 OFFSET $6
"#,
        )
        .await?;
    let rows = client
        .query(
            &list_stmt,
            &[&group_id, &member_name, &status, &master_names, &page_size, &offset],
        )
        .await?;

    let mut entries = rows
        .iter()
        .map(slayer_task_history_entry_from_row)
        .collect::<Result<Vec<_>, _>>()?;

    // Overlay live progress onto whichever row is still in_progress (at most one, and always
    // the newest since a new assignment can't happen until the last task closes) - see
    // `get_live_slayer_task`.
    if let Some(entry) = entries.iter_mut().find(|entry| entry.status == "in_progress") {
        if let Some(live_task) = get_live_slayer_task(client, group_id, member_name).await? {
            // master_name is deliberately not part of this match: after a plugin/client restart
            // mid-task, SlayerTaskState's masterName stays null until the player next talks to a
            // recognized slayer master (see SlayerTaskState.java), so requiring it to match would
            // skip the overlay and leave the row stuck on its stale assignment-time amount_done.
            // task_name plus "the one in_progress row" (at most one, per this fn's doc comment)
            // is already enough to identify it.
            if live_task.task_name.as_deref() == Some(entry.task_name.as_str()) {
                if let Some(live_amount_done) = live_slayer_task_amount_done(&live_task) {
                    entry.amount_done = live_amount_done;
                }
                // Fully killed but not yet turned in to the master - the Current tab already
                // shows this as "Task complete" (see slayer-panel.js's isTaskComplete), so
                // reflect that here too rather than leaving the row stuck on "In progress"
                // until the close event lands. `points` stays unset since the real completion
                // reward isn't known until that close event actually arrives.
                if live_task.amount_remaining.is_some_and(|remaining| remaining <= 0) {
                    entry.status = "completed".to_string();
                }
            }
        }
    }

    Ok(SlayerTaskHistoryPage {
        entries,
        page,
        page_size,
        total_entries,
        total_pages,
    })
}

fn slayer_task_leader_from_row(row: &Row) -> Result<SlayerTaskLeader, ApiError> {
    Ok(SlayerTaskLeader {
        name: row.try_get(0)?,
        count: row.try_get(1)?,
    })
}

async fn top_slayer_task_name(
    client: &Client,
    group_id: i64,
    member_name: &str,
    status_filter: Option<&str>,
    order_by: &str,
) -> Result<Option<SlayerTaskLeader>, ApiError> {
    let sql = format!(
        r#"
SELECT task_name, {order_by} AS n
FROM groupscape.slayer_task_history
WHERE group_id=$1 AND member_name=$2 {status_clause}
GROUP BY task_name
ORDER BY n DESC
LIMIT 1
"#,
        order_by = order_by,
        status_clause = if status_filter.is_some() {
            "AND status = $3"
        } else {
            ""
        },
    );
    let stmt = client.prepare_cached(&sql).await?;
    let rows = if let Some(status) = status_filter {
        client.query(&stmt, &[&group_id, &member_name, &status]).await?
    } else {
        client.query(&stmt, &[&group_id, &member_name]).await?
    };
    rows.first().map(slayer_task_leader_from_row).transpose()
}

/// Completed-history kill total for one specific task name, used to fold the live in-progress
/// task's kills into whichever task_name it belongs to when computing `most_killed_task`.
async fn completed_kills_for_task_name(
    client: &Client,
    group_id: i64,
    member_name: &str,
    task_name: &str,
) -> Result<i64, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
SELECT COALESCE(SUM(amount_done), 0) AS n
FROM groupscape.slayer_task_history
WHERE group_id=$1 AND member_name=$2 AND status='completed' AND task_name=$3
"#,
        )
        .await?;
    let row = client.query_one(&stmt, &[&group_id, &member_name, &task_name]).await?;
    Ok(row.try_get("n")?)
}

/// All-time slayer task stats for one member - the Stats tab. Every "most X" tile is its own
/// small `GROUP BY` query rather than one combined query, since each ranks a different subset
/// (all tasks / completed only / cancelled only) - a single query would need a
/// `FILTER` per aggregate anyway, and these are cheap enough (indexed on `(group_id,
/// member_name)`) that clarity wins over shaving a handful of round trips.
pub async fn get_slayer_task_stats(
    client: &Client,
    group_id: i64,
    member_name: &str,
) -> Result<SlayerTaskStats, ApiError> {
    let totals_stmt = client
        .prepare_cached(
            r#"
SELECT
  COUNT(*) FILTER (WHERE status = 'completed') AS tasks_completed,
  COALESCE(SUM(amount_done) FILTER (WHERE status = 'completed'), 0) AS total_kills,
  COALESCE(SUM(points) FILTER (WHERE status = 'completed'), 0) AS total_points_earned,
  COUNT(*) FILTER (WHERE status IN ('completed', 'cancelled', 'blocked', 'reset')) AS closed_count
FROM groupscape.slayer_task_history
WHERE group_id=$1 AND member_name=$2
"#,
        )
        .await?;
    let totals_row = client
        .query_one(&totals_stmt, &[&group_id, &member_name])
        .await?;
    let tasks_completed: i64 = totals_row.try_get("tasks_completed")?;
    // Fetched once and reused below by both `total_kills` and `most_killed_task`, since both
    // need to overlay the current in-progress task's live kill count (off
    // `groupscape.members.slayer_task`, the same source the Current tab reads) on top of
    // completed-task history - so those tiles visibly tick up while a task is being worked
    // rather than jumping only when it closes. See `get_live_slayer_task`.
    let live_task = get_live_slayer_task(client, group_id, member_name).await?;
    let live_amount_done = live_task.as_ref().and_then(live_slayer_task_amount_done);

    let mut total_kills: i64 = totals_row.try_get("total_kills")?;
    if let Some(live_amount_done) = live_amount_done {
        total_kills += live_amount_done as i64;
    }
    let total_points_earned: i64 = totals_row.try_get("total_points_earned")?;
    let closed_count: i64 = totals_row.try_get("closed_count")?;
    let completion_rate = if closed_count > 0 {
        ((tasks_completed as f64 / closed_count as f64) * 100.0).round() as i32
    } else {
        0
    };

    let most_killed_task = top_slayer_task_name(
        client,
        group_id,
        member_name,
        Some("completed"),
        "COALESCE(SUM(amount_done), 0)",
    )
    .await?;
    // Overlay the live task's kills onto whichever task_name they belong to, so a task in
    // progress on the current leader (or one about to overtake it) is reflected immediately
    // instead of only once it closes and lands in `slayer_task_history`.
    let most_killed_task = match (live_task.as_ref().and_then(|t| t.task_name.as_deref()), live_amount_done) {
        (Some(live_task_name), Some(live_amount_done)) => {
            let completed_for_live_task = completed_kills_for_task_name(
                client,
                group_id,
                member_name,
                live_task_name,
            )
            .await?;
            let live_total = completed_for_live_task + live_amount_done as i64;
            match &most_killed_task {
                Some(leader) if leader.count >= live_total => most_killed_task,
                _ => Some(SlayerTaskLeader {
                    name: live_task_name.to_string(),
                    count: live_total,
                }),
            }
        }
        _ => most_killed_task,
    };
    let most_common_task =
        top_slayer_task_name(client, group_id, member_name, None, "COUNT(*)").await?;
    let most_cancelled_task =
        top_slayer_task_name(client, group_id, member_name, Some("cancelled"), "COUNT(*)").await?;

    let master_stmt = client
        .prepare_cached(
            r#"
SELECT master_name, COUNT(*) AS n
FROM groupscape.slayer_task_history
WHERE group_id=$1 AND member_name=$2
GROUP BY master_name
ORDER BY n DESC
LIMIT 1
"#,
        )
        .await?;
    let most_common_master = client
        .query(&master_stmt, &[&group_id, &member_name])
        .await?
        .first()
        .map(slayer_task_leader_from_row)
        .transpose()?;

    let modifier_stmt = client
        .prepare_cached(
            r#"
SELECT modifier_type, modifier_negative, COUNT(*) AS n
FROM groupscape.slayer_task_history
WHERE group_id=$1 AND member_name=$2 AND modifier_type IS NOT NULL
GROUP BY modifier_type, modifier_negative
ORDER BY n DESC
LIMIT 1
"#,
        )
        .await?;
    let most_common_modifier = client
        .query(&modifier_stmt, &[&group_id, &member_name])
        .await?
        .first()
        .map(|row| {
            Ok::<_, ApiError>(SlayerModifierLeader {
                modifier_type: row.try_get("modifier_type")?,
                // `modifier_negative` is only ever set for `"quantity"`; every other modifier
                // type is a plain positive boost, so a NULL here reads as `false`.
                modifier_negative: row.try_get::<_, Option<bool>>("modifier_negative")?.unwrap_or(false),
                count: row.try_get("n")?,
            })
        })
        .transpose()?;

    let fastest_stmt = client
        .prepare_cached(
            r#"
SELECT task_name, EXTRACT(EPOCH FROM (closed_at - assigned_at))::bigint AS seconds
FROM groupscape.slayer_task_history
WHERE group_id=$1 AND member_name=$2 AND status = 'completed' AND closed_at IS NOT NULL
ORDER BY seconds ASC
LIMIT 1
"#,
        )
        .await?;
    let fastest_completed_task = client
        .query(&fastest_stmt, &[&group_id, &member_name])
        .await?
        .first()
        .map(|row| {
            Ok::<_, ApiError>(SlayerTaskDurationLeader {
                name: row.try_get("task_name")?,
                seconds: row.try_get("seconds")?,
            })
        })
        .transpose()?;

    let boss_task_names = slayer_boss_tasks::names();
    let boss_stmt = client
        .prepare_cached(
            r#"
SELECT task_name, COUNT(*) AS n
FROM groupscape.slayer_task_history
WHERE group_id=$1 AND member_name=$2 AND lower(task_name) = ANY($3)
GROUP BY task_name
ORDER BY n DESC
LIMIT 1
"#,
        )
        .await?;
    let most_common_boss_task = client
        .query(&boss_stmt, &[&group_id, &member_name, &boss_task_names])
        .await?
        .first()
        .map(slayer_task_leader_from_row)
        .transpose()?;

    let boss_count_stmt = client
        .prepare_cached(
            r#"
SELECT COUNT(*) AS n
FROM groupscape.slayer_task_history
WHERE group_id=$1 AND member_name=$2 AND lower(task_name) = ANY($3)
"#,
        )
        .await?;
    let boss_tasks_count: i64 = client
        .query_one(&boss_count_stmt, &[&group_id, &member_name, &boss_task_names])
        .await?
        .try_get("n")?;

    let boss_kills_stmt = client
        .prepare_cached(
            r#"
SELECT COALESCE(SUM(amount_done), 0) AS n
FROM groupscape.slayer_task_history
WHERE group_id=$1 AND member_name=$2 AND status = 'completed' AND lower(task_name) = ANY($3)
"#,
        )
        .await?;
    let mut boss_task_kills: i64 = client
        .query_one(&boss_kills_stmt, &[&group_id, &member_name, &boss_task_names])
        .await?
        .try_get("n")?;
    // Same live overlay as `total_kills` above - if the in-progress task is itself a boss task,
    // its kills should tick up immediately rather than only landing once it closes.
    if let (Some(live_task_name), Some(live_amount_done)) =
        (live_task.as_ref().and_then(|t| t.task_name.as_deref()), live_amount_done)
    {
        if slayer_boss_tasks::is_boss_task(live_task_name) {
            boss_task_kills += live_amount_done as i64;
        }
    }

    Ok(SlayerTaskStats {
        tasks_completed,
        total_kills,
        total_points_earned,
        completion_rate,
        most_killed_task,
        most_common_task,
        most_common_master,
        most_cancelled_task,
        most_common_modifier,
        fastest_completed_task,
        most_common_boss_task,
        boss_tasks_count,
        boss_task_kills,
    })
}

fn group_session_from_row(row: &Row) -> Result<GroupSession, ApiError> {
    Ok(GroupSession {
        id: row.try_get("session_id")?,
        started_at: row.try_get("started_at")?,
        ended_at: row.try_get("ended_at")?,
    })
}

/// Newest-first session list for a group (open session, if any, sorts first since it has no
/// `ended_at`... actually ordered by `started_at DESC` so it's always first regardless).
pub async fn list_sessions(
    client: &Client,
    group_id: i64,
    limit: i64,
) -> Result<Vec<GroupSession>, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT session_id, started_at, ended_at FROM groupscape.sessions WHERE group_id=$1 ORDER BY started_at DESC LIMIT $2",
        )
        .await?;
    let rows = client
        .query(&stmt, &[&group_id, &limit.clamp(1, 200)])
        .await
        .map_err(ApiError::ListSessionsError)?;
    rows.iter().map(group_session_from_row).collect()
}

pub async fn admin_record_audit_log(
    client: &Client,
    action: &str,
    target_type: Option<&str>,
    target_id: Option<&str>,
    detail: Option<serde_json::Value>,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            "INSERT INTO groupscape.admin_audit_log (action, target_type, target_id, detail) VALUES ($1, $2, $3, $4)",
        )
        .await?;
    client
        .execute(&stmt, &[&action, &target_type, &target_id, &detail])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminRecordAuditLogError".to_string(), e))?;
    Ok(())
}

pub async fn admin_list_groups(
    client: &Client,
    search: Option<&str>,
    page: i64,
    page_size: i64,
) -> Result<(Vec<AdminGroupSummary>, i64), ApiError> {
    let offset = (page.max(1) - 1) * page_size.max(1);
    let search_pattern = search.map(|s| format!("%{}%", s));

    let list_stmt = client
        .prepare_cached(
            r#"
SELECT g.group_id, g.group_name, g.version,
  (SELECT COUNT(*) FROM groupscape.members m WHERE m.group_id = g.group_id AND m.member_name != $1) AS member_count,
  COALESCE(gm.status, 'active') AS status
FROM groupscape.groups g
LEFT JOIN groupscape.group_moderation gm ON gm.group_id = g.group_id
WHERE $2::text IS NULL OR g.group_name ILIKE $2
ORDER BY g.group_id DESC
LIMIT $3 OFFSET $4
"#,
        )
        .await?;
    let rows = client
        .query(
            &list_stmt,
            &[&SHARED_MEMBER, &search_pattern, &page_size.max(1), &offset],
        )
        .await
        .map_err(|e| ApiError::AdminDbError("AdminListGroupsError".to_string(), e))?;

    let groups = rows
        .into_iter()
        .map(|row| AdminGroupSummary {
            group_id: row.get("group_id"),
            group_name: row.get("group_name"),
            version: row.get("version"),
            member_count: row.get("member_count"),
            status: row.get("status"),
        })
        .collect();

    let count_stmt = client
        .prepare_cached(
            "SELECT COUNT(*) FROM groupscape.groups g WHERE $1::text IS NULL OR g.group_name ILIKE $1",
        )
        .await?;
    let total: i64 = client
        .query_one(&count_stmt, &[&search_pattern])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminCountGroupsError".to_string(), e))?
        .try_get(0)?;

    Ok((groups, total))
}

pub async fn admin_get_group(
    client: &Client,
    group_id: i64,
) -> Result<Option<AdminGroupDetail>, ApiError> {
    let group_stmt = client
        .prepare_cached(
            r#"
SELECT g.group_id, g.group_name, g.version, COALESCE(gm.status, 'active') AS status, gm.reason
FROM groupscape.groups g
LEFT JOIN groupscape.group_moderation gm ON gm.group_id = g.group_id
WHERE g.group_id = $1
"#,
        )
        .await?;
    let group_row = client
        .query_opt(&group_stmt, &[&group_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminGetGroupError".to_string(), e))?;
    let Some(group_row) = group_row else {
        return Ok(None);
    };

    let members_stmt = client
        .prepare_cached(
            "SELECT member_id, member_name FROM groupscape.members WHERE group_id = $1 AND member_name != $2 ORDER BY member_name",
        )
        .await?;
    let member_rows = client
        .query(&members_stmt, &[&group_id, &SHARED_MEMBER])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminGetGroupMembersError".to_string(), e))?;
    let members = member_rows
        .into_iter()
        .map(|row| AdminGroupMember {
            member_id: row.get("member_id"),
            member_name: row.get("member_name"),
        })
        .collect();

    Ok(Some(AdminGroupDetail {
        group_id: group_row.get("group_id"),
        group_name: group_row.get("group_name"),
        version: group_row.get("version"),
        status: group_row.get("status"),
        reason: group_row.get("reason"),
        members,
    }))
}

pub async fn admin_set_group_moderation(
    client: &Client,
    group_id: i64,
    status: &str,
    reason: Option<&str>,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
INSERT INTO groupscape.group_moderation (group_id, status, reason, updated_at)
VALUES ($1, $2, $3, now())
ON CONFLICT (group_id) DO UPDATE SET status = $2, reason = $3, updated_at = now()
"#,
        )
        .await?;
    client
        .execute(&stmt, &[&group_id, &status, &reason])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminSetGroupModerationError".to_string(), e))?;
    Ok(())
}

pub async fn admin_delete_group(client: &mut Client, group_id: i64) -> Result<(), ApiError> {
    let member_id_stmt = client
        .prepare_cached("SELECT member_id FROM groupscape.members WHERE group_id = $1")
        .await?;
    let member_ids: Vec<i64> = client
        .query(&member_id_stmt, &[&group_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminDeleteGroupMembersLookupError".to_string(), e))?
        .into_iter()
        .map(|row| row.get(0))
        .collect();

    let transaction = client.transaction().await?;

    for member_id in member_ids {
        delete_skills_data_for_member(&transaction, AggregatePeriod::Day, member_id).await?;
        delete_skills_data_for_member(&transaction, AggregatePeriod::Month, member_id).await?;
        delete_skills_data_for_member(&transaction, AggregatePeriod::Year, member_id).await?;
        delete_bank_value_data_for_member(&transaction, AggregatePeriod::Day, member_id).await?;
        delete_bank_value_data_for_member(&transaction, AggregatePeriod::Month, member_id).await?;
        delete_bank_value_data_for_member(&transaction, AggregatePeriod::Year, member_id).await?;
    }

    let delete_members_stmt = transaction
        .prepare_cached("DELETE FROM groupscape.members WHERE group_id = $1")
        .await?;
    transaction
        .execute(&delete_members_stmt, &[&group_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminDeleteGroupMembersError".to_string(), e))?;

    let delete_character_links_stmt = transaction
        .prepare_cached("DELETE FROM groupscape.character_group_links WHERE group_id = $1")
        .await?;
    transaction
        .execute(&delete_character_links_stmt, &[&group_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminDeleteGroupCharacterLinksError".to_string(), e))?;

    let delete_moderation_stmt = transaction
        .prepare_cached("DELETE FROM groupscape.group_moderation WHERE group_id = $1")
        .await?;
    transaction
        .execute(&delete_moderation_stmt, &[&group_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminDeleteGroupModerationError".to_string(), e))?;

    let delete_group_stmt = transaction
        .prepare_cached("DELETE FROM groupscape.groups WHERE group_id = $1")
        .await?;
    transaction
        .execute(&delete_group_stmt, &[&group_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminDeleteGroupError".to_string(), e))?;

    transaction
        .commit()
        .await
        .map_err(|e| ApiError::AdminDbError("AdminDeleteGroupCommitError".to_string(), e))?;

    Ok(())
}

/// Deletes every row the activity feed and Loot Log views surface for a group (union of
/// `list_activity_events`'s and `list_loot_and_kill_events_page`'s event_type filters). The two
/// views read the same table with overlapping event_types (e.g. `kill`), so admins clear them
/// together rather than picking which view's rows to keep.
pub async fn admin_clear_logs(client: &Client, group_id: i64) -> Result<u64, ApiError> {
    let stmt = client
        .prepare_cached("DELETE FROM groupscape.activity_events WHERE group_id = $1")
        .await?;
    client
        .execute(&stmt, &[&group_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminClearLogsError".to_string(), e))
}

/// Deletes specific activity feed rows an admin selected from the moderation view. Scoped by
/// `group_id` as well as `event_id` so a stale/tampered id list can never reach into another
/// group's rows.
pub async fn admin_delete_activity_events(
    client: &Client,
    group_id: i64,
    ids: &[i64],
) -> Result<u64, ApiError> {
    let stmt = client
        .prepare_cached(
            "DELETE FROM groupscape.activity_events WHERE group_id = $1 AND event_id = ANY($2)",
        )
        .await?;
    client
        .execute(&stmt, &[&group_id, &ids])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminDeleteActivityEventsError".to_string(), e))
}

const CLEARABLE_MEMBER_DATA_TYPES: &[&str] = &[
    "collection_log",
    "combat_achievements",
    "skill_xp_history",
    "bank_value_history",
];

/// Applies the group-detail "Data management" matrix selection: a set of (member_id, data_type)
/// pairs. Collection log / combat achievements live as columns on `members` and reset in place;
/// skill/XP and bank-value history are per-member tables and get their rows deleted. Silently
/// skips any member_id that isn't actually in `group_id`, so a stale selection can't reach into
/// another group.
pub async fn admin_clear_member_data(
    client: &mut Client,
    group_id: i64,
    items: &[(i64, String)],
) -> Result<(), ApiError> {
    for (_, data_type) in items {
        if !CLEARABLE_MEMBER_DATA_TYPES.contains(&data_type.as_str()) {
            return Err(ApiError::AdminInvalidRequestError(format!(
                "Unknown data_type: {data_type}"
            )));
        }
    }

    let member_id_stmt = client
        .prepare_cached("SELECT member_id FROM groupscape.members WHERE group_id = $1")
        .await?;
    let valid_member_ids: HashSet<i64> = client
        .query(&member_id_stmt, &[&group_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminClearMemberDataLookupError".to_string(), e))?
        .into_iter()
        .map(|row| row.get(0))
        .collect();

    let transaction = client.transaction().await?;

    for (member_id, data_type) in items {
        if !valid_member_ids.contains(member_id) {
            continue;
        }
        match data_type.as_str() {
            "collection_log" => {
                let stmt = transaction
                    .prepare_cached(
                        "UPDATE groupscape.members SET collection_log = NULL, collection_log_last_update = NULL WHERE member_id = $1",
                    )
                    .await?;
                transaction
                    .execute(&stmt, &[member_id])
                    .await
                    .map_err(|e| {
                        ApiError::AdminDbError("AdminClearCollectionLogError".to_string(), e)
                    })?;
            }
            "combat_achievements" => {
                let stmt = transaction
                    .prepare_cached(
                        "UPDATE groupscape.members SET combat_achievements = NULL, combat_achievements_last_update = NULL WHERE member_id = $1",
                    )
                    .await?;
                transaction
                    .execute(&stmt, &[member_id])
                    .await
                    .map_err(|e| {
                        ApiError::AdminDbError("AdminClearCombatAchievementsError".to_string(), e)
                    })?;
            }
            "skill_xp_history" => {
                delete_skills_data_for_member(&transaction, AggregatePeriod::Day, *member_id)
                    .await?;
                delete_skills_data_for_member(&transaction, AggregatePeriod::Month, *member_id)
                    .await?;
                delete_skills_data_for_member(&transaction, AggregatePeriod::Year, *member_id)
                    .await?;
            }
            "bank_value_history" => {
                delete_bank_value_data_for_member(&transaction, AggregatePeriod::Day, *member_id)
                    .await?;
                delete_bank_value_data_for_member(&transaction, AggregatePeriod::Month, *member_id)
                    .await?;
                delete_bank_value_data_for_member(&transaction, AggregatePeriod::Year, *member_id)
                    .await?;
                let stmt = transaction
                    .prepare_cached(
                        "DELETE FROM groupscape.bank_value_snapshots WHERE member_id = $1",
                    )
                    .await?;
                transaction.execute(&stmt, &[member_id]).await.map_err(|e| {
                    ApiError::AdminDbError("AdminClearBankValueSnapshotsError".to_string(), e)
                })?;
            }
            _ => unreachable!("data_type validated above"),
        }
    }

    transaction
        .commit()
        .await
        .map_err(|e| ApiError::AdminDbError("AdminClearMemberDataCommitError".to_string(), e))?;

    Ok(())
}

pub async fn admin_list_audit_log(
    client: &Client,
    page: i64,
    page_size: i64,
) -> Result<(Vec<AdminAuditLogEntry>, i64), ApiError> {
    let offset = (page.max(1) - 1) * page_size.max(1);

    let list_stmt = client
        .prepare_cached(
            r#"
SELECT id, action, target_type, target_id, detail, created_at
FROM groupscape.admin_audit_log
ORDER BY created_at DESC
LIMIT $1 OFFSET $2
"#,
        )
        .await?;
    let rows = client
        .query(&list_stmt, &[&page_size.max(1), &offset])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminListAuditLogError".to_string(), e))?;
    let entries = rows
        .into_iter()
        .map(|row| AdminAuditLogEntry {
            id: row.get("id"),
            action: row.get("action"),
            target_type: row.get("target_type"),
            target_id: row.get("target_id"),
            detail: row.get("detail"),
            created_at: row.get("created_at"),
        })
        .collect();

    let total: i64 = client
        .query_one("SELECT COUNT(*) FROM groupscape.admin_audit_log", &[])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminCountAuditLogError".to_string(), e))?
        .try_get(0)?;

    Ok((entries, total))
}

pub async fn admin_count_accounts(client: &Client) -> Result<i64, ApiError> {
    let count: i64 = client
        .query_one("SELECT COUNT(*) FROM groupscape.accounts", &[])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminCountAccountsError".to_string(), e))?
        .try_get(0)?;
    Ok(count)
}

// --- Leaderboards ---
//
// XP reuses the Graphs tab's existing `skills_day`/`skills_month`/`skills_year` history. The
// stored `skills INTEGER[24]` column has no slot for "Overall" - it holds only the 24
// non-Overall skills (see `skill_array_index`'s doc comment), so "Overall" XP is the sum of all
// 24 elements, and a specific skill's XP is a single 1-indexed array read. Boss-KC and
// loot-value are computed live from `activity_events`, which is already unbounded/retained - no
// snapshot needed. Only GP-earned needs new history, since bank *contents* aren't tracked
// anywhere else; see `bank_value_snapshots` above.

fn leaderboard_window_cutoff(
    window: crate::leaderboard::LeaderboardWindow,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    use crate::leaderboard::LeaderboardWindow;
    match window {
        LeaderboardWindow::Daily => Some(now - chrono::Duration::days(1)),
        LeaderboardWindow::Weekly => Some(now - chrono::Duration::days(7)),
        LeaderboardWindow::AllTime => None,
    }
}

/// Maps a `SkillName` string (exactly as the client sends it, e.g. `"Woodcutting"`) to its
/// 1-indexed position in the stored `skills INTEGER[24]` column. The column has no slot for
/// "Overall" - it holds exactly the 24 non-Overall skills in the order `Object.keys(SkillName)`
/// visits them client-side (site/src/data/skill.js's `SkillName`, alphabetical except Overall is
/// skipped and Sailing trails), so 0-indexed slot 0=Agility ... 23=Sailing, i.e. Postgres
/// skills[1]=Agility ... skills[24]=Sailing. Returns `None` for "Overall" or any unrecognized
/// string, both of which the caller treats as "sum all 24 elements" rather than a single read.
fn skill_array_index(name: &str) -> Option<i32> {
    Some(match name {
        "Agility" => 1,
        "Attack" => 2,
        "Construction" => 3,
        "Cooking" => 4,
        "Crafting" => 5,
        "Defence" => 6,
        "Farming" => 7,
        "Firemaking" => 8,
        "Fishing" => 9,
        "Fletching" => 10,
        "Herblore" => 11,
        "Hitpoints" => 12,
        "Hunter" => 13,
        "Magic" => 14,
        "Mining" => 15,
        "Prayer" => 16,
        "Ranged" => 17,
        "Runecraft" => 18,
        "Slayer" => 19,
        "Smithing" => 20,
        "Strength" => 21,
        "Thieving" => 22,
        "Woodcutting" => 23,
        "Sailing" => 24,
        _ => return None,
    })
}

/// SQL fragment reading a member's XP for the given (optional, validated) skill: a single
/// 1-indexed array read for a specific skill, or a sum of all 24 elements for "Overall"/`None`/
/// an unrecognized skill name. `column` is the fully-qualified array column reference to read
/// (e.g. `"skills"` or `"s.skills"`) - always a fixed literal from this module, never
/// interpolated from user input.
fn xp_read_expr(column: &str, skill: Option<&str>) -> String {
    match skill.and_then(skill_array_index) {
        Some(index) => format!("COALESCE({column}[{index}], 0)::BIGINT"),
        None => format!("(SELECT COALESCE(SUM(v), 0)::BIGINT FROM unnest({column}) AS v)"),
    }
}

/// XP gained in the window: live total (Overall sum, or a single skill's slot when `skill` is
/// `Some`) minus the oldest available history-table reading at/after (daily) or at/before
/// (weekly/all-time) the cutoff. All-time has no cutoff - it diffs against the earliest
/// surviving row across every history table, whatever that is (those tables never fully delete
/// a member's last row, so there's always some baseline).
///
/// `skill` is a `SkillName` string exactly as the client sends it (e.g. `"Woodcutting"`); `None`
/// or `"Overall"` (or any string this server doesn't recognize) means "Overall" - the sum of all
/// 24 stored skills. Note this fixes a pre-existing bug: this function used to read `skills[1]`
/// unconditionally for "Overall" XP, which is actually Agility's slot, not a sum - see
/// `skill_array_index`'s doc comment for the stored-array layout.
pub async fn get_xp_leaderboard(
    client: &Client,
    group_id: i64,
    window: crate::leaderboard::LeaderboardWindow,
    skill: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Vec<(String, i64)>, ApiError> {
    use crate::leaderboard::LeaderboardWindow;

    let live_sql = format!(
        "SELECT member_name, {} AS xp FROM groupscape.members WHERE group_id=$1 AND member_name != $2",
        xp_read_expr("skills", skill)
    );
    let live_stmt = client.prepare_cached(&live_sql).await?;
    let live_rows = client
        .query(&live_stmt, &[&group_id, &SHARED_MEMBER])
        .await
        .map_err(ApiError::GetLeaderboardSnapshotsError)?;
    let mut live: HashMap<String, i64> = HashMap::new();
    for row in &live_rows {
        let member_name: String = row.try_get("member_name")?;
        let xp: i64 = row.try_get("xp")?;
        live.insert(member_name, xp);
    }

    let xp_expr = xp_read_expr("s.skills", skill);
    let baseline_sql = match window {
        LeaderboardWindow::Daily => {
            format!(
                "SELECT DISTINCT ON (m.member_name) m.member_name, {xp_expr} AS xp
             FROM groupscape.skills_day s JOIN groupscape.members m ON m.member_id = s.member_id
             WHERE m.group_id = $1 AND s.time >= $2
             ORDER BY m.member_name, s.time ASC"
            )
        }
        LeaderboardWindow::Weekly => {
            format!(
                "SELECT DISTINCT ON (m.member_name) m.member_name, {xp_expr} AS xp
             FROM groupscape.skills_month s JOIN groupscape.members m ON m.member_id = s.member_id
             WHERE m.group_id = $1 AND s.time <= $2
             ORDER BY m.member_name, s.time DESC"
            )
        }
        LeaderboardWindow::AllTime => {
            format!(
                "SELECT DISTINCT ON (m.member_name) m.member_name, {xp_expr} AS xp
             FROM groupscape.skills_year s JOIN groupscape.members m ON m.member_id = s.member_id
             WHERE m.group_id = $1
             ORDER BY m.member_name, s.time ASC"
            )
        }
    };
    let cutoff = leaderboard_window_cutoff(window, now);
    let baseline_stmt = client.prepare_cached(&baseline_sql).await?;
    let baseline_rows = match cutoff {
        Some(cutoff) => client
            .query(&baseline_stmt, &[&group_id, &cutoff])
            .await
            .map_err(ApiError::GetLeaderboardSnapshotsError)?,
        None => client
            .query(&baseline_stmt, &[&group_id])
            .await
            .map_err(ApiError::GetLeaderboardSnapshotsError)?,
    };
    let mut baseline: HashMap<String, i64> = HashMap::new();
    for row in &baseline_rows {
        let member_name: String = row.try_get("member_name")?;
        let xp: i64 = row.try_get("xp")?;
        baseline.insert(member_name, xp);
    }

    Ok(live
        .into_iter()
        .map(|(member_name, xp)| {
            let base = baseline.get(&member_name).copied().unwrap_or(xp);
            (member_name, xp - base)
        })
        .collect())
}

/// All `kill` activity events for a group since `since` (`None` = unbounded/all-time) - unlike
/// `list_kill_events`, no `LIMIT`, since leaderboard aggregation needs the true total rather
/// than a recency-capped page.
async fn list_kill_events_since(
    client: &Client,
    group_id: i64,
    since: Option<DateTime<Utc>>,
) -> Result<Vec<ActivityEvent>, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
SELECT event_id, session_id, member_name, event_type, occurred_at, payload
FROM groupscape.activity_events
WHERE group_id=$1
  AND event_type='kill'
  AND ($2::timestamptz IS NULL OR occurred_at >= $2)
ORDER BY occurred_at DESC
"#,
        )
        .await?;
    let rows = client
        .query(&stmt, &[&group_id, &since])
        .await
        .map_err(ApiError::ListKillEventsError)?;
    rows.iter().map(activity_event_from_row).collect()
}

/// Every member's name in a group - used to zero-fill leaderboard metrics computed from
/// `activity_events` so a member with no matching events still ranks (at 0) rather than being
/// omitted outright.
async fn list_member_names(client: &Client, group_id: i64) -> Result<Vec<String>, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT member_name FROM groupscape.members WHERE group_id=$1 AND member_name != $2",
        )
        .await?;
    let rows = client
        .query(&stmt, &[&group_id, &SHARED_MEMBER])
        .await
        .map_err(ApiError::GetLeaderboardSnapshotsError)?;
    rows.iter()
        .map(|row| row.try_get("member_name").map_err(ApiError::from))
        .collect()
}

/// Cumulative per-member boss-kill counts (all bosses, or one `boss` filter) over the window,
/// plus the set of bosses the group has actually killed (for the site's boss-filter dropdown).
pub async fn get_boss_kc_leaderboard(
    client: &Client,
    group_id: i64,
    window: crate::leaderboard::LeaderboardWindow,
    boss: Option<&str>,
    now: DateTime<Utc>,
) -> Result<(Vec<(String, i64)>, Vec<String>), ApiError> {
    let events =
        list_kill_events_since(client, group_id, leaderboard_window_cutoff(window, now)).await?;

    let mut counts: HashMap<String, i64> = list_member_names(client, group_id)
        .await?
        .into_iter()
        .map(|name| (name, 0))
        .collect();
    let mut available_bosses: HashSet<String> = HashSet::new();
    for event in &events {
        let Ok(GameEvent::Kill(kill)) = serde_json::from_value::<GameEvent>(event.payload.clone())
        else {
            continue;
        };
        available_bosses.insert(kill.npc_name.clone());
        if boss.is_some_and(|b| b != kill.npc_name) {
            continue;
        }
        *counts.entry(event.member_name.clone()).or_insert(0) += 1;
    }

    let mut available: Vec<String> = available_bosses.into_iter().collect();
    available.sort();
    Ok((counts.into_iter().collect(), available))
}

/// Sums a flat `[item_id, quantity, item_id, quantity, ...]` array (this server's on-wire
/// item-pairs format, see `validate_member_prop_length`'s `ArrayFormat::ItemPairs` - bank,
/// inventory, and equipment all use it) into a GE-priced total.
fn bank_value(bank: &[i32], ge_prices: &crate::models::GEPrices) -> i64 {
    bank.chunks_exact(2)
        .map(|pair| {
            let (item_id, quantity) = (pair[0], pair[1]);
            ge_prices.get(&item_id).copied().unwrap_or(0) * quantity as i64
        })
        .sum()
}

/// "GP earned" net worth: bank + inventory + equipped items combined, so the metric reflects a
/// member's total holdings day-to-day rather than just what's banked.
fn net_worth_value(
    bank: &[i32],
    inventory: &[i32],
    equipment: &[i32],
    ge_prices: &crate::models::GEPrices,
) -> i64 {
    bank_value(bank, ge_prices) + bank_value(inventory, ge_prices) + bank_value(equipment, ge_prices)
}

/// Captures each member's current bank value once per UTC day - the only leaderboard metric
/// that needs its own history table, since nothing else records what a member's bank was worth
/// on any past day. Upserted on `(member_id, snapshot_date)` so a re-run within the same day
/// (e.g. a restarted job) just refreshes today's figure.
pub async fn capture_bank_value_snapshots(
    client: &Client,
    ge_prices: &crate::models::GEPrices,
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    let members_stmt = client
        .prepare_cached(
            "SELECT member_id, COALESCE(bank, ARRAY[]::INTEGER[]) AS bank,
                    COALESCE(inventory, ARRAY[]::INTEGER[]) AS inventory,
                    COALESCE(equipment, ARRAY[]::INTEGER[]) AS equipment
             FROM groupscape.members
             WHERE bank IS NOT NULL OR inventory IS NOT NULL OR equipment IS NOT NULL",
        )
        .await?;
    let rows = client
        .query(&members_stmt, &[])
        .await
        .map_err(ApiError::CaptureLeaderboardSnapshotsError)?;

    let upsert_stmt = client
        .prepare_cached(
            r#"
INSERT INTO groupscape.bank_value_snapshots (member_id, snapshot_date, captured_at, bank_value)
VALUES ($1, $2, $3, $4)
ON CONFLICT (member_id, snapshot_date) DO UPDATE SET captured_at=excluded.captured_at, bank_value=excluded.bank_value
"#,
        )
        .await?;
    let snapshot_date = now.date_naive();
    for row in &rows {
        let member_id: i64 = row.try_get("member_id")?;
        let bank: Vec<i32> = row.try_get("bank")?;
        let inventory: Vec<i32> = row.try_get("inventory")?;
        let equipment: Vec<i32> = row.try_get("equipment")?;
        let value = net_worth_value(&bank, &inventory, &equipment, ge_prices);
        client
            .execute(&upsert_stmt, &[&member_id, &snapshot_date, &now, &value])
            .await
            .map_err(ApiError::CaptureLeaderboardSnapshotsError)?;
    }
    Ok(())
}

/// GP earned in the window: live bank value minus the nearest snapshot at/before the cutoff
/// (`None` cutoff for all-time uses the earliest snapshot ever captured for that member).
pub async fn get_gp_earned_leaderboard(
    client: &Client,
    group_id: i64,
    window: crate::leaderboard::LeaderboardWindow,
    now: DateTime<Utc>,
    ge_prices: &crate::models::GEPrices,
) -> Result<Vec<(String, i64)>, ApiError> {
    use crate::leaderboard::LeaderboardWindow;

    let live_stmt = client
        .prepare_cached(
            "SELECT member_name, COALESCE(bank, ARRAY[]::INTEGER[]) AS bank,
                    COALESCE(inventory, ARRAY[]::INTEGER[]) AS inventory,
                    COALESCE(equipment, ARRAY[]::INTEGER[]) AS equipment
             FROM groupscape.members WHERE group_id=$1 AND member_name != $2",
        )
        .await?;
    let live_rows = client
        .query(&live_stmt, &[&group_id, &SHARED_MEMBER])
        .await
        .map_err(ApiError::GetLeaderboardSnapshotsError)?;
    let mut live: HashMap<String, i64> = HashMap::new();
    for row in &live_rows {
        let member_name: String = row.try_get("member_name")?;
        let bank: Vec<i32> = row.try_get("bank")?;
        let inventory: Vec<i32> = row.try_get("inventory")?;
        let equipment: Vec<i32> = row.try_get("equipment")?;
        live.insert(member_name, net_worth_value(&bank, &inventory, &equipment, ge_prices));
    }

    let baseline_sql = match window {
        LeaderboardWindow::AllTime => {
            "SELECT DISTINCT ON (m.member_name) m.member_name, b.bank_value
             FROM groupscape.bank_value_snapshots b JOIN groupscape.members m ON m.member_id = b.member_id
             WHERE m.group_id = $1
             ORDER BY m.member_name, b.snapshot_date ASC"
        }
        LeaderboardWindow::Daily | LeaderboardWindow::Weekly => {
            "SELECT DISTINCT ON (m.member_name) m.member_name, b.bank_value
             FROM groupscape.bank_value_snapshots b JOIN groupscape.members m ON m.member_id = b.member_id
             WHERE m.group_id = $1 AND b.captured_at <= $2
             ORDER BY m.member_name, b.snapshot_date DESC"
        }
    };
    let cutoff = leaderboard_window_cutoff(window, now).unwrap_or(now);
    let baseline_stmt = client.prepare_cached(baseline_sql).await?;
    let baseline_rows = match window {
        LeaderboardWindow::AllTime => client
            .query(&baseline_stmt, &[&group_id])
            .await
            .map_err(ApiError::GetLeaderboardSnapshotsError)?,
        _ => client
            .query(&baseline_stmt, &[&group_id, &cutoff])
            .await
            .map_err(ApiError::GetLeaderboardSnapshotsError)?,
    };
    let mut baseline: HashMap<String, i64> = HashMap::new();
    for row in &baseline_rows {
        let member_name: String = row.try_get("member_name")?;
        let value: i64 = row.try_get("bank_value")?;
        baseline.insert(member_name, value);
    }

    Ok(live
        .into_iter()
        .map(|(member_name, value)| {
            let base = baseline.get(&member_name).copied().unwrap_or(value);
            (member_name, value - base)
        })
        .collect())
}

// --- Graphs tab metric data (get-metric-data) ---
//
// Backs the Graphs tab's chart controls once they merge with the leaderboard's period/metric
// picker. Boss-KC and loot-value are bucketed live from the same unbounded `activity_events`
// history the leaderboard metrics already read (queried with no `since` cutoff here, unlike the
// leaderboard's window-limited read, since the chart needs history from before the visible
// period to seed its baseline point - the client's `SkillGraph.generateCompleteTimeSeries`
// does the analogous thing for skills). GP-earned reads `bank_value_day/month/year`, this
// module's other new addition, which are cumulative-by-construction so no delta/running-sum step
// is needed for that one - see `get_bank_value_for_period`.

/// Bucket granularity for a chart period, mirroring `aggregate_skills_for_period`'s existing
/// Day→hour / Month→day / Year→month mapping.
fn bucket_granularity(period: AggregatePeriod) -> &'static str {
    match period {
        AggregatePeriod::Day => "hour",
        AggregatePeriod::Month => "day",
        AggregatePeriod::Year => "month",
    }
}

/// Truncates a timestamp down to the start of its containing hour/day/month bucket.
fn truncate_datetime(dt: DateTime<Utc>, granularity: &str) -> DateTime<Utc> {
    use chrono::{Datelike, Timelike};

    let date = dt.date_naive();
    let naive = match granularity {
        "hour" => date.and_hms_opt(dt.hour(), 0, 0).unwrap(),
        "month" => date.with_day(1).unwrap().and_hms_opt(0, 0, 0).unwrap(),
        // "day" and anything else fall back to a day bucket.
        _ => date.and_hms_opt(0, 0, 0).unwrap(),
    };
    naive.and_utc()
}

/// Turns per-member, per-bucket deltas into ascending, running-cumulative `GroupMetricData` -
/// members with no buckets at all are omitted entirely (same "absent means no data" shape
/// `get_skills_for_period` already has for a member with zero rows).
fn cumulative_metric_data(per_member: HashMap<String, BTreeMap<DateTime<Utc>, i64>>) -> GroupMetricData {
    per_member
        .into_iter()
        .filter(|(_, buckets)| !buckets.is_empty())
        .map(|(name, buckets)| {
            let mut running = 0i64;
            let metric_data = buckets
                .into_iter()
                .map(|(time, delta)| {
                    running += delta;
                    MetricDataPoint { time, value: running }
                })
                .collect();
            MemberMetricData { name, metric_data }
        })
        .collect()
}

/// Cumulative per-member boss-kill-count time series (all bosses summed, or one `boss` filter),
/// bucketed at `period`'s granularity - the chart counterpart to `get_boss_kc_leaderboard`.
pub async fn get_boss_kc_metric_data(
    client: &Client,
    group_id: i64,
    period: AggregatePeriod,
    boss: Option<&str>,
) -> Result<GroupMetricData, ApiError> {
    let events = list_kill_events_since(client, group_id, None).await?;
    let granularity = bucket_granularity(period);

    let mut per_member: HashMap<String, BTreeMap<DateTime<Utc>, i64>> = HashMap::new();
    for event in &events {
        let Ok(GameEvent::Kill(kill)) = serde_json::from_value::<GameEvent>(event.payload.clone())
        else {
            continue;
        };
        if boss.is_some_and(|b| b != kill.npc_name) {
            continue;
        }
        let bucket = truncate_datetime(event.occurred_at, granularity);
        *per_member
            .entry(event.member_name.clone())
            .or_default()
            .entry(bucket)
            .or_insert(0) += 1;
    }

    Ok(cumulative_metric_data(per_member))
}

/// All finalized `raid` events for a group since `since` (`None` = unbounded) - a completion
/// still inside `raid_merge`'s 5-minute open window is excluded by [`parse_finalized_raid`]
/// rather than here, since "finalized" lives in the payload, not a queryable column.
async fn list_raid_events_since(
    client: &Client,
    group_id: i64,
    since: Option<DateTime<Utc>>,
) -> Result<Vec<ActivityEvent>, ApiError> {
    let stmt = client
        .prepare_cached(
            r#"
SELECT event_id, session_id, member_name, event_type, occurred_at, payload
FROM groupscape.activity_events
WHERE group_id=$1
  AND event_type='raid'
  AND ($2::timestamptz IS NULL OR occurred_at >= $2)
ORDER BY occurred_at DESC
"#,
        )
        .await?;
    let rows = client
        .query(&stmt, &[&group_id, &since])
        .await
        .map_err(ApiError::ListKillEventsError)?;
    rows.iter().map(activity_event_from_row).collect()
}

/// Parses an `activity_events` row's payload as a [`RaidCompletionPayload`], returning `None` for
/// a malformed payload or one still inside `raid_merge`'s open merge window - an in-progress
/// completion's participant list is still growing, so counting it now would undercount it and
/// then have it vanish from a later query once merged into a different bucket boundary.
fn parse_finalized_raid(event: &ActivityEvent) -> Option<RaidCompletionPayload> {
    let payload: RaidCompletionPayload = serde_json::from_value(event.payload.clone()).ok()?;
    if payload.merge_open {
        return None;
    }
    Some(payload)
}

/// Buckets a Tombs of Amascut invocation level into the game's own difficulty tiers (Entry
/// 0-149, Normal 150-299, Expert 300+) - the plugin only reports the raw level (see
/// `RaidCompletionEvents` on the plugin side), so this is derived at query time rather than
/// stored.
fn toa_tier(level: i32) -> &'static str {
    if level >= 300 {
        "expert"
    } else if level >= 150 {
        "normal"
    } else {
        "entry"
    }
}

/// Whether a finalized raid completion matches the leaderboard/chart's `raid_type`/`difficulty`
/// filter. `None` or `"all"` for either matches everything. CoX's only split is Regular vs
/// Challenge Mode (`mode` unset vs set); ToA buckets via [`toa_tier`]; ToB has no difficulty
/// filter at all yet (its `mode` is always unset - Hard Mode detection isn't implemented, see
/// `RaidCompletionEvents`'s doc comment on the plugin side), so any `difficulty` filter for ToB
/// matches every ToB completion rather than filtering them all out.
fn raid_completion_matches(
    payload: &RaidCompletionPayload,
    raid_type: Option<&str>,
    difficulty: Option<&str>,
) -> bool {
    if let Some(raid_type) = raid_type.filter(|t| *t != "all") {
        if payload.raid_type.as_str() != raid_type {
            return false;
        }
    }
    let Some(difficulty) = difficulty.filter(|d| *d != "all") else {
        return true;
    };
    match (payload.raid_type, &payload.difficulty) {
        (RaidType::Cox, RaidDifficulty::Mode { mode }) => (difficulty == "regular") == mode.is_none(),
        (RaidType::Toa, RaidDifficulty::Level { level }) => toa_tier(*level) == difficulty,
        _ => true,
    }
}

/// Best (highest) `total_value` among this group's other finalized completions of the same
/// raid+difficulty, excluding `exclude_event_id` (the completion that was just finalized, which is
/// already in `activity_events` by the time this is called) - powers the Discord raid embed's
/// "New group record!" / "Best: X gp" field. `None` when this is the group's first finalized
/// completion of this raid+difficulty (matched via `RaidDifficulty::merge_key`, the same identity
/// `raid_merge` itself uses, so a level-300 and level-350 ToA run never count as the same record).
pub async fn group_raid_best_value(
    client: &Client,
    group_id: i64,
    raid_type: RaidType,
    difficulty: &RaidDifficulty,
    exclude_event_id: i64,
) -> Result<Option<i64>, ApiError> {
    let events = list_raid_events_since(client, group_id, None).await?;
    let merge_key = difficulty.merge_key();
    Ok(events
        .iter()
        .filter(|event| event.id != exclude_event_id)
        .filter_map(parse_finalized_raid)
        .filter(|payload| payload.raid_type == raid_type && payload.difficulty.merge_key() == merge_key)
        .map(|payload| payload.total_value)
        .max())
}

/// Per-member participation counts for raid completions matching `raid_type`/`difficulty` over
/// the window - "participation" means present in [`RaidCompletionPayload::participants`], not
/// just the reporting member, since a raid completion is a group event every party member should
/// get credit for.
pub async fn get_raid_completions_leaderboard(
    client: &Client,
    group_id: i64,
    window: crate::leaderboard::LeaderboardWindow,
    raid_type: Option<&str>,
    difficulty: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Vec<(String, i64)>, ApiError> {
    let events =
        list_raid_events_since(client, group_id, leaderboard_window_cutoff(window, now)).await?;

    let mut counts: HashMap<String, i64> = list_member_names(client, group_id)
        .await?
        .into_iter()
        .map(|name| (name, 0))
        .collect();
    for event in &events {
        let Some(payload) = parse_finalized_raid(event) else {
            continue;
        };
        if !raid_completion_matches(&payload, raid_type, difficulty) {
            continue;
        }
        for participant in &payload.participants {
            *counts.entry(participant.member_name.clone()).or_insert(0) += 1;
        }
    }

    Ok(counts.into_iter().collect())
}

/// Cumulative raid-completion time series matching `raid_type`/`difficulty`, bucketed at
/// `period`'s granularity - the chart counterpart to [`get_raid_completions_leaderboard`].
/// `group_by_member = false` collapses everything into one [`RAID_GROUP_TOTAL_LABEL`] line (one
/// completion, one increment, regardless of party size); `true` credits every participant
/// separately, matching the leaderboard's per-member counting.
pub async fn get_raid_completions_metric_data(
    client: &Client,
    group_id: i64,
    period: AggregatePeriod,
    raid_type: Option<&str>,
    difficulty: Option<&str>,
    group_by_member: bool,
) -> Result<GroupMetricData, ApiError> {
    let events = list_raid_events_since(client, group_id, None).await?;
    let granularity = bucket_granularity(period);

    let mut per_key: HashMap<String, BTreeMap<DateTime<Utc>, i64>> = HashMap::new();
    for event in &events {
        let Some(payload) = parse_finalized_raid(event) else {
            continue;
        };
        if !raid_completion_matches(&payload, raid_type, difficulty) {
            continue;
        }
        let bucket = truncate_datetime(event.occurred_at, granularity);
        if group_by_member {
            for participant in &payload.participants {
                *per_key
                    .entry(participant.member_name.clone())
                    .or_default()
                    .entry(bucket)
                    .or_insert(0) += 1;
            }
        } else {
            *per_key
                .entry(RAID_GROUP_TOTAL_LABEL.to_string())
                .or_default()
                .entry(bucket)
                .or_insert(0) += 1;
        }
    }

    Ok(cumulative_metric_data(per_key))
}

/// Hourly/daily/monthly GP-earned history for the chart, reading `bank_value_day/month/year`
/// (populated by `aggregate_bank_value`) - mirrors `get_skills_for_period` exactly, since bank
/// value is already a cumulative absolute total at each row (no delta/running-sum needed here,
/// unlike the boss-KC metric above).
pub async fn get_bank_value_for_period(
    client: &Client,
    group_id: i64,
    period: AggregatePeriod,
) -> Result<GroupMetricData, ApiError> {
    let s = format!(
        r#"
SELECT member_name, time, b.bank_value
FROM groupscape.bank_value_{} b
INNER JOIN groupscape.members m ON m.member_id=b.member_id
WHERE m.group_id=$1 AND m.member_name != $2
"#,
        match period {
            AggregatePeriod::Day => "day",
            AggregatePeriod::Month => "month",
            AggregatePeriod::Year => "year",
        }
    );
    let stmt = client.prepare_cached(&s).await?;
    let rows = client
        .query(&stmt, &[&group_id, &SHARED_MEMBER])
        .await
        .map_err(ApiError::GetLeaderboardSnapshotsError)?;

    let mut member_data: HashMap<String, MemberMetricData> = HashMap::new();
    for row in rows {
        let member_name: String = row.try_get("member_name")?;
        let point = MetricDataPoint {
            time: row.try_get("time")?,
            value: row.try_get("bank_value")?,
        };
        member_data
            .entry(member_name.clone())
            .or_insert_with(|| MemberMetricData {
                name: member_name,
                metric_data: vec![],
            })
            .metric_data
            .push(point);
    }

    Ok(member_data.into_values().collect())
}

/// Computes and upserts every member's current net worth (bank + inventory + equipment) into
/// `bank_value_day/month/year`, truncated to each table's granularity - the chart-history
/// counterpart to `capture_bank_value_snapshots` (which stays daily-only and keeps serving the
/// GP-earned leaderboard metric unchanged). Bank/inventory/equipment aren't a per-update-
/// timestamped column the way skills are, so unlike `aggregate_skills_for_period` this can't
/// `INSERT ... SELECT` straight from `members` - each member's value is computed app-side via
/// `net_worth_value()` first, then upserted with a prepared statement per member per table
/// (matching `capture_bank_value_snapshots`'s existing per-member loop style).
pub async fn aggregate_bank_value(
    client: &mut Client,
    ge_prices: &crate::models::GEPrices,
    now: DateTime<Utc>,
) -> Result<(), ApiError> {
    let members_stmt = client
        .prepare_cached(
            "SELECT member_id, COALESCE(bank, ARRAY[]::INTEGER[]) AS bank,
                    COALESCE(inventory, ARRAY[]::INTEGER[]) AS inventory,
                    COALESCE(equipment, ARRAY[]::INTEGER[]) AS equipment
             FROM groupscape.members
             WHERE bank IS NOT NULL OR inventory IS NOT NULL OR equipment IS NOT NULL",
        )
        .await?;
    let rows = client
        .query(&members_stmt, &[])
        .await
        .map_err(ApiError::CaptureLeaderboardSnapshotsError)?;
    let mut values: Vec<(i64, i64)> = Vec::with_capacity(rows.len());
    for row in &rows {
        let member_id: i64 = row.try_get("member_id")?;
        let bank: Vec<i32> = row.try_get("bank")?;
        let inventory: Vec<i32> = row.try_get("inventory")?;
        let equipment: Vec<i32> = row.try_get("equipment")?;
        values.push((member_id, net_worth_value(&bank, &inventory, &equipment, ge_prices)));
    }

    let transaction = client.transaction().await?;
    let update_last_aggregation_stmt = transaction
        .prepare_cached(
            r#"
UPDATE groupscape.aggregation_info SET last_aggregation=NOW() WHERE type='bank_value'"#,
        )
        .await?;
    transaction
        .execute(&update_last_aggregation_stmt, &[])
        .await?;

    for (table, granularity) in [("day", "hour"), ("month", "day"), ("year", "month")] {
        let bucket = truncate_datetime(now, granularity);
        let upsert_sql = format!(
            r#"
INSERT INTO groupscape.bank_value_{table} (member_id, time, bank_value)
VALUES ($1, $2, $3)
ON CONFLICT (member_id, time) DO UPDATE SET bank_value=excluded.bank_value
"#
        );
        let upsert_stmt = transaction.prepare_cached(&upsert_sql).await?;
        for (member_id, value) in &values {
            transaction
                .execute(&upsert_stmt, &[member_id, &bucket, value])
                .await?;
        }
    }

    transaction.commit().await?;
    Ok(())
}

async fn apply_bank_value_retention_for_period(
    transaction: &Transaction<'_>,
    period: AggregatePeriod,
    last_aggregation: &DateTime<Utc>,
) -> Result<(), ApiError> {
    let s = format!(
        r#"
DELETE FROM groupscape.bank_value_{0}
WHERE time < ($1::timestamptz - interval '{1}') AND (member_id, time) NOT IN (
  SELECT member_id, max(time) FROM groupscape.bank_value_{0} WHERE time < ($1::timestamptz - interval '{1}') GROUP BY member_id
)
"#,
        match period {
            AggregatePeriod::Day => "day",
            AggregatePeriod::Month => "month",
            AggregatePeriod::Year => "year",
        },
        match period {
            AggregatePeriod::Day => "1 day",
            AggregatePeriod::Month => "1 month",
            AggregatePeriod::Year => "1 year",
        }
    );
    let stmt = transaction.prepare_cached(&s).await?;
    transaction.execute(&stmt, &[last_aggregation]).await?;

    Ok(())
}

/// Mirrors `apply_skills_retention` exactly, against `bank_value_day/month/year` instead of
/// `skills_day/month/year` - same retention windows (day table thinned past 1 day, month table
/// past 1 month, year table past 1 year), same "keep only the last row per member past the
/// cutoff" delete.
pub async fn apply_bank_value_retention(client: &mut Client) -> Result<(), ApiError> {
    let last_aggregation = get_last_aggregation(client, "bank_value").await?;

    let transaction = client.transaction().await?;
    apply_bank_value_retention_for_period(&transaction, AggregatePeriod::Day, &last_aggregation)
        .await?;
    apply_bank_value_retention_for_period(&transaction, AggregatePeriod::Month, &last_aggregation)
        .await?;
    apply_bank_value_retention_for_period(&transaction, AggregatePeriod::Year, &last_aggregation)
        .await?;
    transaction.commit().await?;

    Ok(())
}

// --- Admin account management -------------------------------------------------------------

/// Picks the group's next admin from its remaining `group_permissions` rows (lowest
/// `account_id` first, matching the "first user" semantics `link_character_to_group` already
/// uses to assign the *original* admin), or clears `admin_account_id` back to the "unclaimed"
/// `NULL` state if nobody's left. Called wherever an account is force-removed from a group it
/// owns (ban cascade, hard delete) so the group is never left owned by a departed account.
pub async fn transfer_or_clear_group_ownership(
    client: &Client,
    group_id: i64,
    departing_account_id: i64,
) -> Result<(), ApiError> {
    let next_owner_stmt = client
        .prepare_cached(
            "SELECT account_id FROM groupscape.group_permissions WHERE group_id=$1 AND account_id != $2 ORDER BY account_id ASC LIMIT 1",
        )
        .await?;
    let next_owner: Option<i64> = client
        .query_opt(&next_owner_stmt, &[&group_id, &departing_account_id])
        .await
        .map_err(|e| ApiError::AdminDbError("TransferGroupOwnershipError".to_string(), e))?
        .map(|row| row.get(0));

    let update_stmt = client
        .prepare_cached("UPDATE groupscape.groups SET admin_account_id=$1 WHERE group_id=$2")
        .await?;
    client
        .execute(&update_stmt, &[&next_owner, &group_id])
        .await
        .map_err(|e| ApiError::AdminDbError("TransferGroupOwnershipError".to_string(), e))?;
    Ok(())
}

/// Groups this account currently owns (`groups.admin_account_id`) - the set that needs
/// `transfer_or_clear_group_ownership` run against it before the account is banned or
/// hard-deleted.
pub async fn admin_get_owned_group_ids(
    client: &Client,
    account_id: i64,
) -> Result<Vec<i64>, ApiError> {
    let stmt = client
        .prepare_cached("SELECT group_id FROM groupscape.groups WHERE admin_account_id=$1")
        .await?;
    let rows = client
        .query(&stmt, &[&account_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminGetOwnedGroupsError".to_string(), e))?;
    Ok(rows.into_iter().map(|row| row.get(0)).collect())
}

/// Removes every group membership this account holds (`group_permissions`) - the "ban cascades
/// to remove all group memberships" half of the ban action, run *after*
/// `transfer_or_clear_group_ownership` has already moved ownership of any owned groups off this
/// account.
pub async fn admin_remove_all_group_memberships(
    client: &Client,
    account_id: i64,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached("DELETE FROM groupscape.group_permissions WHERE account_id=$1")
        .await?;
    client
        .execute(&stmt, &[&account_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminRemoveGroupMembershipsError".to_string(), e))?;
    Ok(())
}

/// Grants an account membership in a group the same way `link_character_to_group` does for its
/// own default row - all permission flags off - but without requiring the group's join
/// credentials, since this is the admin support path for accounts that can't self-serve.
/// Idempotent: adding an account that's already a member is a no-op.
///
/// Also links every character the account owns that isn't already linked to a group
/// (`character_group_links`), so the account's character(s) actually appear in the group's
/// roster instead of just being permitted to view it - mirroring what `link_character_to_group`
/// does for the self-serve join flow. A character already linked to a *different* group is left
/// alone rather than moved, since this endpoint only takes a group_id and has no way to know that
/// reassignment (as opposed to just granting access) was intended.
pub async fn admin_add_account_to_group(
    client: &mut Client,
    account_id: i64,
    group_id: i64,
) -> Result<(), ApiError> {
    let transaction = client.transaction().await?;

    let permissions_stmt = transaction
        .prepare_cached(
            "INSERT INTO groupscape.group_permissions (group_id, account_id) VALUES ($1, $2) ON CONFLICT (group_id, account_id) DO NOTHING",
        )
        .await?;
    transaction
        .execute(&permissions_stmt, &[&group_id, &account_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminAddAccountToGroupError".to_string(), e))?;

    let link_stmt = transaction
        .prepare_cached(
            "INSERT INTO groupscape.character_group_links (character_id, group_id) \
             SELECT c.character_id, $2 FROM groupscape.characters c \
             WHERE c.account_id=$1 \
             AND NOT EXISTS ( \
                 SELECT 1 FROM groupscape.character_group_links l WHERE l.character_id = c.character_id \
             )",
        )
        .await?;
    transaction
        .execute(&link_stmt, &[&account_id, &group_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminLinkCharacterToGroupError".to_string(), e))?;

    let admin_stmt = transaction
        .prepare_cached(
            "UPDATE groupscape.groups SET admin_account_id=$1 WHERE group_id=$2 AND admin_account_id IS NULL",
        )
        .await?;
    transaction
        .execute(&admin_stmt, &[&account_id, &group_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminSetGroupOwnerError".to_string(), e))?;

    transaction
        .commit()
        .await
        .map_err(|e| ApiError::AdminDbError("AdminAddAccountToGroupCommitError".to_string(), e))?;
    Ok(())
}

/// Removes a single group membership, transferring ownership first if this account happens to be
/// that group's current owner (mirrors the ban cascade's per-group step, but scoped to one group
/// instead of every group the account owns). Returns `false` if the account wasn't a member.
pub async fn admin_remove_account_from_group(
    client: &Client,
    account_id: i64,
    group_id: i64,
) -> Result<bool, ApiError> {
    let owner_stmt = client
        .prepare_cached("SELECT admin_account_id FROM groupscape.groups WHERE group_id=$1")
        .await?;
    let owner_row = client
        .query_opt(&owner_stmt, &[&group_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminGetGroupOwnerError".to_string(), e))?;
    let is_owner = owner_row
        .and_then(|row| row.get::<_, Option<i64>>(0))
        .is_some_and(|owner_id| owner_id == account_id);
    if is_owner {
        transfer_or_clear_group_ownership(client, group_id, account_id).await?;
    }

    let delete_stmt = client
        .prepare_cached(
            "DELETE FROM groupscape.group_permissions WHERE group_id=$1 AND account_id=$2",
        )
        .await?;
    let affected = client
        .execute(&delete_stmt, &[&group_id, &account_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminRemoveAccountFromGroupError".to_string(), e))?;
    Ok(affected > 0)
}

fn admin_account_summary_from_row(row: &Row) -> Result<AdminAccountSummary, ApiError> {
    Ok(AdminAccountSummary {
        id: row.try_get("id")?,
        username: row.try_get("username")?,
        status: row.try_get("status")?,
        must_change_password: row.try_get("must_change_password")?,
        locked_out: row.try_get("locked_out")?,
        created_at: row.try_get("created_at")?,
        last_visit_at: row.try_get("last_visit_at")?,
        is_online: row.try_get("is_online")?,
    })
}

const ADMIN_ACCOUNT_SUMMARY_COLUMNS: &str = "a.id, a.username, a.status, a.must_change_password, a.created_at, a.last_visit_at, (a.locked_until IS NOT NULL AND a.locked_until > now()) AS locked_out";

/// Mirrors the 60s "online" threshold used for the live online badge in the group view
/// (`get_homepage_stats`), scoped to a single account via its characters' `account_hash`.
/// Uses `last_seen_at` (stamped on every accepted telemetry update, changed or not) rather than
/// the per-field `*_last_update` columns, which only advance when that field's value actually
/// changes - an idle-but-connected player would otherwise read as offline here after 60s despite
/// uploads continuing to arrive fine, same bug `get_homepage_stats` was fixed for.
fn admin_account_is_online_column() -> String {
    format!(
        r#"EXISTS (
    SELECT 1 FROM groupscape.characters c
    JOIN groupscape.members m ON m.account_hash = c.account_hash
    WHERE c.account_id = a.id AND m.member_name != '{SHARED_MEMBER}'
        AND m.last_seen_at >= NOW() - INTERVAL '60 seconds'
) AS is_online"#
    )
}

pub async fn admin_list_accounts(
    client: &Client,
    search: Option<&str>,
    status: Option<&str>,
    group_id: Option<i64>,
    page: i64,
    page_size: i64,
) -> Result<(Vec<AdminAccountSummary>, i64), ApiError> {
    let offset = (page.max(1) - 1) * page_size.max(1);
    let search_pattern = search.map(|s| format!("%{}%", s));
    let search_raw = search.map(|s| s.to_string());

    let is_online_column = admin_account_is_online_column();
    let list_stmt = client
        .prepare_cached(&format!(
            r#"
SELECT {ADMIN_ACCOUNT_SUMMARY_COLUMNS}, {is_online_column}
FROM groupscape.accounts a
WHERE ($1::text IS NULL OR a.username ILIKE $1 OR a.id::text = $2)
  AND ($3::text IS NULL OR a.status = $3)
  AND ($4::bigint IS NULL OR EXISTS (SELECT 1 FROM groupscape.group_permissions gp WHERE gp.account_id = a.id AND gp.group_id = $4))
ORDER BY a.id DESC
LIMIT $5 OFFSET $6
"#
        ))
        .await?;
    let rows = client
        .query(
            &list_stmt,
            &[
                &search_pattern,
                &search_raw,
                &status,
                &group_id,
                &page_size.max(1),
                &offset,
            ],
        )
        .await
        .map_err(|e| ApiError::AdminDbError("AdminListAccountsError".to_string(), e))?;
    let accounts = rows
        .iter()
        .map(admin_account_summary_from_row)
        .collect::<Result<Vec<_>, _>>()?;

    let count_stmt = client
        .prepare_cached(
            r#"
SELECT COUNT(*)
FROM groupscape.accounts a
WHERE ($1::text IS NULL OR a.username ILIKE $1 OR a.id::text = $2)
  AND ($3::text IS NULL OR a.status = $3)
  AND ($4::bigint IS NULL OR EXISTS (SELECT 1 FROM groupscape.group_permissions gp WHERE gp.account_id = a.id AND gp.group_id = $4))
"#,
        )
        .await?;
    let total: i64 = client
        .query_one(&count_stmt, &[&search_pattern, &search_raw, &status, &group_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminCountAccountsFilteredError".to_string(), e))?
        .try_get(0)?;

    Ok((accounts, total))
}

pub async fn admin_get_account(
    client: &Client,
    account_id: i64,
) -> Result<Option<AdminAccountDetail>, ApiError> {
    let is_online_column = admin_account_is_online_column();
    let account_stmt = client
        .prepare_cached(&format!(
            "SELECT {ADMIN_ACCOUNT_SUMMARY_COLUMNS}, {is_online_column} FROM groupscape.accounts a WHERE a.id = $1"
        ))
        .await?;
    let account_row = client
        .query_opt(&account_stmt, &[&account_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminGetAccountError".to_string(), e))?;
    let Some(account_row) = account_row else {
        return Ok(None);
    };
    let summary = admin_account_summary_from_row(&account_row)?;

    let groups_stmt = client
        .prepare_cached(
            r#"
SELECT g.group_id, g.group_name, (g.admin_account_id = $1) AS is_owner
FROM groupscape.group_permissions gp
JOIN groupscape.groups g ON g.group_id = gp.group_id
WHERE gp.account_id = $1
ORDER BY g.group_name
"#,
        )
        .await?;
    let group_rows = client
        .query(&groups_stmt, &[&account_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminGetAccountGroupsError".to_string(), e))?;
    let groups = group_rows
        .into_iter()
        .map(|row| {
            Ok(AdminAccountGroup {
                group_id: row.try_get("group_id")?,
                group_name: row.try_get("group_name")?,
                is_owner: row.try_get("is_owner")?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    let characters_stmt = client
        .prepare_cached(
            "SELECT c.character_id, c.display_rsn, c.status, c.bound_at, l.group_id, g.group_name \
             FROM groupscape.characters c \
             LEFT JOIN groupscape.character_group_links l ON l.character_id = c.character_id \
             LEFT JOIN groupscape.groups g ON g.group_id = l.group_id \
             WHERE c.account_id=$1 ORDER BY c.bound_at",
        )
        .await?;
    let character_rows = client
        .query(&characters_stmt, &[&account_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminGetAccountCharactersError".to_string(), e))?;
    let characters = character_rows
        .into_iter()
        .map(|row| {
            Ok(AdminAccountCharacter {
                id: row.try_get("character_id")?,
                display_rsn: row.try_get("display_rsn")?,
                status: row.try_get("status")?,
                bound_at: row.try_get("bound_at")?,
                group_id: row.try_get("group_id")?,
                group_name: row.try_get("group_name")?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;

    let session_count: i64 = client
        .query_one(
            "SELECT COUNT(*) FROM groupscape.account_sessions WHERE account_id=$1 AND expires_at > now()",
            &[&account_id],
        )
        .await
        .map_err(|e| ApiError::AdminDbError("AdminGetAccountSessionCountError".to_string(), e))?
        .try_get(0)?;

    Ok(Some(AdminAccountDetail {
        id: summary.id,
        username: summary.username,
        status: summary.status,
        must_change_password: summary.must_change_password,
        locked_out: summary.locked_out,
        created_at: summary.created_at,
        last_visit_at: summary.last_visit_at,
        groups,
        characters,
        session_count,
    }))
}

pub async fn admin_set_account_status(
    client: &Client,
    account_id: i64,
    status: &str,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached("UPDATE groupscape.accounts SET status=$1 WHERE id=$2")
        .await?;
    client
        .execute(&stmt, &[&status, &account_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminSetAccountStatusError".to_string(), e))?;
    Ok(())
}

/// Soft delete: status flips to `deleted` and the (unique, citext) username is scrubbed to a
/// placeholder derived from the account id so the real address is freed up for re-registration.
/// Group memberships are left untouched, unlike ban/hard-delete - this is meant to be
/// reversible by an admin flipping status back to `active` and setting a real username again.
pub async fn admin_soft_delete_account(client: &Client, account_id: i64) -> Result<(), ApiError> {
    let placeholder_username = format!("deleted-account-{}", account_id);
    let stmt = client
        .prepare_cached("UPDATE groupscape.accounts SET status='deleted', username=$1 WHERE id=$2")
        .await?;
    client
        .execute(&stmt, &[&placeholder_username, &account_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminSoftDeleteAccountError".to_string(), e))?;
    Ok(())
}

pub async fn admin_list_account_sessions(
    client: &Client,
    account_id: i64,
) -> Result<Vec<AdminAccountSession>, ApiError> {
    let stmt = client
        .prepare_cached(
            "SELECT session_id, created_at, expires_at, ip, user_agent FROM groupscape.account_sessions WHERE account_id=$1 ORDER BY created_at DESC",
        )
        .await?;
    let rows = client
        .query(&stmt, &[&account_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminListAccountSessionsError".to_string(), e))?;
    rows.into_iter()
        .map(|row| {
            Ok(AdminAccountSession {
                session_id: row.try_get("session_id")?,
                created_at: row.try_get("created_at")?,
                expires_at: row.try_get("expires_at")?,
                ip: row.try_get("ip")?,
                user_agent: row.try_get("user_agent")?,
            })
        })
        .collect()
}

/// Returns whether a matching session row existed to revoke (lets the caller 404 a
/// wrong/foreign session id instead of silently no-oping).
pub async fn admin_revoke_account_session(
    client: &Client,
    account_id: i64,
    session_id: i64,
) -> Result<bool, ApiError> {
    let stmt = client
        .prepare_cached("DELETE FROM groupscape.account_sessions WHERE session_id=$1 AND account_id=$2")
        .await?;
    let deleted = client
        .execute(&stmt, &[&session_id, &account_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminRevokeAccountSessionError".to_string(), e))?;
    Ok(deleted > 0)
}

pub async fn admin_revoke_all_account_sessions(
    client: &Client,
    account_id: i64,
) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached("DELETE FROM groupscape.account_sessions WHERE account_id=$1")
        .await?;
    client
        .execute(&stmt, &[&account_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminRevokeAllAccountSessionsError".to_string(), e))?;
    Ok(())
}

pub async fn admin_clear_account_lockout(client: &Client, account_id: i64) -> Result<(), ApiError> {
    let stmt = client
        .prepare_cached(
            "UPDATE groupscape.accounts SET failed_login_attempts=0, locked_until=NULL WHERE id=$1",
        )
        .await?;
    client
        .execute(&stmt, &[&account_id])
        .await
        .map_err(|e| ApiError::AdminDbError("AdminClearAccountLockoutError".to_string(), e))?;
    Ok(())
}

/// Small union search for the admin global search box - up to 10 accounts (by username/id
/// substring) and 10 groups (by name substring, delegating to `admin_list_groups`'s existing
/// search behavior).
pub async fn admin_search(
    client: &Client,
    q: &str,
) -> Result<(Vec<AdminAccountSummary>, Vec<AdminGroupSummary>), ApiError> {
    let (accounts, _) = admin_list_accounts(client, Some(q), None, None, 1, 10).await?;
    let (groups, _) = admin_list_groups(client, Some(q), 1, 10).await?;
    Ok((accounts, groups))
}

pub async fn admin_dashboard(client: &Client) -> Result<AdminDashboard, ApiError> {
    let status_rows = client
        .query(
            "SELECT status, COUNT(*) AS n FROM groupscape.accounts GROUP BY status",
            &[],
        )
        .await
        .map_err(|e| ApiError::AdminDbError("AdminDashboardStatusCountsError".to_string(), e))?;

    let mut active = 0i64;
    let mut suspended = 0i64;
    let mut banned = 0i64;
    let mut deleted = 0i64;
    let mut total = 0i64;
    for row in status_rows {
        let status: String = row.get("status");
        let n: i64 = row.get("n");
        total += n;
        match status.as_str() {
            "active" => active = n,
            "suspended" => suspended = n,
            "banned" => banned = n,
            "deleted" => deleted = n,
            _ => {}
        }
    }

    let live_sessions: i64 = client
        .query_one(
            "SELECT COUNT(*) FROM groupscape.account_sessions WHERE expires_at > now()",
            &[],
        )
        .await
        .map_err(|e| ApiError::AdminDbError("AdminDashboardLiveSessionsError".to_string(), e))?
        .try_get(0)?;

    let locked_out: i64 = client
        .query_one(
            "SELECT COUNT(*) FROM groupscape.accounts WHERE locked_until IS NOT NULL AND locked_until > now()",
            &[],
        )
        .await
        .map_err(|e| ApiError::AdminDbError("AdminDashboardLockedOutError".to_string(), e))?
        .try_get(0)?;

    let (recent_audit, _) = admin_list_audit_log(client, 1, 10).await?;

    Ok(AdminDashboard {
        total,
        active,
        suspended,
        banned,
        deleted,
        live_sessions,
        locked_out,
        recent_audit,
    })
}
