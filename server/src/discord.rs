use crate::config::DiscordConfig;
use crate::db;
use crate::drop_rates;
use crate::error::ApiError;
use crate::item_names;
use crate::item_wiki_icons;
use crate::models::{format_gp, GameEvent, GEPrices};
use crate::notable_npcs;
use crate::unauthed;
use deadpool_postgres::Pool;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::task;

const AUTHORIZE_URL: &str = "https://discord.com/oauth2/authorize";
const TOKEN_URL: &str = "https://discord.com/api/oauth2/token";
const USER_URL: &str = "https://discord.com/api/users/@me";

pub struct DiscordUser {
    pub id: String,
    pub name: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
}

#[derive(Deserialize)]
struct UserResponse {
    id: Option<String>,
    username: Option<String>,
    global_name: Option<String>,
}

/// `identify` scope only - just enough for a stable Discord user id, matching
/// `groupscape-old`'s `discordOAuthClient.ts` decision (ported here, not the raw code, since
/// this stack uses blocking `ureq` rather than `fetch`).
pub fn authorize_url(config: &DiscordConfig, state: &str) -> String {
    format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope=identify&state={}",
        AUTHORIZE_URL,
        urlencoding::encode(&config.client_id),
        urlencoding::encode(&config.redirect_uri),
        urlencoding::encode(state),
    )
}

pub async fn exchange_code(config: &DiscordConfig, code: &str) -> Result<DiscordUser, ApiError> {
    let client_id = config.client_id.clone();
    let client_secret = config.client_secret.clone();
    let redirect_uri = config.redirect_uri.clone();
    let code = code.to_owned();

    task::spawn_blocking(move || {
        let token_res = ureq::post(TOKEN_URL)
            .send_form([
                ("client_id", client_id.as_str()),
                ("client_secret", client_secret.as_str()),
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", redirect_uri.as_str()),
            ])
            .map_err(ApiError::UreqError)?
            .body_mut()
            .read_json::<TokenResponse>()
            .map_err(ApiError::UreqError)?;

        let Some(access_token) = token_res.access_token else {
            return Err(ApiError::DiscordOAuthError(
                "token exchange returned no access_token".to_string(),
            ));
        };

        let user_res = ureq::get(USER_URL)
            .header("Authorization", &format!("Bearer {}", access_token))
            .call()
            .map_err(ApiError::UreqError)?
            .body_mut()
            .read_json::<UserResponse>()
            .map_err(ApiError::UreqError)?;

        match (user_res.id, user_res.global_name.or(user_res.username)) {
            (Some(id), Some(name)) => Ok(DiscordUser { id, name }),
            _ => Err(ApiError::DiscordOAuthError(
                "user fetch returned incomplete data".to_string(),
            )),
        }
    })
    .await
    .unwrap()
}

const KILL_COLOR: u32 = 0x2ECC71;
const DEATH_COLOR: u32 = 0xE74C3C;
const LOOT_COLOR: u32 = 0xF1C40F;
const DROP_COLOR: u32 = 0x9B59B6;
const RAID_COLOR: u32 = 0xE67E22;
const TEST_COLOR: u32 = 0x5865F2;
const COMBAT_TASK_COLOR: u32 = 0xC084FC;
const COLLECTION_LOG_COLOR: u32 = 0x6C8CFF;
const QUEST_COLOR: u32 = 0xC9962B;
const DIARY_COLOR: u32 = 0x29C2B0;
const PET_COLOR: u32 = 0xFF7AC6;
const CLUE_COLOR: u32 = 0x3BA7D6;
const LEVEL_UP_COLOR: u32 = 0x52C41A;
const RARE_DROP_COLOR: u32 = 0xFF2EC1;

/// A drop is called out as "Rare drop" when the rarest curated item in it has odds at or below
/// this (1/500) - matches the rough cutoff OSRS players already use for "that's a rare one"
/// (most boss uniques clear this; common supply drops don't). Items with no curated `rate` at all
/// never trigger this - see `drop_rates::DropRateEntry::rate`'s doc comment on why that's treated
/// as "unknown" rather than "common".
const RARE_DROP_THRESHOLD: f64 = 1.0 / 500.0;

/// The lowest curated drop-rate probability among `lines`' items from `source_name`, or `None` if
/// none of them have a curated `rate` at all - used to decide whether a "Drops"/"Clue casket"
/// embed gets the rare-drop callout treatment (see `RARE_DROP_THRESHOLD`).
fn rarest_probability(lines: &[(i32, i64, i64, String)], source_name: &str) -> Option<f64> {
    lines
        .iter()
        .filter_map(|(item_id, _, _, _)| drop_rates::lookup(source_name, *item_id)?.rate.as_deref())
        .filter_map(drop_rates::probability)
        .fold(None, |min, p| Some(min.map_or(p, |m: f64| m.min(p))))
}

/// `{quantity}x {name} ({value}, {drop rate})` for a single item - factored out of `drop_lines` so
/// `dispatch_drop_message` can rebuild the same line at a different (cumulative) quantity when
/// editing a repeat single-item drop in place, without re-deriving the value/rate formatting.
fn format_drop_line(item_id: i32, quantity: i64, ge_prices: &GEPrices, source_name: &str) -> String {
    let rate_entry = drop_rates::lookup(source_name, item_id);
    let unit_value = ge_prices.get(&item_id).copied();
    let value = unit_value.unwrap_or(0) * quantity;
    let value_part = match unit_value {
        Some(_) => format!("{} gp", format_gp(value)),
        None => "untradeable".to_string(),
    };
    let detail = match rate_entry.and_then(|d| d.rate.clone()) {
        Some(rate) => format!("{}, {}", value_part, rate),
        None => value_part,
    };
    format!("{}x {} ({})", quantity, item_names::display(item_id), detail)
}

/// `{quantity}x {name} ({value}, {drop rate})` lines for the unified "Drops" notification -
/// shared by kill loot, chest/clue loot, since both format the same way once resolved to a
/// display line, a gp value, and (when curated) a drop rate.
///
/// The stack's *combined* value gates `min_value` and is what's shown (e.g. 33x a 100gp item is
/// "3,300 gp", not "100 gp") - a single expensive-enough item shouldn't get hidden just because
/// its unit price is small. Untradeable items (no GE price at all) count as worth 0gp for this
/// comparison - they still show "untradeable" instead of a gp amount, but only clear `min_value`
/// when it's 0 (or when the item bypasses the filter entirely, see below). A non-zero `min_value`
/// with an untradeable item excludes it, same as any other item that can't clear the bar - this is
/// what keeps a regular monster's junk untradeable drops (ensouled heads, seeds, etc.) from
/// spamming the webhook just because they have no GE price to compare.
///
/// When `unique_only` is set, `min_value` is ignored entirely for an item that counts as unique -
/// either `drop_rates::lookup` marks it `is_unique` for `source_name` *and* `source_name` is a
/// curated notable NPC (`notable_npcs::is_notable`), or the item id is in `UNIQUE_DROP_EXCEPTIONS`
/// (see that const's doc comment). Everything else is excluded outright under `unique_only`, even
/// if it's untradeable. Unique-only is otherwise a notable-boss-only mode - a curated-unique item
/// dropped by a non-notable NPC (e.g. a rare drop table item, since that table spans NPCs never
/// added to `notable_npcs`) doesn't count unless it's also in the exception list.
///
/// (item id, quantity, combined stack value, display line) for each item that survives the
/// `min_value` / `unique_only` filter - the item id and value ride along so callers can pick a
/// thumbnail from the same set of items the description actually names, instead of re-deriving
/// "the highest value item" from the full unfiltered loot list (which could surface an item never
/// mentioned in the text, e.g. a priced common drop like Bones when the notable item shown was
/// untradeable). The quantity rides along too, so a single-item drop can be re-formatted at a
/// different (cumulative) quantity later - see `dispatch_drop_message`.
fn drop_lines(
    items: &[crate::models::LootItem],
    ge_prices: &GEPrices,
    min_value: i64,
    unique_only: bool,
    source_name: &str,
) -> Vec<(i32, i64, i64, String)> {
    let source_is_notable = notable_npcs::is_notable(source_name);
    items
        .iter()
        .filter_map(|item| {
            let rate_entry = drop_rates::lookup(source_name, item.item_id);
            let counts_as_unique = UNIQUE_DROP_EXCEPTIONS.contains(&item.item_id)
                || (source_is_notable && rate_entry.map(|d| d.is_unique).unwrap_or(false));
            if unique_only && !counts_as_unique {
                return None;
            }
            let unit_value = ge_prices.get(&item.item_id).copied();
            let value = unit_value.unwrap_or(0) * item.quantity as i64;
            let bypasses_min_value = unique_only && counts_as_unique;
            if !bypasses_min_value && value < min_value {
                return None;
            }
            let line = format_drop_line(item.item_id, item.quantity as i64, ge_prices, source_name);
            Some((item.item_id, item.quantity as i64, value, line))
        })
        .collect()
}

