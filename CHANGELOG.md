# Changelog

All notable changes to GroupScape web are logged here, newest first.

## [1.0.529] - 2026-09-18

### Added
- Dying to Doom of Mokhaiotl now shows which delve level you died on, in the activity feed, toast, and Discord death notification.

### Fixed
- A claimed Doom of Mokhaiotl run's delve level now actually shows up in the loot log and activity feed - it was silently missing even after last-patch's fix to stop the whole run from disappearing.
- Loot log and activity feed no longer merge two delve runs at different levels into one misleading entry (e.g. a level-1 and a level-4 run showing as one "x2" with only one of the levels) - each level now gets its own entry, even back-to-back.
- Dying to Doom of Mokhaiotl now reads "delve level X" in the activity feed and Discord death notification, matching the kill message's wording instead of the inconsistent "on level X".

## [1.0.504] - 2026-09-17

### Changed
- A Doom of Mokhaiotl run now shows up once in your loot log and activity feed, when you claim rewards and leave, instead of once per level cleared along the way - and it now shows the level you left at.

## [1.0.503] - 2026-09-17

### Added
- Group chat now marks unread messages with a "New" divider line, so it's clear where you left off.
- Group chat now remembers what you've already read across every device and browser tab you're signed into. Read a message on your phone or in one tab, and the "new message" dot clears everywhere else that's open and focused - no more re-reading the same messages on every device.
- Slayer Stats tab now shows your most common boss task and a total count of boss tasks assigned.
- Slayer Stats tab now also shows your total boss task kill count alongside the boss task count.
- Group admins can now delete other members' messages from the group chat.
- Group admins can now clear a group's entire chat history in one click, instead of deleting messages one at a time.
- Maggot King and Shellbane Gryphon boss tasks now show an icon in your Slayer Stats.
- Boss slayer tasks now show a "Boss" badge next to the task name, both on your current task and in your task history.

### Changed
- Chat bubble icon no longer has a square border around its circular background.
- Chat messages now show each sender's helmet icon in their GroupScape color next to their name, not just colored text.
- Internal: the OSRS data-refresh tooling can now also detect achievement diary task changes and new slayer task types straight from the wiki, and tracks partial drop-rate coverage for a future cleanup pass.
- Internal: chat messages older than a week are now automatically deleted server-side instead of being kept indefinitely.
- Slayer Stats tab now combines the boss tasks and boss task kills counts into a single tile instead of two separate ones.

