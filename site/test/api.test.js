import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../src/data/api";
import { pubsub } from "../src/data/pubsub";
import { utility } from "../src/utility";
import { groupData } from "../src/data/group-data";

describe("api", () => {
  beforeEach(() => {
    api.enabled = false;
    api.groupName = undefined;
    api.groupToken = undefined;
    api.getGroupInterval = undefined;
    api.nextCheck = undefined;

    groupData.members = new Map();
    groupData.groupItems = {};
    groupData.filters = ["existing"];

    globalThis.fetch = vi.fn();
  });

  it("sets credentials and exposes group-scoped urls", () => {
    api.setCredentials("iron-team", "secret-token");

    expect(api.groupName).toBe("iron-team");
    expect(api.groupToken).toBe("secret-token");
    expect(api.getGroupDataUrl).toContain("/group/iron-team/get-group-data");
    expect(api.deleteMemberUrl).toContain("/group/iron-team/delete-group-member");
    expect(api.blockMemberUrl).toContain("/group/iron-team/block-group-member");
    expect(api.unblockMemberUrl).toContain("/group/iron-team/unblock-group-member");
    expect(api.blockedMembersUrl).toContain("/group/iron-team/get-blocked-members");
    expect(api.amILoggedInUrl).toContain("/group/iron-team/am-i-logged-in");
    expect(api.skillDataUrl).toContain("/group/iron-team/get-skill-data");
    expect(api.groupPermissionsUrl).toContain("/group/iron-team/get-group-permissions");
    expect(api.updateGroupPermissionsUrl).toContain("/group/iron-team/update-group-permissions");
    expect(api.canKickMembersUrl).toContain("/group/iron-team/can-kick-members");
    expect(api.myPermissionsUrl).toContain("/group/iron-team/get-my-permissions");
    expect(api.activityEventsUrl).toContain("/group/iron-team/get-activity-events");
    expect(api.lootLogUrl).toContain("/group/iron-team/get-loot-log");
    expect(api.lootLogSummaryUrl).toContain("/group/iron-team/get-loot-log-summary");
    expect(api.leaderboardUrl).toContain("/group/iron-team/get-leaderboard");
  });

  it("getLeaderboard builds the query string from metric/window and returns the parsed result", async () => {
    api.setCredentials("iron-team", "secret-token");

    const result = { metric: "xp", window: "daily", available_bosses: [], entries: [] };
    globalThis.fetch.mockResolvedValueOnce({ ok: true, json: vi.fn().mockResolvedValue(result) });

    const actual = await api.getLeaderboard("xp", "daily");

    expect(actual).toEqual(result);
    const [url, options] = globalThis.fetch.mock.calls[0];
    expect(url).toContain("/group/iron-team/get-leaderboard?");
    expect(url).toContain("metric=xp");
    expect(url).toContain("window=daily");
    expect(options).toEqual({ headers: { Authorization: "secret-token" } });
  });

  it("getLeaderboard includes the skill param only for the xp metric", async () => {
    api.setCredentials("iron-team", "secret-token");
    globalThis.fetch.mockResolvedValueOnce({ ok: true, json: vi.fn().mockResolvedValue({}) });

    await api.getLeaderboard("xp", "daily", "Woodcutting");

    const [url] = globalThis.fetch.mock.calls[0];
    expect(url).toContain("skill=Woodcutting");
  });

  it("getLeaderboard omits the skill param when not provided", async () => {
    api.setCredentials("iron-team", "secret-token");
    globalThis.fetch.mockResolvedValueOnce({ ok: true, json: vi.fn().mockResolvedValue({}) });

    await api.getLeaderboard("xp", "weekly");

    const [url] = globalThis.fetch.mock.calls[0];
    expect(url).not.toContain("skill=");
  });

  it("getLeaderboard returns an empty result when the request fails", async () => {
    api.setCredentials("iron-team", "secret-token");
    globalThis.fetch.mockResolvedValueOnce({ ok: false });

    expect(await api.getLeaderboard("xp", "daily")).toEqual({
      metric: "xp",
      window: "daily",
      available_bosses: [],
      entries: [],
    });
  });

  it("getLootLog builds the query string from provided filters and returns the parsed page", async () => {
    api.setCredentials("iron-team", "secret-token");

    const page = { events: [{ member_name: "Zezima", source_name: "Vorkath" }], next_before: null, scan_exhausted: true };
    globalThis.fetch.mockResolvedValueOnce({ ok: true, json: vi.fn().mockResolvedValue(page) });

    const result = await api.getLootLog({
      before: "2026-01-01T00:00:00Z",
      limit: 25,
      search: "vorkath",
      itemIds: [995, 11286],
      categories: ["boss", "chest"],
    });

    expect(result).toEqual(page);
    const [url, options] = globalThis.fetch.mock.calls[0];
    expect(url).toContain("/group/iron-team/get-loot-log?");
    expect(url).toContain("before=2026-01-01T00%3A00%3A00Z");
    expect(url).toContain("limit=25");
    expect(url).toContain("search=vorkath");
    expect(url).toContain("item_ids=995%2C11286");
    expect(url).toContain("categories=boss%2Cchest");
    expect(options).toEqual({ headers: { Authorization: "secret-token" } });
  });

  it("getLootLog returns an empty exhausted page when the request fails", async () => {
    api.setCredentials("iron-team", "secret-token");
    globalThis.fetch.mockResolvedValueOnce({ ok: false });

    expect(await api.getLootLog()).toEqual({ events: [], next_before: null, scan_exhausted: true });
  });

  it("getLootLogSummary builds the query string from provided filters and returns the parsed totals", async () => {
    api.setCredentials("iron-team", "secret-token");

    const summary = { total_value: 1000000, event_count: 12 };
    globalThis.fetch.mockResolvedValueOnce({ ok: true, json: vi.fn().mockResolvedValue(summary) });

    const result = await api.getLootLogSummary({ search: "zulrah", categories: ["boss"] });

    expect(result).toEqual(summary);
    const [url, options] = globalThis.fetch.mock.calls[0];
    expect(url).toContain("/group/iron-team/get-loot-log-summary?");
    expect(url).toContain("search=zulrah");
    expect(url).toContain("categories=boss");
    expect(options).toEqual({ headers: { Authorization: "secret-token" } });
  });

  it("getLootLogSummary returns zeroed totals when the request fails", async () => {
    api.setCredentials("iron-team", "secret-token");
    globalThis.fetch.mockResolvedValueOnce({ ok: false });

    expect(await api.getLootLogSummary()).toEqual({ total_value: 0, event_count: 0 });
  });

  it("getActivityEvents builds the query string from provided filters and returns the parsed events", async () => {
    api.setCredentials("iron-team", "secret-token");

    const events = [{ id: 1, event_type: "kill" }];
    globalThis.fetch.mockResolvedValueOnce({ ok: true, json: vi.fn().mockResolvedValue(events) });

    const result = await api.getActivityEvents({
      memberName: "Zezima",
      eventType: "kill",
      before: "2026-01-01T00:00:00Z",
      limit: 10,
    });

    expect(result).toEqual(events);
    const [url, options] = globalThis.fetch.mock.calls[0];
    expect(url).toContain("/group/iron-team/get-activity-events?");
    expect(url).toContain("member_name=Zezima");
    expect(url).toContain("event_type=kill");
    expect(url).toContain("limit=10");
    expect(options).toEqual({ headers: { Authorization: "secret-token" } });
  });

  it("getActivityEvents returns an empty array when the request fails", async () => {
    api.setCredentials("iron-team", "secret-token");
    globalThis.fetch.mockResolvedValueOnce({ ok: false });

    expect(await api.getActivityEvents()).toEqual([]);
  });

  it("canKickMembers resolves to whether the endpoint responded ok", async () => {
    api.setCredentials("testgroup", "token");

    globalThis.fetch.mockResolvedValueOnce({ ok: true });
    expect(await api.canKickMembers()).toBe(true);
    expect(globalThis.fetch).toHaveBeenLastCalledWith("/api/group/testgroup/can-kick-members", {
      headers: { Authorization: "token" },
    });

    globalThis.fetch.mockResolvedValueOnce({ ok: false });
    expect(await api.canKickMembers()).toBe(false);
  });

  it("sends the account auth header on getMyPermissions when an account is logged in", async () => {
    api.setCredentials("testgroup", "token");
    const { accountStorage } = await import("../src/data/account-storage");
    vi.spyOn(accountStorage, "getAccountToken").mockReturnValue("account-token");

    const response = { ok: true, json: vi.fn().mockResolvedValue({ kick_members: true }) };
    globalThis.fetch.mockResolvedValue(response);

    await api.getMyPermissions();

    expect(globalThis.fetch).toHaveBeenCalledWith("/api/group/testgroup/get-my-permissions", {
      headers: { Authorization: "token", "X-Account-Authorization": "account-token" },
    });
  });

  it("sends the account auth header on permission requests when an account is logged in", async () => {
    api.setCredentials("testgroup", "token");
    const { accountStorage } = await import("../src/data/account-storage");
    vi.spyOn(accountStorage, "getAccountToken").mockReturnValue("account-token");

    const response = { ok: true, json: vi.fn().mockResolvedValue({ account_id: 1 }) };
    globalThis.fetch.mockResolvedValue(response);

    await api.getGroupPermissions();
    await api.updateGroupPermissions(1, { kick_members: true });

    expect(globalThis.fetch).toHaveBeenNthCalledWith(1, "/api/group/testgroup/get-group-permissions", {
      headers: { Authorization: "token", "X-Account-Authorization": "account-token" },
    });
    expect(globalThis.fetch).toHaveBeenNthCalledWith(2, "/api/group/testgroup/update-group-permissions", {
      body: JSON.stringify({ account_id: 1, kick_members: true }),
      headers: {
        "Content-Type": "application/json",
        Authorization: "token",
        "X-Account-Authorization": "account-token",
      },
      method: "PUT",
    });
  });

  it("enable waits for data-load events and starts polling once", async () => {
    const waitForAllEventsSpy = vi.spyOn(pubsub, "waitForAllEvents").mockResolvedValue();
    const callOnIntervalSpy = vi.spyOn(utility, "callOnInterval").mockReturnValue(37);

    await api.enable("testgroup", "token");

    expect(waitForAllEventsSpy).toHaveBeenCalledWith("item-data-loaded", "quest-data-loaded");
    expect(callOnIntervalSpy).toHaveBeenCalledWith(expect.any(Function), 1000);
    expect(api.enabled).toBe(true);
    expect(api.groupName).toBe("testgroup");
    expect(api.groupToken).toBe("token");
    expect(api.nextCheck).toBe(new Date(0).toISOString());
  });

  it("disable clears credentials, group caches, and polling interval", async () => {
    const clearIntervalSpy = vi.spyOn(window, "clearInterval");
    api.groupName = "testgroup";
    api.groupToken = "token";
    api.enabled = true;
    api.getGroupInterval = Promise.resolve(99);
    groupData.members = new Map([["Alice", {}]]);
    groupData.groupItems = { 4151: { id: 4151, quantity: 1 } };

    await api.disable();

    expect(clearIntervalSpy).toHaveBeenCalledWith(99);
    expect(api.enabled).toBe(false);
    expect(api.groupName).toBeUndefined();
    expect(api.groupToken).toBeUndefined();
    expect(groupData.members.size).toBe(0);
    expect(groupData.groupItems).toEqual({});
    expect(groupData.filters).toEqual([""]);
  });

  it("getGroupData publishes updated group data after successful fetch", async () => {
    api.setCredentials("testgroup", "token");
    api.nextCheck = "2026-03-30T00:00:00.000Z";

    const payload = [{ name: "Alice" }];
    const responseJson = vi.fn().mockResolvedValue(payload);
    globalThis.fetch.mockResolvedValue({ ok: true, json: responseJson });

    const updateSpy = vi.spyOn(groupData, "update").mockReturnValue(new Date("2026-03-30T00:00:05.000Z"));
    const publishSpy = vi.spyOn(pubsub, "publish");

    await api.getGroupData();

    expect(globalThis.fetch).toHaveBeenCalledWith(
      "/api/group/testgroup/get-group-data?from_time=2026-03-30T00:00:00.000Z",
      {
        headers: { Authorization: "token" },
      }
    );
    expect(updateSpy).toHaveBeenCalledWith(payload);
    expect(api.nextCheck).toBe("2026-03-30T00:00:05.000Z");
    expect(publishSpy).toHaveBeenCalledWith("get-group-data", groupData);
  });

  it("getGroupData handles unauthorized responses by disabling and redirecting", async () => {
    const disableSpy = vi.spyOn(api, "disable").mockResolvedValue();
    const pushStateSpy = vi.spyOn(window.history, "pushState");
    const publishSpy = vi.spyOn(pubsub, "publish");

    globalThis.fetch.mockResolvedValue({ ok: false, status: 401 });

    await api.getGroupData();

    expect(disableSpy).toHaveBeenCalled();
    expect(pushStateSpy).toHaveBeenCalledWith("", "", "/");
    expect(publishSpy).toHaveBeenCalledWith("get-group-data");
  });

  it("getGroupData ignores non-401 fetch errors", async () => {
    const disableSpy = vi.spyOn(api, "disable");
    const publishSpy = vi.spyOn(pubsub, "publish");

    globalThis.fetch.mockResolvedValue({ ok: false, status: 500 });

    await api.getGroupData();

    expect(disableSpy).not.toHaveBeenCalled();
    expect(publishSpy).not.toHaveBeenCalled();
  });

  it("sends expected request shapes for member and auth helper endpoints", async () => {
    api.setCredentials("testgroup", "token");

    const response = { ok: true, json: vi.fn().mockResolvedValue({ enabled: true }) };
    globalThis.fetch.mockResolvedValue(response);

    await api.createGroup("new-group", ["Alice", "Bob"], "captcha-token");
    await api.removeMember("Charlie");
    await api.blockMember("Charlie");
    await api.unblockMember("Charlie");
    await api.amILoggedIn();
    await api.getGePrices();
    await api.getCaptchaEnabled();

    expect(globalThis.fetch).toHaveBeenNthCalledWith(1, "/api/create-group", {
      body: JSON.stringify({
        name: "new-group",
        member_names: ["Alice", "Bob"],
        captcha_response: "captcha-token",
      }),
      headers: { "Content-Type": "application/json" },
      method: "POST",
    });
    expect(globalThis.fetch).toHaveBeenNthCalledWith(2, "/api/group/testgroup/delete-group-member", {
      body: JSON.stringify({ name: "Charlie" }),
      headers: { "Content-Type": "application/json", Authorization: "token" },
      method: "DELETE",
    });
    expect(globalThis.fetch).toHaveBeenNthCalledWith(3, "/api/group/testgroup/block-group-member", {
      body: JSON.stringify({ name: "Charlie" }),
      headers: { "Content-Type": "application/json", Authorization: "token" },
      method: "POST",
    });
    expect(globalThis.fetch).toHaveBeenNthCalledWith(4, "/api/group/testgroup/unblock-group-member", {
      body: JSON.stringify({ name: "Charlie" }),
      headers: { "Content-Type": "application/json", Authorization: "token" },
      method: "POST",
    });
    expect(globalThis.fetch).toHaveBeenNthCalledWith(5, "/api/group/testgroup/am-i-logged-in", {
      headers: { Authorization: "token" },
    });
    expect(globalThis.fetch).toHaveBeenNthCalledWith(6, "/api/ge-prices");
    expect(globalThis.fetch).toHaveBeenNthCalledWith(7, "/api/captcha-enabled");
  });

  it("restart re-enables with existing credentials", async () => {
    api.setCredentials("testgroup", "token");
    const enableSpy = vi.spyOn(api, "enable").mockResolvedValue();

    await api.restart();

    expect(enableSpy).toHaveBeenCalledWith("testgroup", "token");
  });

  describe("chat", () => {
    beforeEach(() => {
      api.setCredentials("iron-team", "secret-token");
    });

    it("getChatMessages fetches against the server-side delivery cursor and returns the parsed list", async () => {
      const messages = [{ messageId: 1, memberName: "Zezima", messageText: "gz", createdAt: "2026-01-01T00:00:00Z" }];
      globalThis.fetch.mockResolvedValueOnce({ ok: true, json: vi.fn().mockResolvedValue(messages) });

      const actual = await api.getChatMessages();

      expect(actual).toEqual(messages);
      expect(globalThis.fetch).toHaveBeenCalledWith("/api/group/iron-team/get-chat-messages", {
        headers: { Authorization: "secret-token" },
      });
    });

    it("getChatMessages sends the account auth header when an account is logged in", async () => {
      const { accountStorage } = await import("../src/data/account-storage");
      vi.spyOn(accountStorage, "getAccountToken").mockReturnValue("account-token");
      globalThis.fetch.mockResolvedValueOnce({ ok: true, json: vi.fn().mockResolvedValue([]) });

      await api.getChatMessages();

      expect(globalThis.fetch).toHaveBeenCalledWith("/api/group/iron-team/get-chat-messages", {
        headers: { Authorization: "secret-token", "X-Account-Authorization": "account-token" },
      });
    });

    it("getChatMessages returns an empty array on a failed response", async () => {
      globalThis.fetch.mockResolvedValueOnce({ ok: false });

      expect(await api.getChatMessages()).toEqual([]);
    });

    it("sendChatMessage posts the trimmed text and account auth headers", async () => {
      globalThis.fetch.mockResolvedValueOnce({ ok: true });

      await api.sendChatMessage("gz on the pet");

      expect(globalThis.fetch).toHaveBeenCalledWith("/api/group/iron-team/send-chat-message", {
        body: JSON.stringify({ text: "gz on the pet" }),
        headers: { "Content-Type": "application/json", Authorization: "secret-token" },
        method: "POST",
      });
    });

    it("chatSocketUrl builds a ws(s) url carrying the group token as a query param", () => {
      expect(api.chatSocketUrl).toBe(
        `ws://${window.location.host}/api/group/iron-team/ws?token=${encodeURIComponent("secret-token")}`
      );
    });

    it("chatSocketUrl is undefined in admin-view mode", () => {
      api.adminView = true;
      api.adminToken = "admin-bearer";
      api.adminViewGroupId = 42;

      expect(api.chatSocketUrl).toBeUndefined();
    });
  });
});