/// Items that should still count as "unique" under `unique_only` even when dropped by a non-notable
/// NPC - the notable-boss restriction in `drop_lines` exists to keep unique-only from surfacing
/// every curated rare-drop-table entry regardless of which (often minor) NPC rolled it, but some
/// items are notable enough on their own (e.g. a key toward a boss's loot) that a group still wants
/// them flagged no matter which NPC dropped them.
static UNIQUE_DROP_EXCEPTIONS: LazyLock<std::collections::HashSet<i32>> = LazyLock::new(|| {
    [
        2366,  // Shield left half
        2577,  // Ranger boots
        11286, // Draconic visage
        24268, // Basilisk jaw
        6798,  // Earth warrior champion scroll
        6799,  // Ghoul champion scroll
        6800,  // Giant champion scroll
        6801,  // Goblin champion scroll
        6802,  // Hobgoblin champion scroll
        6803,  // Imp champion scroll
        6804,  // Jogre champion scroll
        6805,  // Lesser demon champion scroll
        6806,  // Skeleton champion scroll
        6807,  // Zombie champion scroll
        6808,  // Leon's champion scroll
        10330, // 3rd Age range top
        10332, // 3rd Age range legs
        10334, // 3rd Age range coif
        10336, // 3rd Age vambraces
        10338, // 3rd Age robe top
        10340, // 3rd Age robe
        10342, // 3rd Age mage hat
        10344, // 3rd Age amulet
        10346, // 3rd Age platelegs
        10348, // 3rd Age platebody
        10350, // 3rd Age full helmet
        10352, // 3rd Age kiteshield
        12422, // 3rd Age wand
        12424, // 3rd Age bow
        12426, // 3rd Age longsword
        12437, // 3rd Age cloak
        20011, // 3rd Age axe
        20014, // 3rd Age pickaxe
        23242, // 3rd Age plateskirt
        23336, // 3rd Age druidic robe top
        23339, // 3rd Age druidic robe bottoms
        23342, // 3rd Age druidic staff
        23345, // 3rd Age druidic cloak
        28226, // 3rd Age felling axe
    ]
    .into_iter()
    .collect()
});

/// Thumbnail for a "Drops" embed - the highest-value item among the ones actually named in
/// `lines`, so the icon always matches something the description text mentions.
fn drop_thumbnail(lines: &[(i32, i64, i64, String)]) -> Option<String> {
    lines.iter().max_by_key(|(_, _, value, _)| *value).and_then(|(item_id, _, _, _)| item_icon_url(*item_id))
}

/// Combined GE value of a whole kill's loot, regardless of the per-item `drops_min_value`
/// threshold `drop_lines` applies - the kill message reports what the boss actually dropped, not
/// just the slice a group cares to see itemized in the separate Drops notification.
fn total_loot_value(items: &[crate::models::LootItem], ge_prices: &GEPrices) -> i64 {
    items
        .iter()
        .map(|item| ge_prices.get(&item.item_id).copied().unwrap_or(0) * item.quantity as i64)
        .sum()
}

/// Splits a loot list into (pets, everything else) - pets get their own "Pet" embed instead of
/// riding the generic "Drops" line, so they're excluded from whatever the caller does with the
/// second half of this split.
fn split_pets(items: &[crate::models::LootItem]) -> (Vec<crate::models::LootItem>, Vec<crate::models::LootItem>) {
    items.iter().cloned().partition(|item| crate::pets::is_pet_item(item.item_id))
}

/// Posts one "Pet" embed per pet item found in a drop - in practice always at most one, but a
/// list keeps this correct if RuneLite ever correlates more than one pet drop into a single event.
///
/// `account_kc` is the reporting client's own in-game kill count (`KillEvent::account_kc`, parsed
/// from the "kill count is" chat line), *not* this server's tracked kill log - only meaningful for
/// a pet obtained from a `GameEvent::Kill`, so a skilling/minigame pet arriving via a plain
/// `GameEvent::Loot` just passes `None` and the field is omitted.
async fn send_pet_embeds(
    webhook_url: &str,
    member_name: &str,
    source_name: &str,
    pets: &[crate::models::LootItem],
    account_kc: Option<i64>,
) {
    for pet in pets {
        let description = format!("{} received a pet: {}", member_name, item_names::display(pet.item_id));
        let mut fields = vec![("Source".to_string(), source_name.to_string())];
        if let Some(rate) = drop_rates::lookup(source_name, pet.item_id).and_then(|entry| entry.rate.clone()) {
            fields.push(("Drop rate".to_string(), rate));
        }
        if let Some(kc) = account_kc {
            fields.push(("Obtained on kill".to_string(), format!("#{}", kc)));
        }
        send_webhook_embed_rich(
            webhook_url.to_string(),
            "Pet",
            description,
            PET_COLOR,
            item_icon_url(pet.item_id),
            fields,
            Some(member_name.to_string()),
        )
        .await;
    }
}

fn build_embed_json(
    title: &str,
    description: &str,
    color: u32,
    thumbnail_url: Option<&str>,
    fields: &[(&str, &str)],
    footer: Option<&str>,
) -> serde_json::Value {
    let mut embed = serde_json::json!({ "title": title, "description": description, "color": color });
    if let Some(thumbnail_url) = thumbnail_url {
        embed["thumbnail"] = serde_json::json!({ "url": thumbnail_url });
    }
    if !fields.is_empty() {
        embed["fields"] = serde_json::json!(fields
            .iter()
            .map(|(name, value)| serde_json::json!({ "name": name, "value": value, "inline": true }))
            .collect::<Vec<_>>());
    }
    if let Some(footer) = footer {
        embed["footer"] = serde_json::json!({ "text": footer });
    }
    embed
}

fn send_webhook_embed_sync(
    url: &str,
    title: &str,
    description: &str,
    color: u32,
    thumbnail_url: Option<&str>,
    fields: &[(&str, &str)],
    footer: Option<&str>,
) -> Result<(), ureq::Error> {
    let embed = build_embed_json(title, description, color, thumbnail_url, fields, footer);
    ureq::post(url).send_json(serde_json::json!({ "embeds": [embed] }))?;
    Ok(())
}