### Fixed
- Sending a group chat message (or having one arrive while the chat window is already open) no longer briefly shows a "New" divider above it in that same window - the divider only ever marks messages that were actually unread when you opened the window.
- Group chat now always shows your full message history from the past week when you open it, instead of sometimes appearing empty after a refresh even though nothing was actually deleted.
- Group chat history now catches up correctly no matter which device or browser tab you're using - previously each one tracked "what you've already seen" separately, so switching devices could re-show or skip recent messages; that's now tracked on the server, per account.
- Group chat bubble and window are now clickable on the map page - they used to be completely unresponsive there (couldn't open, close, or send a message) since the map view was silently swallowing clicks meant for it.
- Group chat window can actually be closed again - the X button and the chat bubble icon had stopped closing it on every page, not just the map.
- Barrows Brothers, Maggot King, and Shellbane Gryphon boss tasks now correctly count toward your Slayer Stats boss task totals - they were being tracked but silently excluded from the stats.
- Group chat now actually updates live - new messages, deletions, and read receipts from other tabs and group members used to only show up after a page refresh, since the site's WebSocket connection was silently never reaching the server.
- Maggot King and Shellbane Gryphon slayer tasks now actually show their icon - the artwork shipped earlier but was never wired up, so they were still falling back to the generic unknown-task icon.
- Group admins deleting a chat message no longer occasionally get an error and fail to delete it.
- Group chat no longer shows "No messages yet" on top of your actual message history.
- Group chat history loads again for logged-in accounts - it had started failing with an authorization error after the per-account read-tracking change, so messages only ever showed up over the live connection, never on page load.
- Group chat's live connection actually connects now - a production-only URL-building bug meant the browser was trying to reach a garbled, nonexistent address for the live feed, so nothing (including messages sent from RuneLite) ever showed up without a page refresh.

### Removed
- Tumeken's Warden kills no longer clutter the Loot Log - like the other Tombs of Amascut boss rooms, its loot now only shows up in the raid's overall reward.

## [1.0.502] - 2026-09-17

### Added
- Group chat is now available on the website: a chat bubble in the corner of every page opens a live chat window with your group. It shows your recent chat history, updates in real time, and lets you search through past messages. A red badge (and a marker on your browser tab) lets you know when there's something new to read.

## [1.0.498] - 2026-09-17

### Added
- Backend groundwork for group chat (not yet visible in the app - a chat tab is coming soon).

### Changed
- Updated item names and icons for the Moon armour sets, Barrows sigils, Necklace of Fangs, and Robin Hood hat to match the OSRS Wiki's current capitalization.

## [1.0.496] - 2026-09-16

### Fixed
- The "Aberrant spectres" Slayer task now links to its correct OSRS Wiki page instead of a broken link.

## [1.0.495] - 2026-09-14

### Fixed
- Slayer tasks assigned by Turael, Aya, or Spria could show as "Reset" with 0 kills in the Slayer History tab even when you'd fully completed them - this happened whenever your next task also came from one of those masters, since they're your normal early-game masters too, not just a free-skip source. Completed tasks are now correctly recorded even when the following assignment came from the same type of master.

## [1.0.491] - 2026-09-13

### Changed
- Corrupted unicorn and crystalline unicorn kills no longer clutter the Loot Log.

### Fixed
- Mortimer's task modifier badge could keep showing on your next slayer task, even after finishing it or switching to a completely different slayer master. It's now only ever shown while your current task is actually assigned by Mortimer.
- Switching devices mid-task could sometimes create duplicate "Completed" entries for the same task in the Slayer History tab. Resuming the same task on a different device is now correctly recognized as a continuation instead of a new completion.

## [1.0.486] - 2026-09-12

### Added
- Mortimer slayer tasks now show which bonus modifier applies (points, task size, clue rate, superior unique rate, or Slayer XP) as an icon next to the task, both on the Current tab and in the History list.
- Slayer Stats tab now has two new tiles: your most common Mortimer modifier, and your fastest-completed task (down to the exact second).

### Fixed
- Switching devices mid-task (e.g. PC to mobile) no longer strands that task as "Superseded" in the Slayer History tab - it's now recorded as completed with its real kill count.
- Blocking or cancelling a slayer task right after finishing its kill count, but before turning it in, no longer shows up as "Completed" in the History tab with the fee displayed as if it were that task's own reward - it's now correctly labeled Blocked/Cancelled, and a few already-affected rows have been corrected.
- Notable drop chat notifications now show item and total gp values with comma separators (e.g. "20,000 gp") instead of unformatted numbers.
- Discord drop notifications now respect the group's configured minimum drop value again - they were slipping through at any value crossing a member's own plugin notification threshold, regardless of the group setting.

## [1.0.476] - 2026-09-11

### Added
- Loot Log search now understands dates: type "today", "yesterday", "this week", "this month", "last 3 days", a specific date like "2026-05-05", or a range like "2026-05-01..2026-05-05" - combine with other search terms too, e.g. "tormented demons and today".

### Fixed
- Collection log Discord/activity feed notifications now show the correct page progress (e.g. "3/5") instead of always undercounting to just the newly obtained item.
- Legacy Bounty Hunter (Hunter/Rogue) hiscores stats now show an icon instead of a placeholder.
- Lunar Chests now shows its proper icon instead of a placeholder.
- Skipping a task from a higher-level master via Turael/Aya/Spria no longer shows up as "Superseded" in the Slayer History tab - it's now correctly labeled "Reset".
- Restarting your client/plugin mid-task no longer freezes that task's progress at 0 in the Slayer History and Stats tabs until you re-talk to your slayer master - progress now updates live like the Current tab always did.
- Konar quo Maten's slayer master portrait now shows the correct chathead instead of a mismatched image.

[1.0.460] — Slayer History Overhaul, Activity Feed Reactions, Wiki Links

## [1.0.457] - 2026-09-09

### Added
- Activity feed items can now be liked and commented on. Press and hold the Like button to pick from 5 reactions - Like, GG, LOL, Rare!, and F. Comments are capped at 10 per item.
- Group admins can turn likes/comments on or off for the whole group from Group Settings (on by default).

### Fixed
- The Like and comment buttons on activity feed items were showing as blank shapes with no visible icon. They now use the game's proper button style and show clear icons.
- The reaction picker's popup could stay visible even when you weren't pressing the Like button.
- The 5 reaction icons all looked like the same dark blob at small size. Each now has its own distinct color so they're easy to tell apart.

## [1.0.449] - 2026-09-08

### Changed
- Crystalline/Corrupted Scorpion kills from the Gauntlet no longer clutter the Loot Log, matching the other Gauntlet trash mobs already filtered out.

### Fixed
- Loot Log value searches like `>100k` were only matching a single kill's own loot, so a big farming session made of many smaller kills (e.g. 19 kills worth 141k combined) could vanish from the results even though its total clearly cleared the bar. Now matches if either a single kill or the whole session's combined value clears it.
- The Equipment tab on mobile was cutting off the attack/defence bonus stats entirely instead of showing them. It now splits into Gear and Stats tabs so both fit on screen.

### Changed
- Internal: commits now require a CHANGELOG.md entry - a pre-commit hook rejects a commit that doesn't stage one.
- Internal: Loot Log's value-clause matching (`>100k` etc.) now ignores an item's gp value if that item isn't one the client can actually render, so a session card can't clear a value search using gp from an item that doesn't visibly appear on it. Search debounce also increased from 300ms to 1s to reduce firing mid-multi-word-query.

## [1.0.444] - 2026-09-08

### Added
- Loot Log search now understands `&&` (or the word "and") to require multiple conditions at once, e.g. `vorkath && >1m` finds only Vorkath kills worth over a million - previously all search words were just "match any of these", so there was no way to combine a name with a value filter.
- Loot Log search can now filter by kill count too, e.g. `vorkath && >10kc` (or `>10 kills`) only shows farming sessions where you've killed Vorkath more than 10 times in a row.
- Loot Log search now also understands `unique` (only unique drops), `rare` (rare or rarer), a drop-rate check like `<1/5000` (rarer than 1 in 5000), and `kill`/`chest`/`clue` to filter by drop type generically, e.g. `chest && >1m`.

### Changed
- Internal: corrected stale doc comments referencing the plugin's slayer task event upload key.

### Fixed
- A Mutated Zygomites slayer task now shows its proper monster icon instead of a blank/unknown icon.

## [1.0.434] - 2026-09-08

### Added
- Each member's Slayer panel now has History and Stats sub-tabs alongside the current task: History lists every past task (filterable by status or slayer master, with masters ordered by progression and grouped with their stand-ins - Nieve/Steve, Duradel/Kuradal, etc.) with points earned/spent and kill progress, while Stats shows all-time totals and "most killed", "most common", and "most cancelled" records. Task and master icons now render in a ringed medallion frame.

### Changed
- The current task's monster icon and slayer master portrait now sit in the same ringed medallion frame as History/Stats, so they stand out against the panel background instead of blending into it.

### Fixed
- Loot Log search could show a stale batch of unfiltered kills after typing a new search (e.g. groups worth far under the searched value would slip in under a value filter like ">1m"). A value/quantity search now also matches a kill's combined loot value, not just individual item stacks.
- Moons of Peril kill entries in the activity feed and toast notifications no longer merge two different moon combos into one misleading "x2" - each combo (e.g. "Eclipse, Blue, Blood" vs. just "Blue") now shows as its own entry with its own loot.
- Skill level-ups (activity feed entry, toast, and Discord notification) were silently missing for every skill's higher levels - the server was using the wrong XP thresholds and thought you were still one level behind where you actually were. Fixed so level-up milestones (10, 20, ..., 99) now fire at the correct level for everyone.

## [1.0.421] - 2026-09-07

### Added
- Each group member's panel now has a "Boss KC" tab showing their real, live OSRS hiscores kill counts for every boss, clue scroll tier, and minigame - not GroupScape's own tracked counts, but the account's actual lifetime totals straight from the official hiscores.
- Opening the Barrows chest or the Moons of Peril reward chest now logs a single activity feed entry naming exactly which brothers/moons you defeated that run, e.g. "Barrows (Dharok, Verac, Torag)" or "Moons of Peril (Eclipse, Blood)" - Moons of Peril kills weren't shown anywhere before this.
- GroupScape can now be installed as an app on your phone or desktop, with an install prompt for Android/desktop and a manual "Add to Home Screen" tip on iPhone/iPad. Once installed, the app shell keeps working when you lose connection, with a banner letting you know your data may be out of date.

### Changed
- Individual Barrows brother and Moon kills no longer show up on their own in the activity feed, toast notifications, or Discord kill webhook - only the combined chest-opening entry does now.
- The installed app now refreshes itself automatically in the background whenever a new version of GroupScape ships.
- The Set Bonus dialog's "Other Sets" list is now split into collapsible Combat and Skilling groups, so it's faster to find the set you're looking for.
- The member panel's tab bar is now split across two rows so there's room for the new Boss KC tab without crowding the rest, grouped so Inventory/Equipment/Skills/Slayer sit up top and Quests/Diaries/Collection Log/Combat Achievements/Boss KC sit below.
- The Boss KC tab icon is now Vorkath's head instead of Zulrah's fang.
- The Boss KC tab icon is now a trophy, matching the icon RuneLite uses for its own hiscores panel.
- Hovering a boss/clue/minigame in the Boss KC tab now shows its name and kill count, and clicking it opens the OSRS Wiki page for it in a new tab.
- On mobile, the side panel toggle button moved to the bottom-left corner (above the Discord button). The admin dashboard shortcut button moved from the top-right to the bottom-right corner.
- Corrupted Gauntlet trash mobs (corrupted/crystalline wolves, spiders, bats, rats, dragons, bears, dark beasts) and Tombs of Amascut's Ba-Ba, Akkha, Kephri, and Zebak no longer clutter the Loot Log.

[1.0.409] — Richer Discord Notifications, Rare Drop Callouts, Wiki Links

## [1.0.392] - 2026-09-05

### Added
- Site admins viewing a group can now select individual activity feed events and delete them for everyone in the group.
- Each Discord notification type in group settings now has a "Test" button that sends a realistic example message, so you can check the channel/formatting/icons without waiting for the real thing to happen in-game.

### Fixed
- Discord "Unique drops only" no longer always flags brimstone keys, dragon med helms, ancient shards, or dark totem pieces as unique - those also drop from common non-boss sources, so they were spamming notifications outside the curated boss list.
- Discord "Drops" notifications were posting untradeable junk drops (e.g. ensouled heads) from regular monsters even with a minimum value set - untradeable drops now count as 0gp against that minimum instead of always getting through.
- Opening your collection log to browse it in-game could previously overwrite items GroupScape had already tracked for you; browsing now merges into what's already recorded instead of replacing it.
- Site admin activity feed delete was failing with a server error instead of deleting the selected events.
- A group member's current target no longer stays stuck showing whoever they were last fighting after they've actually stopped interacting with anything.
- Discord collection log notifications were posting a garbled double-link instead of a clean item name/link.
- Site admin activity feed delete now also invalidates the Loot Log cache, so a deleted kill/loot entry no longer keeps showing there after being removed from the activity feed.
- Discord drop/pet/collection-log notifications could show the wrong item image (e.g. a totem piece showing up as coins) - thumbnails now link straight to the wiki's own image instead of a locally cached one that was wrong for some untradeable items.
- Discord item thumbnails now use a verified wiki image lookup for 13,000+ items instead of guessing the filename, catching cases the guess would've missed (e.g. coins linking to the wrong denomination's icon).
- Discord "Drops" notifications could show a thumbnail for an item that wasn't even mentioned in the message (e.g. showing Bones when the text only named an untradeable drop) - the icon now always matches an item named in the message.
- Admin accounts page was failing to load ("Failed to load accounts") because the new "Last visit" column's database migration was never actually applied.

