import { pubsub } from "./pubsub";
import { utility } from "../utility";
import { groupData } from "./group-data";
import { accountStorage } from "./account-storage";
import { adminViewSession } from "./admin-view-session";

class Api {
  constructor() {
    this.baseUrl = "/api";
    this.createGroupUrl = `${this.baseUrl}/create-group`;
    this.enabled = false;
    this.adminView = false;
  }

  // Permission-gated group actions (member removal, group settings) need to identify the
  // *account* performing them, not just the group - the shared group token proves membership
  // in the group but carries no account identity. Undefined when no account is logged in;
  // the server rejects those requests with 401 rather than silently no-op'ing. Not applicable
  // in admin-view mode, where there's no account behind the request at all.
  get accountAuthHeaders() {
    if (this.adminView) return {};
    const accountToken = accountStorage.getAccountToken();
    return accountToken ? { "X-Account-Authorization": accountToken } : {};
  }

  // Every group-scoped request goes to a group-token-authed `/api/group/{name}` route normally,
  // or, in admin-view mode, to the admin-bearer-authed `/api/admin/group-view/{id}` route
  // instead - which only mounts read handlers, so mutating calls fail closed with a 404 rather
  // than silently no-op'ing. This is the single seam that makes every dashboard page reusable
  // read-only for an admin without touching the pages themselves.
  get groupScopeUrl() {
    if (this.adminView) {
      return `${this.baseUrl}/admin/group-view/${this.adminViewGroupId}`;
    }
    return `${this.baseUrl}/group/${this.groupName}`;
  }

  get authHeader() {
    return this.adminView ? `Bearer ${this.adminToken}` : this.groupToken;
  }

  get getGroupDataUrl() {
    return `${this.groupScopeUrl}/get-group-data`;
  }

  get deleteMemberUrl() {
    return `${this.groupScopeUrl}/delete-group-member`;
  }

  get blockMemberUrl() {
    return `${this.groupScopeUrl}/block-group-member`;
  }

  get unblockMemberUrl() {
    return `${this.groupScopeUrl}/unblock-group-member`;
  }

  get blockedMembersUrl() {
    return `${this.groupScopeUrl}/get-blocked-members`;
  }

  get canKickMembersUrl() {
    return `${this.groupScopeUrl}/can-kick-members`;
  }

  get amILoggedInUrl() {
    return `${this.groupScopeUrl}/am-i-logged-in`;
  }

  get groupPermissionsUrl() {
    return `${this.groupScopeUrl}/get-group-permissions`;
  }

  get myPermissionsUrl() {
    return `${this.groupScopeUrl}/get-my-permissions`;
  }

  get updateGroupPermissionsUrl() {
    return `${this.groupScopeUrl}/update-group-permissions`;
  }

  get updateMemberColorUrl() {
    return `${this.groupScopeUrl}/update-member-color`;
  }

  get discordSettingsUrl() {
    return `${this.groupScopeUrl}/get-discord-settings`;
  }

  get updateDiscordSettingsUrl() {
    return `${this.groupScopeUrl}/update-discord-settings`;
  }

  get testDiscordNotificationUrl() {
    return `${this.groupScopeUrl}/test-discord-notification`;
  }

  get renameGroupUrl() {
    return `${this.groupScopeUrl}/rename-group`;
  }

  get rerollGroupTokenUrl() {
    return `${this.groupScopeUrl}/reroll-group-token`;
  }

  get deleteGroupUrl() {
    return `${this.groupScopeUrl}/delete-group`;
  }

  get gePricesUrl() {
    return `${this.baseUrl}/ge-prices`;
  }

  get skillDataUrl() {
    return `${this.groupScopeUrl}/get-skill-data`;
  }

  get leaderboardUrl() {
    return `${this.groupScopeUrl}/get-leaderboard`;
  }

  get captchaEnabledUrl() {
    return `${this.baseUrl}/captcha-enabled`;
  }

  get portraitUrl() {
    return `${this.groupScopeUrl}/portrait`;
  }

  get bossKcUrl() {
    return `${this.groupScopeUrl}/boss-kc`;
  }

  get activityEventsUrl() {
    return `${this.groupScopeUrl}/get-activity-events`;
  }

  get activitySettingsUrl() {
    return `${this.groupScopeUrl}/get-activity-settings`;
  }

  get updateActivitySettingsUrl() {
    return `${this.groupScopeUrl}/update-activity-settings`;
  }

  get activityReactionsUrl() {
    return `${this.groupScopeUrl}/get-activity-reactions`;
  }