/// Same as `send_webhook_embed_sync` but requests Discord return the created message (`?wait=true`,
/// otherwise a webhook post gets a bare 204) and hands back its id, so a later kill on the same
/// boss/member can edit this message in place instead of posting a new one (see `KILL_MESSAGE_CACHE`).
fn send_webhook_embed_get_id_sync(
    url: &str,
    title: &str,
    description: &str,
    color: u32,
    thumbnail_url: Option<&str>,
    fields: &[(&str, &str)],
    footer: Option<&str>,
) -> Result<String, ureq::Error> {
    let embed = build_embed_json(title, description, color, thumbnail_url, fields, footer);
    let separator = if url.contains('?') { "&" } else { "?" };
    let wait_url = format!("{}{}wait=true", url, separator);
    let mut response = ureq::post(&wait_url).send_json(serde_json::json!({ "embeds": [embed] }))?;
    let body: serde_json::Value = response.body_mut().read_json()?;
    Ok(body["id"].as_str().unwrap_or_default().to_string())
}

/// PATCHes a previously-sent webhook message in place (Discord's `PATCH .../messages/{id}`
/// endpoint) - used to bump a "Kill" embed's count instead of posting a fresh message per kill.
fn edit_webhook_embed_sync(
    url: &str,
    message_id: &str,
    title: &str,
    description: &str,
    color: u32,
    thumbnail_url: Option<&str>,
    fields: &[(&str, &str)],
    footer: Option<&str>,
) -> Result<(), ureq::Error> {
    let embed = build_embed_json(title, description, color, thumbnail_url, fields, footer);
    let edit_url = format!("{}/messages/{}", url.trim_end_matches('/'), message_id);
    ureq::patch(&edit_url).send_json(serde_json::json!({ "embeds": [embed] }))?;
    Ok(())
}

/// Awaited (not fire-and-forget) so a bad URL fails the settings save immediately, matching
/// `groupscape-old`'s `discordSettings.ts` validate-before-save behavior.
pub async fn test_webhook(url: &str) -> Result<(), ApiError> {
    let url = url.to_owned();
    task::spawn_blocking(move || {
        send_webhook_embed_sync(
            &url,
            "GroupScape",
            "This channel is now connected to a GroupScape group.",
            TEST_COLOR,
            None,
            &[],
            None,
        )
        .map_err(|err| ApiError::DiscordWebhookInvalidError(err.to_string()))
    })
    .await
    .unwrap_or_else(|err| Err(ApiError::DiscordWebhookInvalidError(err.to_string())))
}