### Changed
- Internal: the server now logs why it rejected a malformed request from the plugin instead of just returning a 400 with no trace of the reason.
- Internal: sped up production deploys — backend Docker builds now reuse compiled dependencies instead of recompiling all of them on every push, and the backend/frontend images build in parallel with unchanged ones skipped entirely; fixed a follow-up bug where the deploy monitor couldn't tell a parallel build apart from a stalled one.
- Internal: added scripts to detect new OSRS quests and combat achievement tasks against the wiki, so the data-refresh workflow can catch these the same way it already does for items and bosses.
- Discord "Drops" notifications no longer spam a new message per drop - a repeat of the same single item from the same source within an hour now edits the previous drop message's quantity/value in place instead, the same way kill/death notifications already do.
- Group settings' "Unique drops only" option now also flags a curated list of items (Brimstone key, 3rd Age gear, Draconic visage, champion's scrolls, and more) as unique regardless of which NPC dropped them, since they're notable enough on their own even outside the curated boss list.
- Admin panel's account list/detail now shows "Last visit" (last time the account was active on the site) instead of "Last login" (which only reflected the last password sign-in).

## [1.0.383] - 2026-09-04

### Changed
- Discord boss kill and death notifications no longer spam a new message per kill/death - a repeat kill or death on the same boss within an hour now edits the previous message's count in place instead, including deaths with no identified killer.
- Discord collection log notifications now post for every new item you unlock, not just when you finish a whole page.

