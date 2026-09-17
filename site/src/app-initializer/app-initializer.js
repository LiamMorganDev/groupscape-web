import { BaseElement } from "../base-element/base-element";
import { Item } from "../data/item";
import { Quest } from "../data/quest";
import { api } from "../data/api";
import { storage } from "../data/storage";
import { adminViewSession } from "../data/admin-view-session";
import { pubsub } from "../data/pubsub";
import { loadingScreenManager } from "../loading-screen/loading-screen-manager";
import { AchievementDiary } from "../data/diaries";
import { toastSource } from "../data/toast-source";
import { combatAchievement } from "../data/combat-achievement";
import { chatStore } from "../data/chat-store";

export class AppInitializer extends BaseElement {
  constructor() {
    super();
  }

  html() {
    return `{{app-initializer.html}}`;
  }

  connectedCallback() {
    super.connectedCallback();
    this.initializeApp();
  }

  disconnectedCallback() {
    super.disconnectedCallback();
    this.cleanup();
    // This only unmounts when navigating out of the whole `/group` wrapper (see `wrap-routes`),
    // never for in-place navigation between dashboard subpages - so leaving here is exactly the
    // "done with this view" signal. Without this, a lingering admin view session would keep
    // outranking a real member's own credentials in `initializeApp` the next time they came back
    // to `/group`, e.g. via account/characters pages, showing them the last group an admin viewed
    // instead of their own.
    adminViewSession.clear();
  }

  cleanup() {
    api.disable();
    toastSource.disable();
    chatStore.disable();
    // Unpublish everything to prevent any data leaking over into another session
    pubsub.unpublishAll();
    loadingScreenManager.hideLoadingScreen();
  }

  async initializeApp() {
    this.cleanup();
    loadingScreenManager.showLoadingScreen();
    await Promise.all([
      Item.loadItems(),
      Item.loadGePrices(),
      Quest.loadQuests(),
      AchievementDiary.loadDiaries(),
      combatAchievement.initCatalog(),
    ]);
    const adminView = adminViewSession.get();
    const group = storage.getGroup();

    // Make sure this component is still connected after loading the above. We don't want to start
    // making requests for group data if the user navigated away before the preload completed.
    if (this.isConnected) {
      if (adminView) {
        await this.loadAdminView(adminView);
      } else {
        // "@EXAMPLE" is a real, seeded, read-only demo group (see server's `seed` binary) -
        // loaded through the normal API path like any other group, not a client-side mock.
        await this.loadGroup(group);
      }

      loadingScreenManager.hideLoadingScreen();
    }
  }

  async loadGroup(group) {
    const firstDataEvent = pubsub.waitUntilNextEvent("get-group-data", false);
    await api.enable(group.groupName, group.groupToken);
    await firstDataEvent;
    toastSource.enable();
    chatStore.enable(group.groupName);
  }

  // A global admin viewing a group's dashboard read-only - see `admin-view-session.js`. Reuses
  // every dashboard page unchanged; `toastSource` (which pushes new-loot/level-up toasts) stays
  // off since this is an observer session, not a member's own.
  async loadAdminView(adminView) {
    const firstDataEvent = pubsub.waitUntilNextEvent("get-group-data", false);
    await api.enableAdminView(adminView.groupId, adminView.groupName, adminView.adminToken);
    await firstDataEvent;
  }
}

customElements.define("app-initializer", AppInitializer);
