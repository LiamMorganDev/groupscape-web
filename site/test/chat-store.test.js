import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { chatStore } from "../src/data/chat-store";
import { chatSocket } from "../src/data/chat-socket";
import { api } from "../src/data/api";
import { pubsub } from "../src/data/pubsub";

describe("chat-store", () => {
  beforeEach(() => {
    chatStore.enabled = false;
    chatStore.messages = [];
    chatStore.groupName = undefined;
    chatStore.myMemberName = undefined;
    chatStore.syncedReadCursor = 0;
    localStorage.clear();
    vi.spyOn(api, "getChatMessages").mockResolvedValue([]);
    vi.spyOn(api, "getMyPermissions").mockResolvedValue({ ok: true, json: async () => ({ member_name: "Zezima" }) });
    vi.spyOn(api, "markChatRead").mockResolvedValue({ messageId: 0 });
    vi.spyOn(chatSocket, "enable").mockImplementation(() => {});
    vi.spyOn(chatSocket, "disable").mockImplementation(() => {});
    pubsub.unpublish("chat-messages");
    pubsub.unpublish("chat-unread-count");
  });

  afterEach(() => {
    chatStore.disable();
    vi.restoreAllMocks();
  });

  it("loads the backfill and normalizes messageText/createdAt onto the client shape", async () => {
    api.getChatMessages.mockResolvedValue([
      { messageId: 1, memberName: "Zezima", messageText: "gz", createdAt: "2026-01-01T00:00:00Z" },
    ]);

    chatStore.enable("Iron Foundry");
    await chatStore.loadBackfill();

    expect(chatStore.messages).toEqual([
      { messageId: 1, memberName: "Zezima", text: "gz", createdAt: "2026-01-01T00:00:00Z" },
    ]);
  });

  it("dedupes a live socket message already present from the backfill", async () => {
    api.getChatMessages.mockResolvedValue([
      { messageId: 1, memberName: "Zezima", messageText: "gz", createdAt: "2026-01-01T00:00:00Z" },
    ]);
    chatStore.enable("Iron Foundry");
    await chatStore.loadBackfill();

    chatStore.handleSocketMessage({ messageId: 1, memberName: "Zezima", text: "gz" });

    expect(chatStore.messages).toHaveLength(1);
  });

  it("appends a new live socket message and republishes chat-messages", async () => {
    chatStore.enable("Iron Foundry");
    await chatStore.loadBackfill();

    const published = [];
    pubsub.subscribe("chat-messages", (messages) => published.push(messages), false);
    chatStore.handleSocketMessage({ messageId: 2, memberName: "Woox", text: "vork trip?" });

    expect(chatStore.messages.map((m) => m.messageId)).toEqual([2]);
    expect(published).toHaveLength(1);
  });

  it("counts every message past the stored last-read id as unread", async () => {
    localStorage.setItem("chat-last-read-message-id:Iron Foundry", "1");
    api.getChatMessages.mockResolvedValue([
      { messageId: 1, memberName: "Zezima", messageText: "gz", createdAt: "2026-01-01T00:00:00Z" },
      { messageId: 2, memberName: "Woox", messageText: "gg", createdAt: "2026-01-01T00:01:00Z" },
      { messageId: 3, memberName: "B0aty", messageText: "nice", createdAt: "2026-01-01T00:02:00Z" },
    ]);

    let unread;
    pubsub.subscribe("chat-unread-count", (count) => (unread = count), false);
    chatStore.enable("Iron Foundry");
    await chatStore.loadBackfill();

    expect(unread).toBe(2);
  });

  it("markRead persists the newest message id and zeroes the unread count", async () => {
    api.getChatMessages.mockResolvedValue([
      { messageId: 1, memberName: "Zezima", messageText: "gz", createdAt: "2026-01-01T00:00:00Z" },
      { messageId: 5, memberName: "Woox", messageText: "gg", createdAt: "2026-01-01T00:01:00Z" },
    ]);
    chatStore.enable("Iron Foundry");
    await chatStore.loadBackfill();

    let unread;
    pubsub.subscribe("chat-unread-count", (count) => (unread = count), false);
    chatStore.markRead();

    expect(unread).toBe(0);
    expect(localStorage.getItem("chat-last-read-message-id:Iron Foundry")).toBe("5");
  });

  it("keys the last-read cursor per group so switching groups doesn't leak unread state", async () => {
    localStorage.setItem("chat-last-read-message-id:Group A", "10");
    api.getChatMessages.mockResolvedValue([
      { messageId: 1, memberName: "Zezima", messageText: "gz", createdAt: "2026-01-01T00:00:00Z" },
    ]);

    let unread;
    pubsub.subscribe("chat-unread-count", (count) => (unread = count), false);
    chatStore.enable("Group B");
    await chatStore.loadBackfill();

    expect(unread).toBe(1);
  });

  it("markRead syncs the newest message id to the server read cursor", async () => {
    api.getChatMessages.mockResolvedValue([
      { messageId: 1, memberName: "Zezima", messageText: "gz", createdAt: "2026-01-01T00:00:00Z" },
      { messageId: 5, memberName: "Woox", messageText: "gg", createdAt: "2026-01-01T00:01:00Z" },
    ]);
    chatStore.enable("Iron Foundry");
    await chatStore.loadBackfill();

    chatStore.markRead();
    await Promise.resolve();

    expect(api.markChatRead).toHaveBeenCalledWith(5);
  });

  it("markRead does not re-sync a message id already synced to the server", async () => {
    api.getChatMessages.mockResolvedValue([
      { messageId: 5, memberName: "Woox", messageText: "gg", createdAt: "2026-01-01T00:01:00Z" },
    ]);
    chatStore.enable("Iron Foundry");
    await chatStore.loadBackfill();

    chatStore.markRead();
    await Promise.resolve();
    chatStore.markRead();
    await Promise.resolve();

    expect(api.markChatRead).toHaveBeenCalledTimes(1);
  });

  it("an incoming chat_read frame from this account's own member clears the unread badge", async () => {
    localStorage.setItem("chat-last-read-message-id:Iron Foundry", "1");
    api.getChatMessages.mockResolvedValue([
      { messageId: 1, memberName: "Zezima", messageText: "gz", createdAt: "2026-01-01T00:00:00Z" },
      { messageId: 2, memberName: "Woox", messageText: "gg", createdAt: "2026-01-01T00:01:00Z" },
    ]);
    chatStore.enable("Iron Foundry");
    await chatStore.loadBackfill();
    await Promise.resolve(); // let resolveMyMemberName's getMyPermissions mock resolve

    let unread;
    pubsub.subscribe("chat-unread-count", (count) => (unread = count), false);
    chatStore.handleSocketRead({ memberName: "Zezima", messageId: 2 });

    expect(unread).toBe(0);
    expect(localStorage.getItem("chat-last-read-message-id:Iron Foundry")).toBe("2");
  });

  it("ignores a chat_read frame for a different group member", async () => {
    localStorage.setItem("chat-last-read-message-id:Iron Foundry", "1");
    api.getChatMessages.mockResolvedValue([
      { messageId: 1, memberName: "Zezima", messageText: "gz", createdAt: "2026-01-01T00:00:00Z" },
      { messageId: 2, memberName: "Woox", messageText: "gg", createdAt: "2026-01-01T00:01:00Z" },
    ]);
    chatStore.enable("Iron Foundry");
    await chatStore.loadBackfill();
    await Promise.resolve();

    chatStore.handleSocketRead({ memberName: "Woox", messageId: 2 });

    expect(localStorage.getItem("chat-last-read-message-id:Iron Foundry")).toBe("1");
  });

  it("disable tears down the socket subscription and clears published state", async () => {
    chatStore.enable("Iron Foundry");
    await chatStore.loadBackfill();

    chatStore.disable();

    expect(chatSocket.disable).toHaveBeenCalled();
    expect(pubsub.getMostRecent("chat-messages")).toBeUndefined();
    expect(pubsub.getMostRecent("chat-unread-count")).toBeUndefined();
  });
});