  activityEventReactUrl(eventId) {
    return `${this.groupScopeUrl}/activity-events/${eventId}/react`;
  }

  activityEventCommentsUrl(eventId) {
    return `${this.groupScopeUrl}/activity-events/${eventId}/comments`;
  }

  get lootLogUrl() {
    return `${this.groupScopeUrl}/get-loot-log`;
  }

  get lootLogSummaryUrl() {
    return `${this.groupScopeUrl}/get-loot-log-summary`;
  }

  get slayerHistoryUrl() {
    return `${this.groupScopeUrl}/get-slayer-task-history`;
  }

  get slayerStatsUrl() {
    return `${this.groupScopeUrl}/get-slayer-task-stats`;
  }

  get itemBonusesUrl() {
    return `${this.groupScopeUrl}/get-item-bonuses`;
  }

  get activePingsUrl() {
    return `${this.groupScopeUrl}/get-active-pings`;
  }

  get activeRaidMarkersUrl() {
    return `${this.groupScopeUrl}/get-active-raid-markers`;
  }

  get chatMessagesUrl() {
    return `${this.groupScopeUrl}/get-chat-messages`;
  }

  get sendChatMessageUrl() {
    return `${this.groupScopeUrl}/send-chat-message`;
  }

  // The chat drawer's live feed - same `/ws` handler and `Authenticated{group_id}` the RuneLite
  // party overlay uses (see server's `websocket::party_overlay_ws`), just reached through the
  // group-token scope instead of the character-key one. The browser `WebSocket` constructor can't
  // set an `Authorization` header, so the token rides a `?token=` query param instead - the
  // server's `auth_middleware.rs` falls back to it only when the header is absent. Not available
  // in admin-view mode (no group token to send - see `authHeader`).
  get chatSocketUrl() {
    if (this.adminView) return undefined;
    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    return `${protocol}//${window.location.host}${this.groupScopeUrl}/ws?token=${encodeURIComponent(this.authHeader)}`;
  }

  async getChatMessages(since = 0) {
    const response = await fetch(`${this.chatMessagesUrl}?since=${since}`, {
      headers: { Authorization: this.authHeader },
    });
    if (!response.ok) return [];
    return response.json();
  }