### Added
- Group settings' Discord Drops notification now has a "Unique drops only" option - turn it on and only drops that are actually unique to that boss/source get posted, ignoring the minimum-value threshold entirely.
- Pets and clue caskets now get their own Discord notifications instead of riding the generic Drops post, each with its own on/off toggle in group settings.
- Discord notifications for kills, diaries, quests, collection log, and raids now show an icon and link straight to the relevant Old School RuneScape Wiki page; kills and boss kill counts are now shown as proper fields instead of run-on text.
- Discord can now post skill level-up notifications, with a configurable "post every 1/5/10 levels" interval in group settings (level 99 always posts).

### Fixed
- Cockatrice slayer tasks now show the cockatrice icon instead of an unknown-task placeholder, plus the same fix for any other task whose singular/plural name didn't match our icon list.
- Loot log item grid now wraps to fit narrow screens instead of overflowing off the edge on mobile.
- Combat Achievements page is usable on mobile now - member rows collapse into cards instead of a squeezed, unreadable table.

## [1.0.376] - 2026-09-04

### Fixed
- Regular, Mortimer, and Wilderness slayer streaks now actually keep updating as you complete tasks, instead of getting stuck at whatever value they first showed.
- Discord boss kill notifications now fire for every tracked boss, not just the ~30 with a curated drop table - bosses like Corporeal Beast, TzTok-Jad, Skotizo, and Duke Sucellus were being silently skipped even with kill notifications turned on.
- The same live overlay/websocket kill event had the same gap - fixed to match.
- Fixed several boss drops in the Loot Log showing the wrong name or icon (e.g. Theatre of Blood, Chambers of Xeric, Tempoross, and Nightmare of Ashihama pets were pointing at the wrong item).

### Added
- Added Loot Log rarity data for 41 more bosses that previously had none, including Corporeal Beast, TzTok-Jad, TzKal-Zuk, Duke Sucellus, The Leviathan, The Whisperer, Vardorvis, Araxxor, Yama, the Royal Titans, the Blood/Blue/Eclipse Moons, and more.

### Changed
- Discord combat achievement notifications now fire for every completed combat task (not just full-boss completions), and every notification links straight to the task's or boss's Old School RuneScape Wiki page.

## [1.0.351] - 2026-09-03

### Added
- Discord kill, drop, and quest completion notifications now show a matching boss/item/quest icon.
- Loot Log session headers (monster, clue tier, chest/raid names) are now clickable and link to the matching Old School RuneScape Wiki page.
- Loot Log now has filter pills (Bosses, Other kills, Chests, Clues) above the search bar, all on by default - toggle them to narrow the list and totals to just what you want to see. Your selection is remembered next time you visit.
- Boss slayer tasks now show that boss's own icon in the Slayer task panel instead of a generic unknown-task icon - covers every boss assignable via the "Boss" task, not just the Desert Treasure II bosses.
- A handful of new items from today's OSRS update now show up correctly with names and icons (the elemental diamond/amulet set, Necklace of fangs).
- Added support for tracking kills of 36 more bosses and quest bosses in the activity feed and Loot Log's boss filter (including Brutus and Demonic Brutus), and backfilled combat levels for 26 already-tracked bosses that were missing one.
- Toast notifications now show a small countdown bar and auto-dismiss after 5 minutes; a kill toast that gets updated with another kill resets its timer instead of expiring mid-streak.
- Slayer panel now shows your regular, Mortimer, and Wilderness task streaks side by side, instead of just whichever one matches your current task - the other two no longer disappear when you switch masters.
- Slayer master is now shown as a clickable portrait that links to their Old School RuneScape Wiki page, including 4 masters that weren't shown before (Mortimer, Aya, Achtryn, Kuradal).
- Added icons for dozens of slayer tasks (cows, greater demons, revenants, black dragons, and many more) that were showing a generic unknown-task icon.
- Slayer task panel now shows a "Task complete" banner and green progress bar once you've killed enough for your current task, so you know to head back to your slayer master.