/// Fixed example content for the "Send test message" button next to each notification toggle
/// in group settings - lets an admin see exactly what a real embed for that category looks like
/// (icons included) without waiting for it to actually happen in-game. Built from real OSRS
/// names/ids and, for the "Raid" case, the same [`crate::models::RaidCompletionPayload`] the real
/// relay uses, so the example can't drift out of sync with the real format. `kind` is one of this
/// struct's own `notify_*` field names - the same string the settings page's checkbox
/// `data-key` already carries, so the client has nothing new to know about.
pub async fn send_test_notification(webhook_url: String, web_origin: String, kind: &str) -> Result<(), ApiError> {
    const MEMBER: &str = "TestPlayer";
    const TANZANITE_FANG: i32 = 12922;
    const PET_SNAKELING: i32 = 12921;
    const DRAGON_BOOTS: i32 = 11840;
    const SERPENTINE_VISAGE: i32 = 12927;

    let (title, description, color, thumbnail, fields): (&'static str, String, u32, Option<String>, Vec<(String, String)>) =
        match kind {
            "notify_kills" => (
                "Kill",
                format!("{} killed [Zulrah]({})", MEMBER, wiki_url("Zulrah")),
                KILL_COLOR,
                Some(boss_icon_url(&web_origin, "Zulrah")),
                vec![
                    ("Kill count".to_string(), "42".to_string()),
                    ("Loot value".to_string(), format!("{} gp", format_gp(1_200_000))),
                ],
            ),
            "notify_deaths" => (
                "Death",
                format!("{} died to [Vorkath]({})", MEMBER, wiki_url("Vorkath")),
                DEATH_COLOR,
                Some(boss_icon_url(&web_origin, "Vorkath")),
                vec![("Deaths".to_string(), "5".to_string())],
            ),
            "notify_drops" => (
                "Drops",
                format!(
                    "{} received 1x {} from [Zulrah]({})",
                    MEMBER,
                    item_names::display(TANZANITE_FANG),
                    wiki_url("Zulrah")
                ),
                LOOT_COLOR,
                item_icon_url(TANZANITE_FANG),
                Vec::new(),
            ),
            "notify_pets" => (
                "Pet",
                format!("{} received a pet: {}", MEMBER, item_names::display(PET_SNAKELING)),
                PET_COLOR,
                item_icon_url(PET_SNAKELING),
                vec![
                    ("Source".to_string(), "Zulrah".to_string()),
                    ("Drop rate".to_string(), "1/4000".to_string()),
                    ("Obtained on kill".to_string(), "#4,812".to_string()),
                ],
            ),
            "notify_clues" => (
                "Clue casket",
                format!("{} opened a hard casket: 1x {}", MEMBER, item_names::display(DRAGON_BOOTS)),
                CLUE_COLOR,
                Some(clue_casket_icon_url("hard")),
                vec![("Tier".to_string(), "hard".to_string())],
            ),
            "notify_raids" => {
                let payload = crate::models::RaidCompletionPayload {
                    raid_type: crate::models::RaidType::Tob,
                    difficulty: crate::models::RaidDifficulty::Mode { mode: Some("Hard Mode".to_string()) },
                    participants: vec![crate::models::RaidParticipant {
                        member_name: MEMBER.to_string(),
                        value: 3_500_000,
                        loot: Vec::new(),
                    }],
                    total_value: 3_500_000,
                    merge_open: false,
                };
                (
                    "Raid",
                    payload.to_message(),
                    RAID_COLOR,
                    Some(raid_icon_url(crate::models::RaidType::Tob)),
                    Vec::new(),
                )
            }
            "notify_combat_achievements" => (
                "Combat achievements",
                format!(
                    "{} completed the combat task [{}]({})",
                    MEMBER,
                    "Zulrah Adept",
                    wiki_url("Zulrah Adept")
                ),
                COMBAT_TASK_COLOR,
                Some(combat_achievement_icon_url()),
                vec![("Completed by".to_string(), "38.9% of players".to_string())],
            ),
            "notify_collection_log" => (
                "Collection log",
                format!(
                    "{} added [{}]({}) to their collection log",
                    MEMBER,
                    item_names::name(SERPENTINE_VISAGE).unwrap_or("an item"),
                    item_names::wiki_link(SERPENTINE_VISAGE)
                ),
                COLLECTION_LOG_COLOR,
                item_icon_url(SERPENTINE_VISAGE),
                vec![
                    ("Page progress".to_string(), "14/34 (Vorkath)".to_string()),
                    ("Drop rate".to_string(), "1/512".to_string()),
                ],
            ),
            "notify_quests" => (
                "Quest",
                format!("{} completed [Dragon Slayer II]({})", MEMBER, wiki_url("Dragon Slayer II")),
                QUEST_COLOR,
                quest_icon_url(&web_origin, Some("Grandmaster")),
                vec![
                    ("Difficulty".to_string(), "Grandmaster".to_string()),
                    ("Quests complete".to_string(), "148 of 172".to_string()),
                ],
            ),
            "notify_diaries" => (
                "Diary",
                format!("{} completed the [Ardougne]({}) Elite diary", MEMBER, wiki_url("Ardougne Diary")),
                DIARY_COLOR,
                Some(diary_icon_url()),
                Vec::new(),
            ),
            "notify_level_ups" => (
                "Level up",
                format!("{} reached level 99 [Attack]({})", MEMBER, wiki_url("Attack")),
                LEVEL_UP_COLOR,
                Some(skill_icon_url("Attack")),
                Vec::new(),
            ),
            _ => return Err(ApiError::DiscordWebhookInvalidError(format!("unknown notification kind: {kind}"))),
        };

    task::spawn_blocking(move || {
        let fields: Vec<(&str, &str)> = fields.iter().map(|(name, value)| (name.as_str(), value.as_str())).collect();
        send_webhook_embed_sync(&webhook_url, title, &description, color, thumbnail.as_deref(), &fields, Some(MEMBER))
            .map_err(|err| ApiError::DiscordWebhookInvalidError(err.to_string()))
    })
    .await
    .unwrap_or_else(|err| Err(ApiError::DiscordWebhookInvalidError(err.to_string())))
}

async fn send_webhook_embed_with_thumbnail(
    url: String,
    title: &'static str,
    description: String,
    color: u32,
    thumbnail_url: Option<String>,
) {
    send_webhook_embed_rich(url, title, description, color, thumbnail_url, Vec::new(), None).await;
}

/// Full-featured embed send - fields and a footer (the member's name) on top of the thumbnail
/// every embed already had. The simpler wrappers above cover the embeds that don't need either.
async fn send_webhook_embed_rich(
    url: String,
    title: &'static str,
    description: String,
    color: u32,
    thumbnail_url: Option<String>,
    fields: Vec<(String, String)>,
    footer: Option<String>,
) {
    let _ = task::spawn_blocking(move || {
        let fields: Vec<(&str, &str)> = fields.iter().map(|(name, value)| (name.as_str(), value.as_str())).collect();
        if let Err(err) = send_webhook_embed_sync(
            &url,
            title,
            &description,
            color,
            thumbnail_url.as_deref(),
            &fields,
            footer.as_deref(),
        ) {
            log::warn!("discord: webhook send failed: {}", err);
        }
    })
    .await;
}

/// Async wrapper around `send_webhook_embed_get_id_sync` - see that function's docs.
async fn send_webhook_embed_rich_get_id(
    url: String,
    title: &'static str,
    description: String,
    color: u32,
    thumbnail_url: Option<String>,
    fields: Vec<(String, String)>,
    footer: Option<String>,
) -> Option<String> {
    task::spawn_blocking(move || {
        let fields: Vec<(&str, &str)> = fields.iter().map(|(name, value)| (name.as_str(), value.as_str())).collect();
        send_webhook_embed_get_id_sync(&url, title, &description, color, thumbnail_url.as_deref(), &fields, footer.as_deref())
            .map_err(|err| log::warn!("discord: webhook send failed: {}", err))
            .ok()
    })
    .await
    .unwrap_or(None)
}

/// Async wrapper around `edit_webhook_embed_sync`. Returns whether the edit succeeded - a `false`
/// (e.g. the message was deleted, or is older than 14 days and Discord rejects the edit) tells the
/// caller to fall back to posting a fresh message instead.
async fn edit_webhook_embed_rich(
    url: String,
    message_id: String,
    title: &'static str,
    description: String,
    color: u32,
    thumbnail_url: Option<String>,
    fields: Vec<(String, String)>,
    footer: Option<String>,
) -> bool {
    task::spawn_blocking(move || {
        let fields: Vec<(&str, &str)> = fields.iter().map(|(name, value)| (name.as_str(), value.as_str())).collect();
        match edit_webhook_embed_sync(&url, &message_id, title, &description, color, thumbnail_url.as_deref(), &fields, footer.as_deref()) {
            Ok(()) => true,
            Err(err) => {
                log::warn!("discord: webhook edit failed: {}", err);
                false
            }
        }
    })
    .await
    .unwrap_or(false)
}

/// Time a "Kill"/"Death"/"Drops" embed can still be edited in place for a repeat kill, death, or
/// single-item drop on the same boss+member (or item+source+member), past which a new event posts
/// a fresh message instead - keeps an event after a long gap (next session, days later) from
/// silently editing a message that's since scrolled far up the channel.
const WEBHOOK_MESSAGE_EDIT_TTL: Duration = Duration::from_secs(60 * 60);

/// What a tracked "Kill"/"Death"/"Drops" message was about, so a later event can tell whether it's
/// a repeat of *this* message specifically rather than just "some message for this member" - a
/// kill on Zulrah and a death to Zulrah (or a death with no known killer at all) are different
/// things and must never edit each other's message.
#[derive(Clone, PartialEq, Eq)]
enum WebhookEventKind {
    Kill(String),
    /// `None` covers a death with no identified killer (`DeathEvent::killer_name` is best-effort)
    /// - those still get grouped/edited together as long as nothing else interrupts them.
    Death(Option<String>),
    /// A drop event that named exactly one item (item id, source name) - a drop event naming more
    /// than one item never gets tracked here at all (see `dispatch_drop_message`), so it can never
    /// match or be matched by this variant.
    Drop(i32, String),
}

/// Only one entry is kept per (group, member): the *last* Kill/Death/Drops message posted for
/// them, regardless of what it was about. A new event only edits that message when it matches the
/// same `WebhookEventKind` and is still within `WEBHOOK_MESSAGE_EDIT_TTL` - any other message
/// posted for this member in between (a different boss, a death with no killer, a different item,
/// etc.) already overwrote this entry, which is exactly the "no other message has been posted
/// since" check: there's nothing else to consult.
type MemberKey = (i64, String);

struct LastMemberMessage {
    message_id: String,
    kind: WebhookEventKind,
    touched: Instant,
    /// Cumulative quantity named by the tracked message so far - only meaningful when `kind` is
    /// `Drop`, ignored (and passed as `0`) for `Kill`/`Death`, whose counts come from
    /// `db::count_kills_for_member_npc`/`count_deaths_for_member_npc` instead.
    quantity: i64,
}

/// In-memory only (see `dispatch_event_webhook`'s doc comment on loading settings fresh each
/// call) - resets on server restart, which just means the next event after a deploy posts a new
/// message rather than editing one from before the restart.
static LAST_MEMBER_MESSAGE: LazyLock<Mutex<HashMap<MemberKey, LastMemberMessage>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cached_message_for(member_key: &MemberKey, kind: &WebhookEventKind) -> Option<(String, i64)> {
    let cache = LAST_MEMBER_MESSAGE.lock().unwrap();
    cache
        .get(member_key)
        .filter(|entry| entry.kind == *kind && entry.touched.elapsed() < WEBHOOK_MESSAGE_EDIT_TTL)
        .map(|entry| (entry.message_id.clone(), entry.quantity))
}

fn store_last_message(member_key: MemberKey, kind: WebhookEventKind, message_id: String, quantity: i64) {
    LAST_MEMBER_MESSAGE
        .lock()
        .unwrap()
        .insert(member_key, LastMemberMessage { message_id, kind, touched: Instant::now(), quantity });
}

/// Absolute URL to a boss's self-hosted RuneLite-hiscore-style icon (see `site/src/data/
/// boss-icons.js` / `site/public/icons/hiscore/bosses/`), for the "Kill" embed's thumbnail.
/// Not every boss has a downloaded icon in that set - a miss just means Discord's embed proxy
/// fails to fetch the thumbnail and silently omits it, so this doesn't need to check existence.
fn boss_icon_url(web_origin: &str, npc_name: &str) -> String {
    format!(
        "{}/icons/hiscore/bosses/{}.png",
        web_origin.trim_end_matches('/'),
        drop_rates::slugify_npc_name(npc_name)
    )
}

/// Direct link to the item's own wiki-hosted image, for the "Drops"/"Drop"/"Pet"/"Collection log"
/// embed thumbnails - not the self-hosted `site/public/icons/items/<id>.webp` set the in-app UI
/// uses, since a chunk of that set (everything not GE-tradeable, inherited from the original
/// `group-ironmen` cache dump - see `fetch-items.mjs`'s doc comment) was found to hold the wrong
/// sprite for some items. Going straight to the wiki sidesteps needing to re-download and
/// re-verify that whole set just to fix Discord thumbnails.
///
/// Prefers `item_wiki_icons`' verified id -> filename map (built by
/// `fetch-item-wiki-icons.mjs` from the wiki's own `File:` pages, so it's already resolved any
/// redirect quirks - e.g. "Coins.png" actually being "Coins 100.png") and falls back to guessing
/// `"<name>.png"` for anything not yet in that map (a brand-new item, or one added by hand to
/// `item_names.json` - see the `update-osrs-data` skill's notes - since the last time
/// `fetch-item-wiki-icons.mjs` was regenerated). Same silent-miss tolerance as `boss_icon_url`
/// either way: an item with no known name, or whose name doesn't match the wiki's file naming,
/// just drops the thumbnail.
fn item_icon_url(item_id: i32) -> Option<String> {
    if let Some(filename) = item_wiki_icons::filename(item_id) {
        return Some(wiki_icon_url(filename));
    }
    item_names::name(item_id).map(|name| wiki_icon_url(&format!("{}.png", name)))
}

/// Wiki article URL for a page title - an NPC's death killer, a combat-achievement task, or a
/// boss's combat-achievement group. Spaces become underscores (the wiki's page-title convention)
/// before percent-encoding the rest, so e.g. "Kree'arra" resolves to the wiki's real page.
/// Mirrors `wikiTitle`/`taskWikiUrl`/`bossWikiUrl` in `site/src/data/combat-achievement.js`.
fn wiki_url(page_title: &str) -> String {
    format!(
        "https://oldschool.runescape.wiki/w/{}",
        urlencoding::encode(&page_title.trim().replace(' ', "_"))
    )
}

/// Absolute URL to the quest-point difficulty icon matching a quest's difficulty, for the
/// "Quest" embed's thumbnail - mirrors `Quest.icon` in `site/src/data/quest.js`. Quests don't
/// have individual icons in this app, only one per difficulty tier.
fn quest_icon_url(web_origin: &str, difficulty: Option<&str>) -> Option<String> {
    let file = match difficulty {
        Some("Novice") => "3399-0.png",
        Some("Intermediate") => "3400-0.png",
        Some("Experienced") => "3402-0.png",
        Some("Master") => "3403-0.png",
        Some("Grandmaster") | Some("Special") => "3404-0.png",
        _ => return None,
    };
    Some(format!("{}/icons/{}", web_origin.trim_end_matches('/'), file))
}

/// Direct link to a wiki-hosted file, for the embeds that have no self-hosted icon set of their
/// own (diaries, combat achievements, collection log, raids, pets, clue caskets) - cheaper than
/// downloading and maintaining another icon set for a handful of fixed, slow-changing images. Same
/// silent-miss tolerance as `boss_icon_url`: a renamed/missing wiki file just drops the thumbnail.
fn wiki_icon_url(file_name: &str) -> String {
    format!(
        "https://oldschool.runescape.wiki/w/Special:FilePath/{}",
        urlencoding::encode(file_name)
    )
}

/// Thumbnail for the "Diary" embed - one generic achievement-diary icon rather than one per
/// region, since the wiki has no reliably-named per-region icon file to key off of.
fn diary_icon_url() -> String {
    wiki_icon_url("Achievement Diaries.png")
}

/// Thumbnail for the "Combat achievements" embed - one generic icon rather than per-tier, since
/// task payloads don't carry a tier (only a task id) to key a per-tier icon off of.
fn combat_achievement_icon_url() -> String {
    wiki_icon_url("Combat Achievements icon.png")
}

/// Thumbnail for the "Collection log" embed - the completed page's own boss icon when the page is
/// a notable boss (reusing `boss_icon_url`'s self-hosted set), otherwise a generic log icon for
/// skilling/minigame pages that don't have one.
fn collection_log_icon_url(web_origin: &str, page: &str) -> String {
    if notable_npcs::is_notable(page) {
        boss_icon_url(web_origin, page)
    } else {
        wiki_icon_url("Collection log.png")
    }
}

/// Thumbnail for the "Level up" embed, keyed off the skill's own wiki icon file (e.g.
/// "Agility icon.png").
fn skill_icon_url(skill: &str) -> String {
    wiki_icon_url(&format!("{} icon.png", skill))
}

/// Thumbnail for the "Raid" embed, keyed off the raid's own wiki icon file.
fn raid_icon_url(raid_type: crate::models::RaidType) -> String {
    wiki_icon_url(&format!("{} icon.png", raid_type.display_name()))
}

/// Thumbnail for the "Clue casket" embed - reward caskets are real (untradeable) items named
/// "Reward casket (<tier>)" in-game, matching the wiki's file naming for their icon.
fn clue_casket_icon_url(tier: &str) -> String {
    wiki_icon_url(&format!("Reward casket ({}).png", tier.to_lowercase()))
}

/// Posts (or edits) a "Drops" embed for a resolved, non-empty `lines` list - shared by the Kill
/// arm's kill-loot and the standalone Loot arm below. Mirrors the Kill/Death "edit in place" flow
/// (see `WEBHOOK_MESSAGE_EDIT_TTL`'s doc comment) for the one case where a repeat drop has a
/// well-defined identity to check: `lines` naming exactly one item. A repeat of that same item
/// from the same source, for the same member, within the window edits the last drop message with
/// a bumped cumulative quantity (and rebuilt gp value) instead of posting a new one.
///
/// A drop event naming more than one item never participates - it always posts a fresh message,
/// same as before this existed - and, since it never calls `store_last_message`, leaves the cache
/// untouched. That means a later single-item drop can still land on (and edit) an older cached
/// message even though a multi-item drop was posted in between; that's an existing gap already
/// true of Kill/Death around any event that doesn't go through this cache (e.g. a Loot message
/// today doesn't interrupt a Kill/Death chain either), not a new inconsistency.
async fn dispatch_drop_message(
    webhook_url: String,
    group_id: i64,
    member_name: String,
    source_name: String,
    lines: Vec<(i32, i64, i64, String)>,
    ge_prices: &GEPrices,
) {
    let thumbnail = drop_thumbnail(&lines);
    let is_rare = rarest_probability(&lines, &source_name).is_some_and(|p| p <= RARE_DROP_THRESHOLD);
    let (title, color) = if is_rare { ("\u{1F48E} Rare drop", RARE_DROP_COLOR) } else { ("Drops", LOOT_COLOR) };
    let [(item_id, quantity, _, _)] = lines.as_slice() else {
        let description = format!(
            "{} received {} from [{}]({})",
            member_name,
            lines.iter().map(|(_, _, _, line)| line.as_str()).collect::<Vec<_>>().join(", "),
            source_name,
            wiki_url(&source_name)
        );
        send_webhook_embed_rich(webhook_url, title, description, color, thumbnail, Vec::new(), Some(member_name))
            .await;
        return;
    };
    let (item_id, quantity) = (*item_id, *quantity);
    let member_key: MemberKey = (group_id, member_name.clone());
    let kind = WebhookEventKind::Drop(item_id, source_name.clone());
    let cached = cached_message_for(&member_key, &kind);
    let total_quantity = quantity + cached.as_ref().map(|(_, prev)| *prev).unwrap_or(0);
    let line = format_drop_line(item_id, total_quantity, ge_prices, &source_name);
    let description = format!("{} received {} from [{}]({})", member_name, line, source_name, wiki_url(&source_name));
    let edited_id = match cached {
        Some((message_id, _)) => {
            let edited = edit_webhook_embed_rich(
                webhook_url.clone(),
                message_id.clone(),
                title,
                description.clone(),
                color,
                thumbnail.clone(),
                Vec::new(),
                Some(member_name.clone()),
            )
            .await;
            edited.then_some(message_id)
        }
        None => None,
    };
    let message_id = match edited_id {
        Some(message_id) => Some(message_id),
        None => {
            send_webhook_embed_rich_get_id(
                webhook_url,
                title,
                description,
                color,
                thumbnail,
                Vec::new(),
                Some(member_name),
            )
            .await
        }
    };
    if let Some(message_id) = message_id {
        store_last_message(member_key, kind, message_id, total_quantity);
    }
}

/// Fire-and-forget, mirroring `push::dispatch_alert_push` - a dead or misconfigured webhook must
/// never fail the telemetry upload that triggered it. Loads this group's webhook settings fresh
/// on every call rather than threading them through from the caller, since `update_group_member`
/// may fire this for several events per upload and settings rarely change.
pub fn dispatch_event_webhook(
    db_pool: Pool,
    group_id: i64,
    member_name: String,
    event: GameEvent,
    web_origin: String,
) {
    tokio::spawn(async move {
        let client = match db_pool.get().await {
            Ok(client) => client,
            Err(err) => {
                log::warn!("discord: failed to get db client: {}", err);
                return;
            }
        };
        let settings = match db::get_discord_webhook_settings(&client, group_id).await {
            Ok(settings) => settings,
            Err(err) => {
                log::warn!("discord: failed to load webhook settings: {}", err);
                return;
            }
        };
        let Some(webhook_url) = settings.webhook_url else {
            return;
        };

        match event {
            GameEvent::Kill(kill) => {
                // Regular NPCs (guards, random monsters) aren't posted - only bosses, matching
                // what a group actually wants pinged in Discord. Uses the same curated boss list
                // as the activity feed (`notable_npcs`).
                if settings.notify_kills && notable_npcs::is_notable(&kill.npc_name) {
                    let description = format!(
                        "{} killed [{}]({}){}",
                        member_name,
                        kill.npc_name,
                        wiki_url(&kill.npc_name),
                        kill.sub_kills_suffix()
                    );
                    // A combo kill ("Barrows"/"Moons of Peril") has no KC of its own to report -
                    // only its individual sub-bosses do, and those are tracked silently (excluded
                    // from `notable_npcs` on purpose - see `KillEvent::sub_kills`).
                    let mut fields = Vec::new();
                    if kill.sub_kills.is_none() {
                        // Prefer the account's real in-game KC (parsed client-side from the "kill
                        // count is" chat line); only fall back to counting this server's own kill
                        // log when that line didn't arrive in time, e.g. right at plugin startup.
                        let kc = match kill.account_kc {
                            Some(kc) => kc as i64,
                            None => db::count_kills_for_member_npc(&client, group_id, &member_name, &kill.npc_name)
                                .await
                                .unwrap_or_else(|err| {
                                    log::warn!("discord: failed to load kill count: {}", err);
                                    0
                                }),
                        };
                        fields.push(("Kill count".to_string(), kc.to_string()));
                    }
                    if let Some(loot) = &kill.loot {
                        let value = total_loot_value(loot, &unauthed::get_ge_prices_map());
                        if value > 0 {
                            fields.push(("Loot value".to_string(), format!("{} gp", format_gp(value))));
                        }
                    }
                    let thumbnail = boss_icon_url(&web_origin, &kill.npc_name);
                    let member_key: MemberKey = (group_id, member_name.clone());
                    let kind = WebhookEventKind::Kill(kill.npc_name.clone());
                    let edited_id = match cached_message_for(&member_key, &kind) {
                        Some((message_id, _)) => {
                            let edited = edit_webhook_embed_rich(
                                webhook_url.clone(),
                                message_id.clone(),
                                "Kill",
                                description.clone(),
                                KILL_COLOR,
                                Some(thumbnail.clone()),
                                fields.clone(),
                                Some(member_name.clone()),
                            )
                            .await;
                            edited.then_some(message_id)
                        }
                        None => None,
                    };
                    let message_id = match edited_id {
                        Some(message_id) => Some(message_id),
                        None => {
                            send_webhook_embed_rich_get_id(
                                webhook_url.clone(),
                                "Kill",
                                description,
                                KILL_COLOR,
                                Some(thumbnail),
                                fields,
                                Some(member_name.clone()),
                            )
                            .await
                        }
                    };
                    if let Some(message_id) = message_id {
                        store_last_message(member_key, kind, message_id, 0);
                    }
                }
                if let Some(loot) = &kill.loot {
                    let (pets, rest) = split_pets(loot);
                    if settings.notify_pets && !pets.is_empty() {
                        send_pet_embeds(&webhook_url, &member_name, &kill.npc_name, &pets, kill.account_kc.map(i64::from))
                            .await;
                    }
                    if settings.notify_drops {
                        let ge_prices = unauthed::get_ge_prices_map();
                        let lines = drop_lines(
                            &rest,
                            &ge_prices,
                            settings.drops_min_value,
                            settings.drops_unique_only,
                            &kill.npc_name,
                        );
                        if !lines.is_empty() {
                            dispatch_drop_message(
                                webhook_url.clone(),
                                group_id,
                                member_name.clone(),
                                kill.npc_name.clone(),
                                lines,
                                &ge_prices,
                            )
                            .await;
                        }
                    }
                }
            }
            GameEvent::Death(death) => {
                if settings.notify_deaths {
                    // Mirrors the "Kill" flow above, including for an unidentified killer (see
                    // `WebhookEventKind::Death`'s doc comment) - a repeat death within
                    // `WEBHOOK_MESSAGE_EDIT_TTL` edits the last death message (with a bumped
                    // "Deaths" count) instead of posting a new one, as long as nothing else was
                    // posted for this member in between (`LAST_MEMBER_MESSAGE` only ever holds the
                    // single most recent message per member, so a differing `WebhookEventKind`
                    // already means that check fails).
                    let killer = death.killer_name.clone();
                    let count = db::count_deaths_for_member_npc(&client, group_id, &member_name, killer.as_deref())
                        .await
                        .unwrap_or_else(|err| {
                            log::warn!("discord: failed to load death count: {}", err);
                            0
                        });
                    let description = match &killer {
                        Some(killer) => format!("{} died to [{}]({})", member_name, killer, wiki_url(killer)),
                        None => format!("{} died", member_name),
                    };
                    let fields = vec![("Deaths".to_string(), count.to_string())];
                    let thumbnail = killer.as_deref().map(|killer| boss_icon_url(&web_origin, killer));
                    let member_key: MemberKey = (group_id, member_name.clone());
                    let kind = WebhookEventKind::Death(killer);
                    let edited_id = match cached_message_for(&member_key, &kind) {
                        Some((message_id, _)) => {
                            let edited = edit_webhook_embed_rich(
                                webhook_url.clone(),
                                message_id.clone(),
                                "Death",
                                description.clone(),
                                DEATH_COLOR,
                                thumbnail.clone(),
                                fields.clone(),
                                Some(member_name.clone()),
                            )
                            .await;
                            edited.then_some(message_id)
                        }
                        None => None,
                    };
                    let message_id = match edited_id {
                        Some(message_id) => Some(message_id),
                        None => {
                            send_webhook_embed_rich_get_id(
                                webhook_url,
                                "Death",
                                description,
                                DEATH_COLOR,
                                thumbnail,
                                fields,
                                Some(member_name.clone()),
                            )
                            .await
                        }
                    };
                    if let Some(message_id) = message_id {
                        store_last_message(member_key, kind, message_id, 0);
                    }
                }
            }
            GameEvent::Loot(loot_event) if loot_event.source_type == crate::models::LootSourceType::Clue => {
                if settings.notify_clues && !loot_event.loot.is_empty() {
                    let tier = loot_event.clue_tier.as_deref().unwrap_or("unknown");
                    let ge_prices = unauthed::get_ge_prices_map();
                    let lines = drop_lines(&loot_event.loot, &ge_prices, 0, false, &loot_event.source_name);
                    if !lines.is_empty() {
                        let description = format!(
                            "{} opened a {} casket: {}",
                            member_name,
                            tier,
                            lines.iter().map(|(_, _, _, line)| line.as_str()).collect::<Vec<_>>().join(", ")
                        );
                        let is_rare = rarest_probability(&lines, &loot_event.source_name).is_some_and(|p| p <= RARE_DROP_THRESHOLD);
                        let (title, color) =
                            if is_rare { ("\u{1F48E} Rare clue reward", RARE_DROP_COLOR) } else { ("Clue casket", CLUE_COLOR) };
                        send_webhook_embed_rich(
                            webhook_url,
                            title,
                            description,
                            color,
                            Some(clue_casket_icon_url(tier)),
                            vec![("Tier".to_string(), tier.to_string())],
                            Some(member_name.clone()),
                        )
                        .await;
                    }
                }
            }
            GameEvent::Loot(loot_event) => {
                let (pets, rest) = split_pets(&loot_event.loot);
                if settings.notify_pets && !pets.is_empty() {
                    send_pet_embeds(&webhook_url, &member_name, &loot_event.source_name, &pets, None).await;
                }
                if settings.notify_drops && !rest.is_empty() {
                    let ge_prices = unauthed::get_ge_prices_map();
                    let lines = drop_lines(
                        &rest,
                        &ge_prices,
                        settings.drops_min_value,
                        settings.drops_unique_only,
                        &loot_event.source_name,
                    );
                    if !lines.is_empty() {
                        dispatch_drop_message(
                            webhook_url,
                            group_id,
                            member_name.clone(),
                            loot_event.source_name.clone(),
                            lines,
                            &ge_prices,
                        )
                        .await;
                    }
                }
            }
            // Raid completions aren't relayed per-event like the other variants above - they go
            // through `dispatch_raid_webhook` instead, called once from `raid_merge`'s finalize
            // step after the merge window closes, so a group's whole party doesn't each trigger
            // a separate near-duplicate Discord post for the same raid.
            GameEvent::Raid(_) => {}
        }
    });
}

/// Fire-and-forget, mirroring `dispatch_event_webhook` above - posts the merged raid-completion
/// message (`RaidCompletionPayload::to_message`) as a Discord embed, once per completion, when
/// the group has a webhook configured and `notify_raids` is on. Takes the finalized payload
/// directly (rather than a pre-built message) since the merge/finalize step that produces it
/// already lives outside this per-event match, and this now also needs `total_value` /
/// `participants` itself for the per-member split and record fields below.
///
/// `previous_best` is this group's best `total_value` for the same raid+difficulty *before* this
/// completion (see `db::group_raid_best_value`), looked up by the caller since that needs a db
/// client the finalize step already had open - `None` means this is the first-ever finalized
/// completion of this raid+difficulty for the group.
pub fn dispatch_raid_webhook(
    db_pool: Pool,
    group_id: i64,
    payload: crate::models::RaidCompletionPayload,
    previous_best: Option<i64>,
) {
    tokio::spawn(async move {
        let client = match db_pool.get().await {
            Ok(client) => client,
            Err(err) => {
                log::warn!("discord: failed to get db client: {}", err);
                return;
            }
        };
        let settings = match db::get_discord_webhook_settings(&client, group_id).await {
            Ok(settings) => settings,
            Err(err) => {
                log::warn!("discord: failed to load webhook settings: {}", err);
                return;
            }
        };
        drop(client);
        let Some(webhook_url) = settings.webhook_url else {
            return;
        };
        if !settings.notify_raids {
            return;
        }

        let message = payload.to_message();
        let mut fields = Vec::new();
        if payload.total_value > 0 && payload.participants.len() > 1 {
            for participant in &payload.participants {
                let pct = participant.value as f64 / payload.total_value as f64 * 100.0;
                fields.push((participant.member_name.clone(), format!("{:.0}% ({} gp)", pct, format_gp(participant.value))));
            }
        }
        let record_field = match previous_best {
            Some(best) if payload.total_value > best => "\u{1F3C6} New group record!".to_string(),
            Some(best) => format!("Best: {} gp", format_gp(best)),
            None => "\u{1F3C6} New group record!".to_string(),
        };
        fields.push(("Record".to_string(), record_field));

        send_webhook_embed_rich(
            webhook_url,
            "Raid",
            message,
            RAID_COLOR,
            Some(raid_icon_url(payload.raid_type)),
            fields,
            None,
        )
        .await;
    });
}

/// Fire-and-forget, mirroring `dispatch_event_webhook` above - posts the same message text
/// already built for the roster-websocket broadcast (`NotableDropEvent::to_message`) as a
/// Discord embed, when the group has a webhook configured and `notify_drops` is on. Notable
/// drops aren't a `GameEvent` (they're never stored, see `update_group_member`), so this takes
/// the pre-built message directly rather than matching over the enum like the function above.
///
/// `total_value` is gated against the group's own `drops_min_value` here, independently of the
/// plugin's `notableDropThreshold` that decided whether this event fired at all - that plugin
/// config only controls the in-game/websocket chat notification (see the `notable_drops` handler
/// in `authed.rs`), and must never let a drop below the group's configured Discord threshold
/// (e.g. a member left at the plugin's low default) leak into the webhook.
pub fn dispatch_drop_webhook(db_pool: Pool, group_id: i64, message: String, item_id: i32, total_value: i64) {
    tokio::spawn(async move {
        let client = match db_pool.get().await {
            Ok(client) => client,
            Err(err) => {
                log::warn!("discord: failed to get db client: {}", err);
                return;
            }
        };
        let settings = match db::get_discord_webhook_settings(&client, group_id).await {
            Ok(settings) => settings,
            Err(err) => {
                log::warn!("discord: failed to load webhook settings: {}", err);
                return;
            }
        };
        drop(client);
        let Some(webhook_url) = settings.webhook_url else {
            return;
        };
        if !settings.notify_drops || total_value < settings.drops_min_value {
            return;
        }

        let thumbnail = item_icon_url(item_id);
        send_webhook_embed_with_thumbnail(webhook_url, "Drop", message, DROP_COLOR, thumbnail).await;
    });
}

/// Fire-and-forget, mirroring `dispatch_event_webhook` above - posts a `ProgressEvent` (quest,
/// diary, combat-achievement, collection-log, or skill level-up milestone) as a Discord embed when
/// the matching notify setting is on.
///
/// Collection-log items post individually (`kind: "item"` in the payload), plus a page-completion
/// post (`kind: "page"`) when the addition just finished a page. Combat-achievement tasks post
/// individually (each linked to its wiki page), plus a boss-completion post (`kind: "boss"`)
/// linked to the boss's wiki page.
pub fn dispatch_progress_webhook(
    db_pool: Pool,
    group_id: i64,
    member_name: String,
    event: crate::progress_events::ProgressEvent,
    web_origin: String,
) {
    use crate::progress_events::{
        EVENT_TYPE_COLLECTION_LOG, EVENT_TYPE_COMBAT_TASK, EVENT_TYPE_DIARY, EVENT_TYPE_LEVEL_UP, EVENT_TYPE_QUEST,
    };

    tokio::spawn(async move {
        let client = match db_pool.get().await {
            Ok(client) => client,
            Err(err) => {
                log::warn!("discord: failed to get db client: {}", err);
                return;
            }
        };
        let settings = match db::get_discord_webhook_settings(&client, group_id).await {
            Ok(settings) => settings,
            Err(err) => {
                log::warn!("discord: failed to load webhook settings: {}", err);
                return;
            }
        };
        drop(client);
        let Some(webhook_url) = settings.webhook_url else {
            return;
        };

        let post = match event.event_type {
            EVENT_TYPE_QUEST if settings.notify_quests => {
                let quest_id = event.payload["quest_id"].as_i64().unwrap_or_default() as i32;
                let name = crate::quest_ids::quest_name(quest_id).unwrap_or("a quest");
                let difficulty = crate::quest_ids::quest_difficulty(quest_id);
                let thumbnail = quest_icon_url(&web_origin, difficulty);
                let mut fields = Vec::new();
                if let Some(difficulty) = difficulty {
                    fields.push(("Difficulty".to_string(), difficulty.to_string()));
                }
                if let (Some(completed), Some(total)) =
                    (event.payload["completed_count"].as_i64(), event.payload["total_count"].as_i64())
                {
                    fields.push(("Quests complete".to_string(), format!("{} of {}", completed, total)));
                }
                Some((
                    "Quest",
                    format!("{} completed [{}]({})", member_name, name, wiki_url(name)),
                    QUEST_COLOR,
                    thumbnail,
                    fields,
                ))
            }
            EVENT_TYPE_DIARY if settings.notify_diaries => {
                let region = event.payload["region"].as_str().unwrap_or("");
                let tier = event.payload["tier"].as_str().unwrap_or("");
                Some((
                    "Diary",
                    format!(
                        "{} completed the [{}]({}) {} diary",
                        member_name,
                        region,
                        wiki_url(&format!("{} Diary", region)),
                        tier
                    ),
                    DIARY_COLOR,
                    Some(diary_icon_url()),
                    Vec::new(),
                ))
            }
            EVENT_TYPE_COMBAT_TASK if settings.notify_combat_achievements && event.payload["kind"] == "boss" => {
                let boss = event.payload["boss"].as_str().unwrap_or("a boss");
                let url = wiki_url(boss);
                Some((
                    "Combat achievements",
                    format!("{} completed every combat achievement for [{}]({})", member_name, boss, url),
                    COMBAT_TASK_COLOR,
                    Some(combat_achievement_icon_url()),
                    Vec::new(),
                ))
            }
            EVENT_TYPE_COMBAT_TASK if settings.notify_combat_achievements && event.payload["kind"].is_null() => {
                let task_id = event.payload["task_id"].as_i64().unwrap_or_default();
                let task = crate::combat_achievement_content::task_name(task_id).unwrap_or("a combat task");
                let url = wiki_url(task);
                let mut fields = Vec::new();
                if let Some(pct) = crate::combat_achievement_content::completion_percent(task_id) {
                    fields.push(("Completed by".to_string(), format!("{:.1}% of players", pct)));
                }
                Some((
                    "Combat achievements",
                    format!("{} completed the combat task [{}]({})", member_name, task, url),
                    COMBAT_TASK_COLOR,
                    Some(combat_achievement_icon_url()),
                    fields,
                ))
            }
            EVENT_TYPE_COLLECTION_LOG if settings.notify_collection_log && event.payload["kind"] == "item" => {
                let item_id = event.payload["item_id"].as_i64().unwrap_or_default() as i32;
                let quantity = event.payload["quantity"].as_i64().unwrap_or(1);
                let name = item_names::name(item_id).unwrap_or("an item");
                let mut fields = Vec::new();
                let page = event.payload["page"].as_str();
                if let (Some(page), Some(owned), Some(total)) =
                    (page, event.payload["page_owned"].as_i64(), event.payload["page_total"].as_i64())
                {
                    fields.push(("Page progress".to_string(), format!("{}/{} ({})", owned, total, page)));
                }
                if let Some(rate) =
                    page.and_then(|page| drop_rates::lookup(page, item_id)).and_then(|entry| entry.rate.clone())
                {
                    fields.push(("Drop rate".to_string(), rate));
                }
                Some((
                    "Collection log",
                    format!(
                        "{} added [{}{}]({}) to their collection log",
                        member_name,
                        if quantity > 1 { format!("{}x ", quantity) } else { String::new() },
                        name,
                        item_names::wiki_link(item_id)
                    ),
                    COLLECTION_LOG_COLOR,
                    item_icon_url(item_id),
                    fields,
                ))
            }
            EVENT_TYPE_COLLECTION_LOG if settings.notify_collection_log && event.payload["kind"] == "page" => {
                let page = event.payload["page"].as_str().unwrap_or("a page");
                Some((
                    "Collection log",
                    format!(
                        "{} completed the [{}]({}) collection log page",
                        member_name,
                        page,
                        wiki_url(page)
                    ),
                    COLLECTION_LOG_COLOR,
                    Some(collection_log_icon_url(&web_origin, page)),
                    Vec::new(),
                ))
            }
            // Level 99 always posts regardless of the configured interval - see
            // `DiscordWebhookSettings::level_up_interval`'s doc comment.
            EVENT_TYPE_LEVEL_UP if settings.notify_level_ups => {
                let skill = event.payload["skill"].as_str().unwrap_or("a skill");
                let level = event.payload["level"].as_i64().unwrap_or_default();
                let interval = i64::from(settings.level_up_interval.max(1));
                if level != 99 && level % interval != 0 {
                    None
                } else {
                    Some((
                        "Level up",
                        format!(
                            "{} reached level {} [{}]({})",
                            member_name,
                            level,
                            skill,
                            wiki_url(skill)
                        ),
                        LEVEL_UP_COLOR,
                        Some(skill_icon_url(skill)),
                        Vec::new(),
                    ))
                }
            }
            _ => None,
        };

        if let Some((title, description, color, thumbnail, fields)) = post {
            send_webhook_embed_rich(webhook_url, title, description, color, thumbnail, fields, Some(member_name)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rarest_probability_flags_a_curated_rare_item() {
        // Draconic visage is 1/5000 off Vorkath - well past RARE_DROP_THRESHOLD (1/500).
        let lines = vec![(11286, 1, 0, "1x Draconic visage".to_string())];
        let p = rarest_probability(&lines, "Vorkath").expect("draconic visage has a curated rate");
        assert!(p <= RARE_DROP_THRESHOLD);
    }

    #[test]
    fn rarest_probability_is_none_without_curated_rate_data() {
        let lines = vec![(995, 1000, 0, "1000x Coins".to_string())];
        assert!(rarest_probability(&lines, "Some Random Npc").is_none());
    }

    #[test]
    fn rarest_probability_takes_the_lowest_across_multiple_items() {
        // Silver ore (442) has no curated rate; draconic visage (11286) does - the missing one
        // must not hide the rare one that's actually there.
        let lines = vec![
            (442, 1, 0, "1x Silver ore".to_string()),
            (11286, 1, 0, "1x Draconic visage".to_string()),
        ];
        let p = rarest_probability(&lines, "Vorkath").expect("draconic visage has a curated rate");
        assert!(p <= RARE_DROP_THRESHOLD);
    }
}