  async sendChatMessage(text) {
    const response = await fetch(this.sendChatMessageUrl, {
      body: JSON.stringify({ text }),
      headers: {
        "Content-Type": "application/json",
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
      method: "POST",
    });

    return response;
  }

  setCredentials(groupName, groupToken) {
    this.groupName = groupName;
    this.groupToken = groupToken;
  }

  async restart() {
    const groupName = this.groupName;
    const groupToken = this.groupToken;
    if (this.adminView) {
      await this.enableAdminView(this.adminViewGroupId, groupName, this.adminToken);
    } else {
      await this.enable(groupName, groupToken);
    }
  }

  async enable(groupName, groupToken) {
    await this.disable();
    this.adminView = false;
    this.setCredentials(groupName, groupToken);
    await this.startPolling();
  }

  // Same startup as `enable()`, but authenticated as a global admin viewing this group
  // read-only rather than as a member holding the group's token - see `groupScopeUrl`.
  async enableAdminView(groupId, groupName, adminToken) {
    await this.disable();
    this.adminView = true;
    this.adminViewGroupId = groupId;
    this.groupName = groupName;
    this.adminToken = adminToken;
    await this.startPolling();
  }

  async startPolling() {
    this.nextCheck = new Date(0).toISOString();
    if (!this.enabled) {
      this.enabled = true;
      // getGroupInterval is a Promise so we can make sure this method does not leak
      // any intervals with multiple calls to .enable(). This could be possible because of
      // the wait for the item and quest data loads before we create the interval.
      this.getGroupInterval = pubsub.waitForAllEvents("item-data-loaded", "quest-data-loaded").then(() => {
        return utility.callOnInterval(this.pollTick.bind(this), 1000);
      });
    }

    await this.getGroupInterval;
  }

  async disable() {
    this.enabled = false;
    this.adminView = false;
    this.adminViewGroupId = undefined;
    this.adminToken = undefined;
    this.groupName = undefined;
    this.groupToken = undefined;
    groupData.members = new Map();
    groupData.groupItems = {};
    groupData.filters = [""];
    this.seenPingIds = undefined;
    if (this.getGroupInterval) {
      window.clearInterval(await this.getGroupInterval);
    }
  }

  // The single interval tick driving `startPolling` - keeps active pings on the same ~1s
  // cadence as member positions rather than opening a second interval for them.
  async pollTick() {
    await this.getGroupData();
    await this.getActivePings();
    await this.getActiveRaidMarkers();
  }

  async getActivePings() {
    const response = await fetch(this.activePingsUrl, {
      headers: {
        Authorization: this.authHeader,
      },
    });
    if (!response.ok) return;

    const pings = await response.json();

    // A ping newly appearing since the last poll gets a toast - same "diff against what we saw
    // last time" idea as toast-source.js's activity events, just sourced from this poll instead
    // since pings never land in the activity event feed (they're ephemeral, not stored).
    if (this.seenPingIds) {
      for (const ping of pings) {
        if (!this.seenPingIds.has(ping.pingId)) {
          pubsub.publish("toast", { type: "ping", ping });
        }
      }
    }
    this.seenPingIds = new Set(pings.map((ping) => ping.pingId));

    pubsub.publish("active-pings", pings);
  }

  // No toast/seen-id diffing here, unlike getActivePings - a raid marker is a persistent state
  // change (up to 8 per player) rather than a one-off event worth calling out, and the plugin
  // itself doesn't chat-message on marker start either.
  async getActiveRaidMarkers() {
    const response = await fetch(this.activeRaidMarkersUrl, {
      headers: {
        Authorization: this.authHeader,
      },
    });
    if (!response.ok) return;

    const markers = await response.json();
    pubsub.publish("active-raid-markers", markers);
  }

  async getGroupData() {
    const nextCheck = this.nextCheck;

    const response = await fetch(`${this.getGroupDataUrl}?from_time=${nextCheck}`, {
      headers: {
        Authorization: this.authHeader,
      },
    });
    if (!response.ok) {
      if (response.status === 401) {
        const wasAdminView = this.adminView;
        await this.disable();
        if (wasAdminView) adminViewSession.clear();
        window.history.pushState("", "", wasAdminView ? "/admin/groups" : "/");
        pubsub.publish("get-group-data");
      }
      return;
    }

    const newGroupData = await response.json();
    this.nextCheck = groupData.update(newGroupData).toISOString();
    pubsub.publish("get-group-data", groupData);
  }

  async createGroup(groupName, memberNames, captchaResponse) {
    const response = await fetch(this.createGroupUrl, {
      body: JSON.stringify({ name: groupName, member_names: memberNames, captcha_response: captchaResponse }),
      headers: {
        "Content-Type": "application/json",
      },
      method: "POST",
    });

    return response;
  }

  async removeMember(memberName) {
    const response = await fetch(this.deleteMemberUrl, {
      body: JSON.stringify({ name: memberName }),
      headers: {
        "Content-Type": "application/json",
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
      method: "DELETE",
    });

    return response;
  }

  async blockMember(memberName) {
    const response = await fetch(this.blockMemberUrl, {
      body: JSON.stringify({ name: memberName }),
      headers: {
        "Content-Type": "application/json",
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
      method: "POST",
    });

    return response;
  }

  async unblockMember(memberName) {
    const response = await fetch(this.unblockMemberUrl, {
      body: JSON.stringify({ name: memberName }),
      headers: {
        "Content-Type": "application/json",
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
      method: "POST",
    });

    return response;
  }

  async getBlockedMembers() {
    const response = await fetch(this.blockedMembersUrl, {
      headers: {
        Authorization: this.authHeader,
      },
    });

    return response;
  }

  // The endpoint itself is the permission gate (401/403 when this account can't kick), so the
  // site treats "ok" as "show the remove/block controls" rather than duplicating the permission
  // check client-side.
  async canKickMembers() {
    const response = await fetch(this.canKickMembersUrl, {
      headers: {
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
    });

    return response.ok;
  }

  async getDiscordSettings() {
    const response = await fetch(this.discordSettingsUrl, {
      headers: {
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
    });

    return response;
  }

  async updateDiscordSettings(settings) {
    const response = await fetch(this.updateDiscordSettingsUrl, {
      body: JSON.stringify(settings),
      headers: {
        "Content-Type": "application/json",
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
      method: "PUT",
    });

    return response;
  }

  async testDiscordNotification(kind) {
    const response = await fetch(this.testDiscordNotificationUrl, {
      body: JSON.stringify({ kind }),
      headers: {
        "Content-Type": "application/json",
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
      method: "POST",
    });

    return response;
  }

  async getGroupPermissions() {
    const response = await fetch(this.groupPermissionsUrl, {
      headers: {
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
    });

    return response;
  }

  async getMyPermissions() {
    const response = await fetch(this.myPermissionsUrl, {
      headers: {
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
    });

    return response;
  }

  async updateGroupPermissions(accountId, patch) {
    const response = await fetch(this.updateGroupPermissionsUrl, {
      body: JSON.stringify({ account_id: accountId, ...patch }),
      headers: {
        "Content-Type": "application/json",
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
      method: "PUT",
    });

    return response;
  }

  async updateMemberColor(accountId, color) {
    const response = await fetch(this.updateMemberColorUrl, {
      body: JSON.stringify({ account_id: accountId, color }),
      headers: {
        "Content-Type": "application/json",
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
      method: "PUT",
    });

    return response;
  }

  async renameGroup(newName) {
    const response = await fetch(this.renameGroupUrl, {
      body: JSON.stringify({ new_name: newName }),
      headers: {
        "Content-Type": "application/json",
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
      method: "PUT",
    });

    return response;
  }

  async rerollGroupToken() {
    const response = await fetch(this.rerollGroupTokenUrl, {
      headers: {
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
      method: "POST",
    });

    return response;
  }

  async deleteGroup() {
    const response = await fetch(this.deleteGroupUrl, {
      headers: {
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
      method: "DELETE",
    });

    return response;
  }

  async amILoggedIn() {
    const response = await fetch(this.amILoggedInUrl, {
      headers: { Authorization: this.authHeader },
    });

    return response;
  }

  async getGePrices() {
    const response = await fetch(this.gePricesUrl);
    return response;
  }

  async getSkillData(period) {
    const response = await fetch(`${this.skillDataUrl}?period=${period}`, {
      headers: {
        Authorization: this.authHeader,
      },
    });
    if (!response.ok) {
      return [];
    }
    return response.json();
  }

  async getLeaderboard(metric, window, skill) {
    const query = new URLSearchParams({ metric, window });
    if (skill && metric === "xp") query.set("skill", skill);
    const response = await fetch(`${this.leaderboardUrl}?${query.toString()}`, {
      headers: {
        Authorization: this.authHeader,
      },
    });
    if (!response.ok) {
      return { metric, window, available_bosses: [], entries: [] };
    }
    return response.json();
  }

  async getCaptchaEnabled() {
    const response = await fetch(this.captchaEnabledUrl);
    return response.json();
  }

  async getActivityEvents({ memberName, eventType, before, limit } = {}) {
    const query = new URLSearchParams();
    if (memberName) query.set("member_name", memberName);
    if (eventType) query.set("event_type", eventType);
    if (before) query.set("before", before);
    if (limit) query.set("limit", limit);

    const response = await fetch(`${this.activityEventsUrl}?${query.toString()}`, {
      headers: {
        Authorization: this.authHeader,
      },
    });
    if (!response.ok) {
      return [];
    }
    return response.json();
  }

  // Reachable by any group member - the feed needs to know whether to render reaction UI at all,
  // not just the admin settings page (see server's `get_activity_settings` doc comment).
  async getActivitySettings() {
    const response = await fetch(this.activitySettingsUrl, {
      headers: { Authorization: this.authHeader },
    });
    if (!response.ok) {
      return { reactions_enabled: false };
    }
    return response.json();
  }

  async updateActivitySettings(settings) {
    const response = await fetch(this.updateActivitySettingsUrl, {
      body: JSON.stringify(settings),
      headers: {
        "Content-Type": "application/json",
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
      method: "PUT",
    });

    return response;
  }

  // Batch reaction/comment-count lookup for whatever page of events the feed currently has
  // rendered - separate from the get-activity-events cursor poll (see server's
  // `GetActivityReactionsQuery` doc comment for why). Returns `{}` when reactions are off for the
  // group, or on any failure - callers treat an empty object as "nothing to show" either way.
  async getActivityReactions(eventIds) {
    if (!eventIds || !eventIds.length) return {};
    const response = await fetch(`${this.activityReactionsUrl}?event_ids=${eventIds.join(",")}`, {
      headers: {
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
    });
    if (!response.ok) return {};
    return response.json();
  }

  async reactToActivityEvent(eventId, reaction) {
    const response = await fetch(this.activityEventReactUrl(eventId), {
      body: JSON.stringify({ reaction }),
      headers: {
        "Content-Type": "application/json",
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
      method: "PUT",
    });

    return response;
  }

  async getActivityComments(eventId) {
    const response = await fetch(this.activityEventCommentsUrl(eventId), {
      headers: {
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
    });
    if (!response.ok) {
      return { comments: [], comment_count: 0 };
    }
    return response.json();
  }

  async addActivityComment(eventId, commentText) {
    const response = await fetch(this.activityEventCommentsUrl(eventId), {
      body: JSON.stringify({ comment_text: commentText }),
      headers: {
        "Content-Type": "application/json",
        Authorization: this.authHeader,
        ...this.accountAuthHeaders,
      },
      method: "POST",
    });

    return response;
  }

  async getLootLog({ before, limit, search, itemIds, categories } = {}) {
    const query = new URLSearchParams();
    if (before) query.set("before", before);
    if (limit) query.set("limit", limit);
    if (search) query.set("search", search);
    if (itemIds && itemIds.length) query.set("item_ids", itemIds.join(","));
    if (categories && categories.length) query.set("categories", categories.join(","));
    // Plumbing only, never shown in any UI - lets the server resolve calendar-relative search
    // tokens ("today", "this week", ...) against the viewer's own calendar day. See
    // GetLootLogQuery::tz_offset_minutes on the server.
    if (search) query.set("tz_offset_minutes", new Date().getTimezoneOffset());

    const response = await fetch(`${this.lootLogUrl}?${query.toString()}`, {
      headers: {
        Authorization: this.authHeader,
      },
    });
    if (!response.ok) {
      return { events: [], next_before: null, scan_exhausted: true };
    }
    return response.json();
  }

  async getLootLogSummary({ search, itemIds, categories } = {}) {
    const query = new URLSearchParams();
    if (search) query.set("search", search);
    if (itemIds && itemIds.length) query.set("item_ids", itemIds.join(","));
    if (categories && categories.length) query.set("categories", categories.join(","));
    if (search) query.set("tz_offset_minutes", new Date().getTimezoneOffset());

    const response = await fetch(`${this.lootLogSummaryUrl}?${query.toString()}`, {
      headers: {
        Authorization: this.authHeader,
      },
    });
    if (!response.ok) {
      return { total_value: 0, event_count: 0 };
    }
    return response.json();
  }

  async getSlayerHistory({ playerName, page, status, masterName } = {}) {
    const query = new URLSearchParams();
    query.set("player_name", playerName);
    if (page) query.set("page", page);
    if (status) query.set("status", status);
    if (masterName) query.set("master_name", masterName);

    const response = await fetch(`${this.slayerHistoryUrl}?${query.toString()}`, {
      headers: {
        Authorization: this.authHeader,
      },
    });
    if (!response.ok) {
      return { entries: [], page: 1, page_size: 5, total_entries: 0, total_pages: 1 };
    }
    return response.json();
  }

  async getSlayerStats({ playerName } = {}) {
    const query = new URLSearchParams();
    query.set("player_name", playerName);

    const response = await fetch(`${this.slayerStatsUrl}?${query.toString()}`, {
      headers: {
        Authorization: this.authHeader,
      },
    });
    if (!response.ok) {
      return {
        tasks_completed: 0,
        total_kills: 0,
        total_points_earned: 0,
        completion_rate: 0,
      };
    }
    return response.json();
  }

  async getItemBonuses(itemId) {
    const response = await fetch(`${this.itemBonusesUrl}?item_id=${itemId}`, {
      headers: {
        Authorization: this.authHeader,
      },
    });
    if (!response.ok) {
      throw new Error(`get-item-bonuses ${itemId} failed: ${response.status}`);
    }
    return response.json();
  }

  // Live OSRS hiscores boss/clue/minigame kill counts for `memberName` (boss-kc-panel) - fetched
  // fresh from Jagex on every call server-side, never cached here. Returns `{ ok: true, data }`
  // on success or `{ ok: false, kind, message }` on failure (`kind` is `"not_found"` for a
  // private/nonexistent hiscores profile or `"unavailable"` for a network/5xx failure - see
  // `hiscores::HiscoresError` server-side) so the panel can show a distinct message per case.
  async getBossKc(memberName) {
    let response;
    try {
      response = await fetch(`${this.bossKcUrl}/${encodeURIComponent(memberName)}`, {
        headers: { Authorization: this.authHeader },
      });
    } catch {
      return { ok: false, kind: "unavailable", message: "Could not reach the server" };
    }

    const body = await response.json().catch(() => null);
    if (!response.ok) {
      return {
        ok: false,
        kind: body?.kind ?? "unavailable",
        message: body?.message ?? "Failed to load hiscores",
      };
    }
    return { ok: true, data: body };
  }

  async getPortrait(memberName) {
    const response = await fetch(`${this.portraitUrl}/${encodeURIComponent(memberName)}`, {
      headers: {
        Authorization: this.authHeader,
      },
    });
    if (!response.ok) {
      return null;
    }
    return response.arrayBuffer();
  }
}

const api = new Api();

export { api };