### Changed
- The Slayer task panel's task icon and name are now part of one clickable card that links to the Old School RuneScape Wiki guide, instead of just a small text link.
- The Slayer task button now opens a docked panel below the minibar, matching how the bag and stats panels behave, instead of a separate floating popover.
- The "Join our Discord" button now sits in the bottom-left corner instead of bottom-right.
- Internal: removed unused CI workflows for Docker image publishing and cache validation - deploy already builds images locally on the server, these weren't part of the real pipeline.
- Internal: added an automated pipeline that pulls item and boss data from the OSRS Wiki, plus a weekly scheduled check, so item names/icons/alch values stay current after game updates instead of drifting.
- Admin "Clear activity feed" and "Clear loot log" buttons are now a single "Clear activity & loot logs" button, since both cleared the same underlying log data.

### Fixed
- Corrected outdated high alchemy values (and a couple of stale names) across several hundred items.
- Clue scroll toast notifications now show the proper scroll icon and tier color instead of a plain bullet.
- The toast stack's "Clear all" button no longer gets pushed off-screen when a lot of notifications pile up - the list now scrolls within the screen instead.
- The Loot Log now actually shows two farming-session cards side by side on wide screens instead of leaving empty space next to a single column.
- Slayer task panel no longer shows a literal "null" for the assigning master when that name hasn't been captured yet - it's just omitted.
- Slayer task progress, kill count, and streaks now update live instead of only refreshing when you close and reopen the panel.
- Slayer task progress now shows kills done so far out of the total (e.g. 2/45) instead of kills remaining out of the total.
- Slayer task panel no longer shows a literal "null" for the task name when it occasionally fails to resolve - the name and wiki link are omitted instead.
- Activity feed and toast notifications no longer merge kills from before and after a death into one streak - a death now starts a fresh kill count for that boss.
- Linking a character to a group no longer floods the activity feed with every level-up milestone it already had - milestones now only post for progress made after linking.
- Added a backstop against the activity feed getting flooded with a member's entire level-up history in one burst - a heartbeat that would post an implausible number of milestones at once is now dropped and that member's skill tracking quietly restarts from their current levels instead.

### Removed
- Removed GP Earned, Boss KC, and Raid Completions from the Graphs page - it's XP-only now (kill/loot/raid tracking elsewhere in the app is unaffected).

[1.0.350] — Broaden Activity Feed Empty-State Message

## [1.0.329] - 2026-09-02

### Added
- The Activity Feed and toast notifications now post a Level Up milestone whenever a group member reaches a notable skill level - every 10 levels up to 80, every 5 up to 95, then every level from 96 to 99 - each with its own skill-flavored message and icon, and 99 called out as a maxed skill.
- Internal: site admins can now clear a group's activity feed or loot log, and reset individual members' collection log, combat achievements, skill/XP history, or bank value history, from the admin panel.
- Each group member's side panel now has a Slayer button - click it to see their current task with a monster icon, their assigned master with a portrait, remaining progress, streak, points, and a link to that task's wiki guide (or just their streak and points if they're between tasks).

### Changed
- Loot Log now loads much bigger chunks of history per scroll and auto-loads further before asking you to click "Load more" again, so scrolling through a group's full loot history takes far fewer steps.
- The item icon shown next to a Collection Log completion in the Activity Feed is now bigger and easier to see.

### Fixed
- Fixed Level Up milestones sometimes getting skipped in the Activity Feed when a skill jumped past more than one milestone in a single update (e.g. a big XP gain crossing both level 80 and 85 at once now posts both, not just the higher one).
- Fixed the Graphs tab sometimes showing the wrong group's leaderboard numbers by making sure it waits for your group's data to be ready before loading them.
- Fixed the Activity Feed and Loot Log tabs sometimes going permanently stuck (no more loading, scrolling, or updates) after you'd navigated away from them once and come back.
- Fixed Loot Log session cards sometimes growing more loot after they'd already loaded, when scrolling further back added drops to a session you'd already seen.
- Loot Log's "load more" indicator no longer lingers forever near the bottom of a long history full of drop-less kills - it now gives up after a few empty attempts instead of implying there's always more to find.
- Fixed a case where a very long, uninterrupted farming session in the Loot Log could keep quietly adding loot to a session card you'd already scrolled past, instead of that card staying as you first saw it.
- Internal: fixed the new admin "Data management" member checkboxes not rendering.
- Royal Titans kills and loot now show up in the Activity Feed - they were silently being filtered out.
- Fixed the Boss KC graph on the Graphs tab showing a flat, wrong line instead of your group's actual kill counts.
- Fixed Loot Log kill sessions sometimes wrongly merging an old, late-arriving batch of kills in with a fresh session, making the count and time range for that boss look wrong.
- Royal Titans now has its boss icon everywhere it's shown, instead of a blank spot.
- Eldric the Ice King and Branda the Fire Queen (the two Royal Titans) now show a boss icon in the Activity Feed and Loot Log instead of a blank spot.
- Artio, Calvar'ion, Chaos Fanatic, Crazy Archaeologist, Deranged Archaeologist, Doom of Mokhaiotl, Hespori, Scurrius, Sol Heredit, Mad Angel, Maggot King, Shellbane Gryphon, The Mimic, and the Gauntlet/Corrupted Gauntlet bosses now show up in the Activity Feed with their boss icon - their kills and loot were being silently filtered out before.
- Spindel, the Royal Titans, Phosani's Nightmare, and Moons of Peril (Blood Moon, Blue Moon, Eclipse Moon) now show up in the Activity Feed with their boss icon instead of being silently filtered out.
- Fixed Royal Titans' boss icon not showing up anywhere on the site - it was never added despite earlier attempts to fix it.
- Corrected Doom of Mokhaiotl's combat level shown on its boss card - it was wrong.

## [1.0.299] - 2026-09-02

### Added
- Loot Log now has a single search box that finds drops by player, monster, item name, item value, quantity, or even monster combat level - try things like ">1m", "<500k", or a boss name. It loads more results automatically as you scroll instead of being limited to a fixed time window.
- Activity Feed entries now show the member's hue-coloured helmet icon in front of their name, matching the icon used on the map and side panels.

### Changed
- Loot Log entries older than 28 days are now automatically cleared out to keep things tidy.
- Loot Log now groups drops into farming sessions per player (kills within 45 minutes of each other), showing when each session started and ended, instead of lumping a boss's entire history into one block.
- Searching the Loot Log highlights the matching item in a session and fades the rest, making it easy to spot what you searched for.
- Health and prayer bars now stretch to fill the wider panel when you have the equipment tab open, instead of stopping short of the gear/stat columns.
- The Set Bonuses dialog now groups sets into Active, Partial, and a collapsed "Other Sets" list instead of one flat list, and set names link out to their OSRS Wiki page.
- Set Bonuses now tracks Shayzien armour (tier 5, full lizardman shaman poison reduction) and no longer lists Ferocious gloves, which was never a real set effect.
- Set Bonuses now covers every skilling outfit (Graceful, Carpenter's, Farmer's, Pyromancer, Angler's, Prospector, Zealot's, Raiments of the Eye, Smiths', Rogue, Lumberjack) as well as Bloodbark and Swampbark armour, on top of the combat sets it already tracked.
- Set Bonuses now shows each set's OSRS Wiki equipped-character image next to its entry, including all 6 Barrows sets - click a thumbnail to open that set's wiki page.
- Loot Log now shows two session cards side by side on wide screens instead of one long column, collapsing back to one per row on narrower screens.
- Leaderboards, the Graphs tab, session history, and the Loot Log's totals bar now load noticeably faster on a group with a lot of activity.

### Fixed
- Fixed uneven spacing around the item icon in Activity Feed collection log entries.
- Loot Log (and Activity Feed) could get stuck showing only the first page of results with the loading spinner spinning forever, or silently load an inconsistent amount depending on the page - scrolling now reliably keeps loading further results.
- Loot Log item icons within a session now line up in a fixed 10-per-row grid instead of reflowing unevenly based on available width.
- Loot Log's total gp value now sits directly above the item icons instead of floating off to the right of a wide panel.
- Fixed the demo group (/demo) failing to (re)seed on every deploy and scheduled refresh, which could leave it stuck unavailable.
- Fixed a rare case where scrolling the Loot Log (or Activity Feed) to load more could lock up the whole tab.
- Fixed Loot Log and Activity Feed sometimes loading nothing and getting stuck with a "Load more" that never worked, if you refreshed directly on that page.
- Loot Log and Activity Feed no longer show an endlessly-spinning loading icon once a page has actually finished loading - it only spins while genuinely fetching more, and sits still the rest of the time.

### Removed
- Removed the Loot Log's time window, boss, and member filter dropdowns - the new search box covers all of that in one place.

### Fixed
- Activity Feed collection log entries now show the item's own icon in front of its name, matching the "icon, item name, log book icon" order instead of stacking both icons after the name.
- Fixed the collection log item icon rendering oversized instead of at the same 18px size as the log book icon next to it.
- The "Boss kill" in-game chat notification no longer fires for every NPC kill (including slayer task mobs) - it's now limited to actual tracked bosses, matching what the setting says it does.
- Set Bonuses now defaults "Other Sets" open when you have no active or partial sets, instead of hiding everything behind a collapsed accordion.
- Set Bonuses is now scrollable again when its contents overflow the dialog, including when the "Other Sets" list is long.
- Set Bonuses' wiki links for the 6 Barrows sets and 3 Moon sets now go to the actual equipment page instead of the Grand Exchange trading-bundle page, which doesn't describe the set effect at all.

## [1.0.257] - 2026-09-01

### Added
- Activity Feed now posts a standalone announcement when a member finishes every combat achievement task for a specific boss, the same way finishing a collection log page already does.
- Group Settings now has a Discord Integration section — paste in a channel webhook URL and GroupScape can post your group's kills, deaths, loot, notable drops, and raid completions straight to Discord, with a toggle for each one. Saving sends a live test message first, so a bad or deleted webhook is caught immediately instead of silently failing later.
- Discord boss kill notifications now show the total GE value of what the boss dropped, e.g. "killed Zulrah (KC: 45) — loot worth 1,204,300 gp".
- The Activity Feed now shows raid completions for Chambers of Xeric, Theatre of Blood, and Tombs of Amascut, including who finished it, the difficulty (invocation level for Tombs of Amascut, mode for the other two), and how much the reward chest was worth. If several of you finish the same raid together, it shows up as one entry crediting everyone instead of one per person. Raids also get their own filter chip and toast notification, and Tombs of Amascut shows a different icon depending on the invocation level reached.
- Graphs now has a "Raid Completions" option alongside XP and Boss KC, showing your group's Chambers of Xeric, Theatre of Blood, and Tombs of Amascut completions over time. Filter by raid and (for CoX/ToA) by difficulty, and switch between one combined group line or a line per member - the leaderboard panel ranks members by how many of those completions they were part of.
- Loot Log tiles now show a small helmet badge (tinted to each member's color) for who got the drop when "All members" is selected. An item dropped by more than one member is combined into a single tile with a stacked badge and a per-member breakdown in the tooltip, instead of one duplicate tile per member. Boss kill sources also get their chathead icon next to the name.
- Loot Log summary now shows a session card - the time span from your first kill to your last within the selected window, plus the duration.
- Each Loot Log source now shows its own first-kill-to-last-kill time range next to the kill count, so a boss farmed across several days shows that span instead of just a total.
- Clicking a Loot Log item now opens its OSRS Wiki page in a new tab.
- Loot Log boss names now show their combat level (e.g. "Vorkath (level-732)"), matching the in-game overhead format, for bosses with one fixed level.
- Group Settings' Discord Integration section now has toggles for Combat achievements, Collection log, Quest completions, and Diary completions, so those milestones can post to Discord too.

### Changed
- Activity Feed kill entries now show the boss's icon next to its name, and repeated kills of the same boss by the same member within an hour merge into one entry with a running count (e.g. "killed Vorkath ×3") instead of listing each kill separately - the same merging applies to a kill toast that's still on screen.
- Loot Log now groups drops by boss, chest, or clue tier under a square icon grid instead of a flat list, with a rarity-colored border on each item and a hover tooltip showing its name and value. Each source shows its own total value alongside a kill/open/casket count. Loot splitting and the per-person breakdown have been removed - Loot Log is just a log now.
- Loot Log item art now has a lot more breathing room inside its rarity border instead of crowding the edge.
- The Loot Log time window's "Last day" option is now labeled "Last 24h".
- Activity Feed entries for new collection log items now show the item's own icon next to the collection log icon, and link out to that item's OSRS Wiki page.
- Activity Feed's left-hand rail now filters by activity type (kills, raids, collection log, clues, etc.) and the pills at the top now filter by member - swapped from before, where it was the other way around.
- Discord's "Loot" and "Notable drops" toggles are now a single "Drops" toggle with an adjustable minimum value (250k gp by default) - only drops worth at least that amount get posted.
- Discord's "Kills" notification is now "Boss kills" and only fires for actual bosses, not regular NPCs, and now includes the member's kill count for that boss.
- Discord kill and drop messages now show the item's real name, linked to its OSRS Wiki page, instead of "item #12345".
- Loot Log now defaults to the last 24h instead of the last hour when you open it.
- Discord "Drops" messages now show each item's value (or "untradeable" if it has none) and its drop rate when known, and value now reflects the whole stack - 33x a 100gp item shows as 3,300 gp, not 100 gp.
- Loot Log's Source filter now only lists sources you've actually looted in the selected date range, instead of every boss and chest in the game - picking a different date range updates the list, and it's empty if nothing was looted in that range.
- Internal: fixed the commit workflow doc so it can't cause the version number to double-bump on a single commit.

### Fixed
- Loot Log items dropped by a boss with no curated rarity data no longer show up as "Item #12345" - the real item name is shown.
- Loot Log items GroupScape doesn't recognize (no name, image, or value on file) no longer render as broken empty tiles - they're hidden instead.
- Loot Log items from the same kill no longer reshuffle their order every time the page refreshes.
- Coin drops in the Loot Log no longer value at 0gp.
- Loot Log boss, chest, and clue-tier icons now use the same small square icon as RuneLite's Hiscore panel (and the official OSRS hiscores page), self-hosted instead of hotlinked so they always load.
- Test/diagnostic kill data no longer shows up in the Loot Log.
- Untradeable items in the Loot Log now show an "Untradeable" tag in their tooltip instead of "0 gp x quantity".
- The "Exit view" button on the admin read-only viewing banner no longer fails to click while looking at the Map tab.
- Activity Feed's type and member filter counts no longer zero out every other option when you pick a filter - they now show real counts for each choice regardless of what's currently selected.
- The Discord notify toggles in Group Settings were invisible and couldn't be clicked at all - fixed.
- Kills, loot, and deaths could occasionally show up twice in the Activity Feed and get posted twice to Discord, most often right around a server restart - fixed.
- Activity Feed no longer gets stuck endlessly spinning for a group with a long same-boss farming streak - when merging repeat kills keeps pulling in more history without adding anything new to see, it now pauses and shows a "Load more" button instead of spinning forever.
- Activity Feed no longer breaks entirely (spinner stuck forever) when a kill's loot includes an item GroupScape doesn't recognize yet - that item is now just skipped instead of crashing the whole feed.
- Activity Feed for an account with a long, near-continuous kill history against the same boss no longer keeps auto-loading page after page trying to catch up - it now stops after a handful of pages and shows a "Load more" button instead of spinning indefinitely.

## [1.0.254] - 2026-08-31

### Fixed
- Loot Log was always empty - every kill report from the plugin was silently rejected by the server, so no kill or loot ever got recorded. Kills (and their loot) now save properly.

## [1.0.253] - 2026-08-31

### Added
- Graphs now has 1H, 6H, and 12H time period options, alongside the existing 24H/7D/30D/1Y.

### Changed
- Simplified the top nav's account menu into a plain divider next to the tab row, instead of a floating panel — fixes it occasionally overlapping page content on Activity Feed, Combat Achievements, and Loot Log.

## [1.0.248] - 2026-08-31

### Changed
- GP Earned now tracks your total wealth - bank, inventory, and worn gear combined - instead of just your bank, so it shows a true day-to-day gp gain or loss.

### Fixed
- Boss KC now actually records kills. Previously most boss kills, especially ones with longer death animations, were never counted.

### Removed
- The Loot Value graph and leaderboard option, which never had any data, have been removed.

## [1.0.246] - 2026-08-31

### Added
- Raid markers on the map now include four numbered (1-4) and four lettered (A-D) generic callouts, alongside Danger/Defend/Loot/Focus.

### Changed
- Renamed the "Safe Spot" raid marker to "Defend".

[1.0.245] — Fix Menu Overlap On Activity Feed, Loot Log, Graphs Leaderboard

## [1.0.243] - 2026-08-31

### Fixed
- The target HP bar in the roster no longer looks short of full when you're talking to or banking with an NPC.

## [1.0.241] - 2026-08-31

### Changed
- The Activity Feed and Loot Log pages now use the full width of the screen instead of staying narrow and centered, matching the Items page.

## [1.0.238] - 2026-08-31

### Changed
- The loot log page is simpler now - the clue tier, sort, and accounting-mode filters are gone, and drops are always listed most-recent-first.

## [1.0.235] - 2026-08-31

### Added
- Clue casket completions now show up in the activity feed, with the clue tier, total casket value in gp, and a clue scroll icon.

## [1.0.233] - 2026-08-31

### Fixed
- The "Combat Achievements" tab in the top nav no longer gets crowded out by the account menu on narrower screens.

## [1.0.231] - 2026-08-31

### Fixed
- On mobile, the player stats panel could no longer be closed by tapping outside it without also blocking taps on the buttons and tabs inside the panel itself.

## [1.0.229] - 2026-08-31

### Changed
- The Combat Achievements member list and the group settings colour picker now show each member's actual helmet icon, tinted to their chosen colour, instead of a plain initial or a hand-drawn helmet shape.

## [1.0.227] - 2026-08-31

### Added
- The group map now shows raid markers (Danger, Safe Spot, Loot, Focus/Kill Target) dropped by teammates in-game, with the same icon they see in RuneLite.

## [1.0.226] - 2026-08-31

### Fixed
- Fixed clearing a ping (and pings ending automatically) silently failing every time.

## [1.0.225] - 2026-08-31

### Changed
- The ping toast now stays on screen for 20 seconds instead of 6, so there's actually time to click it before it disappears.

## [1.0.222] - 2026-08-31

### Added
- Dropping a ping now pops a quick toast for everyone in the group - click it to jump straight to that spot on the map.

## [1.0.221] - 2026-08-31

### Added
- Group members can now drop pings on the map that everyone in the group sees live, both in-game and on the website map, laying the groundwork for the upcoming right-click/hotkey ping feature in the RuneLite plugin.

## [1.0.71] - 2026-08-31

### Changed
- Internal: fixed the deploy pipeline so the Discord patch-notes bot can find the server's deploy folder, so it stops reporting "no changes" on every real release.

## [1.0.70] - 2026-08-31

### Fixed
- Fixed the RuneLite plugin's live party overlay and side panel not showing other group members, even though they showed up fine as online on the website. A routing mix-up on the server meant the plugin's real-time connection request and character-linking request were quietly being rejected.

## [1.0.69] - 2026-08-31

### Changed
- Internal: the live group vitals feed now also carries each member's map position, laying the groundwork for the upcoming RuneLite plugin feature that shows group members on the in-game world map and minimap.

## [1.0.68] - 2026-08-31

### Fixed
- Fixed a group member's active prayer icons sometimes staying visible after they logged out.

## [1.0.67] - 2026-08-30

### Fixed
- Fixed the account page's online badge showing a group member as offline after a minute of standing idle even though their client was still checking in fine.

## [1.0.66] - 2026-08-29

### Added
- Activity feed now shows icons for quest completions, combat achievement tiers, diary entries, and collection log additions.

### Changed
- Combat Achievements tab now fills the full page width like Graphs does, with bigger progress bars, per-tier percentages, and a group-average summary row.
- Character avatars and text on the Account > Characters page are bigger and easier to read.

### Fixed
- Fixed group members incorrectly showing as offline while idle (standing still, AFK skilling, etc.) even though RuneLite was still connected and sending updates fine.
- Added logging around the live party-overlay connection so future connection drops are easier to diagnose (no user-facing change).
- Group Settings no longer shows your personal account token in place of your group's real invite token when you open it via Account > Characters — the Copy button is disabled with an explanation until you reroll for a real, shareable one.
- Discord boss kill notifications now show your account's real in-game kill count instead of how many times GroupScape happened to see that boss's kill logged on its own server.

## [1.0.65] - 2026-08-28

### Removed
- Removed farming patch and bird house timers tracking from the site and Timers page.

## [1.0.64] - 2026-08-28

### Added
- Deploys now automatically post a summary of what changed to Discord.
