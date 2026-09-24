# Home Assistant ↔ Apple Home parity & voice control

Working notes for achieving full parity and control between **HA Voice (Assist)** and
**Apple Home (HomeKit / Siri)** for the house. Captured live from the running instance;
update as the configuration converges.

- **Instance:** HAOS `2026.8.3` at `http://10.0.0.13:8123` (network_mode host, `/config` on host)
- **Token:** 1Password item `Home Assistant (orca)` (long-lived, admin)
- **Host access:** `ssh root@10.0.0.13` (key-based) → `/config/.storage/*` is ground truth.
  No `python3` on the HAOS host; pull `.storage/*.json` locally to parse.
- **WS API:** `ws://10.0.0.13:8123/api/websocket` (auth → `config/*_registry/list`, `config_entries/get`).
  `config_entries/get` **strips `options`** — bridge include/exclude filters must be read from
  `.storage/core.config_entries`, not the WS list.

## Topology (as found 2026-08-23)

- **305 states / 684 registry entries / 86 devices / 8 areas / 2 floors.**
- Floors: `Main` (1/2 Bath, Exterior, Kitchen, Living Room), `Second` (Bedroom, Hallway, Office).
- Controllable devices: 48 enabled/visible; **45 have an area**, 3 unassigned are infra-only
  (`button.zigbee2mqtt_bridge_restart`, `switch.zigbee2mqtt_bridge_permit_join`,
  `button.bedroom_right_remote_2_identify`) — correctly area-less.
- Voice hardware: 2× HA Voice PE satellites — `...09e467` "Jarvis" (idle/online),
  `...09d006` "Office Assistant" (**unavailable** — offline/powered down).

### HomeKit exposure (bridge domain filters)
- `HASS Bridge:21064` (mode=bridge, primary): alarm_control_panel, climate, cover, humidifier,
  fan, light, lock, media_player, remote, switch, vacuum, water_heater.
  **Missing `scene` + `script`** → scenes absent from this Apple Home.
- `HASS Bridge JJ:21065` (mode=bridge, second home/person): the above **plus** automation,
  binary_sensor, button, device_tracker, input_*, person, scene, script, select, sensor, valve,
  lawn_mower. **Over-exposed** → Apple Home flooded with 87 sensors / 23 buttons / 26 selects etc.
- Accessory-mode (correct — TVs/locks require it): `Frontdoor Lock` (`lock.door_lock`),
  `Living Room TV` (lg webos), `Living Room TV Screen` (samsung 55in),
  `Livingroom Soundbar DLNA`, `PlayStation 5`.
  ⚠️ These media_players are **also** caught by the `media_player` domain in both bridges →
  double exposure. Add them to each bridge's `exclude_entities`.

### Voice / Assist exposure — THE parity gap
- `.storage/homeassistant.exposed_entities`: only **3 entities** exposed to `conversation`
  (Assist): 2 media_players + 1 conversation. **HA Voice can control almost nothing.**
- **107** entities exposed to `cloud.google_assistant` (legacy Google Home path).
- Apple Home/Siri coverage (via HomeKit bridge) is broad; Assist coverage is ~empty →
  the two voice surfaces are wildly out of parity.

### Assist pipelines
- `Home Assistant Cloud`: HA Cloud STT+TTS, `conversation.home_assistant` agent.
- `Whisper`: local faster-whisper STT, `tts.home_assistant_cloud`, `ollama` conversation agent.
- **`preferred_pipeline: None`** — no default pipeline set.

## Known corrections (prioritized)

1. **Voice parity:** expose the curated controllable set (~45 area-assigned entities, matching
   HomeKit's controllable domains + scenes) to `conversation`. Set voice-friendly aliases.
2. **HomeKit consistency:** make both bridges expose the SAME controllable set; add `scene`+`script`
   to the primary bridge; strip clutter domains (sensor/binary_sensor/button/select/device_tracker/
   automation) from `JJ`. Curate to controllable domains only.
3. **Media double-exposure:** exclude the 5 accessory-mode media_players from both bridges' domains.
4. **Set preferred Assist pipeline** and assign to both satellites; recover "Office Assistant" (offline).
5. **Repair:** remove dead `http:` block from `configuration.yaml` (lines 12-16). Settings already
   migrated to `.storage/http` (`yaml_migration_done: true`, trusted_proxies `10.0.0.6/32` preserved) —
   safe to delete. Deadline: before HA 2027.2.0.
6. **Decide Google Assistant** (107 exposed): retire if going all-in on Apple, or keep for parity.
7. **Later:** HA dashboards + HomeKit/Apple Home room+dashboard organization.

## Decisions (confirmed 2026-08-23)
- **One HomeKit bridge.** Both bridges (`21064` primary + `21065` "JJ") are paired to the SAME two
  Apple Home controllers (`5fdd2c1e…`, `0d5638b3…`) → JJ is a duplicate, not a second person's home,
  and is the source of double accessories in the Home app. **Remove `HASS Bridge JJ`**; keep
  `HASS Bridge` as the single curated bridge + the 5 accessory-mode entries.
- **Keep Google (3-way parity).** Treat Google Home like Apple. Sync ONE curated controllable set
  across all three surfaces: HomeKit bridge, Assist (`conversation`), and `cloud.google_assistant`.
- **Everything clearly labeled** — friendly names + voice aliases on every controllable entity.
- Execution: curated set, step-by-step with approval.

## Labeling audit (controllable entities) — problems found
Areas are mostly correct. The issues are **names + missing aliases** (NO entity has any voice alias):
- **Ambiguous names** (bad for voice/Home app): `media_player.googlehome7284`="Speaker",
  `light.livingroom`="Area", `light.living_room`="Living room Area",
  `light.living_room_main_lights`="Overhead", `switch.office_overhead`/`switch.bedroom_main_lights`
  both ="Overhead", `light.office_table_lamp`="Office", `lock.exterior_lock…`="Lock",
  `light.exterior_dimmer…`="Dimmer".
- **Leaked slugs in names:** `switch.kitchen_washer…`="Washer Kitchen-Washer_switch".
- **Three overlapping Living Room light groups** (`light.living_room`, `light.livingroom`,
  `light.living_room_main_lights`) with meaningless names — need to know what each controls.
- **Two front-door locks?** `lock.door_lock`="Frontdoor Lock" (accessory bridge) vs
  `lock.exterior_lock…`="Lock" — confirm which is real / dedupe.
- **Stale entity_id slugs** (cosmetic): `media_player.living_room_samsung_55in`="Bedroom Screen"
  (now in Bedroom), `media_player.bedroom_googletv`="Office GoogleTV" (now in Office). Area+name
  agree; slugs are just historical.

## Applied changes (2026-08-23)
- ✅ **Deleted duplicate `HASS Bridge JJ` (21065)** via REST `DELETE /api/config/config_entries/entry/…`
  (`require_restart:false`). Single `HASS Bridge` (21064) remains + 5 accessory entries. Backup:
  `/config/.storage/core.config_entries.bak-20260823-202549` on host.
- ✅ **Relabeled 11 controllable entities** + added voice aliases via WS `config/entity_registry/update`:
  Bedroom Speaker, Living Room Overhead, Office/Bedroom Overhead, Office Table Lamp, Kitchen Washer,
  Back Porch Lights, **Patio Door** (real lock; was "Lock"), **Patio String Lights** (was "Dimmer"),
  **Living Room Lamps** (HA group light.living_room). Hid redundant Z2M group `light.livingroom` ("Area").
- Geography learned: patio door = the de-facto front door (**no smart lock on the actual front door**);
  carport + patio + string lights + back porch all on the same side; a separate street light exists
  (candidate label "Back Door"? — TBD).

## More applied changes (2026-08-23, cont.)
- ✅ Renamed soundbar `media_player.living_room_soundbar_dlna_3` → **"Living Room Soundbar"** (+aliases);
  DLNA/Cast duplicates stay hidden. Decision: LG panel keeps "Living Room TV"; Apple TV stays
  "Living Room Apple TV" (no HomeKit TV change).
- ✅ **Removed dead `http:` block** from `configuration.yaml` (backup `.bak-20260823-2030`);
  `ha core check` passed. Repair clears (settings live in `.storage/http`).
- Notes: `media_player.bedroom_googletv` = the **Office** Chromecast (area/name already Office;
  entity_id slug `bedroom_googletv` is stale → optional slug rename). Apple TV network hostname ≈ "greg".

## Curated parity set (26 entities) — pending exposure
The single source of truth to expose identically to HomeKit + Assist(`conversation`) + Google.
Built from labeled controllable entities, EXCLUDING: car lock, dead `lock.door_lock`, voice-satellite
helper switches (`_mute`/`_wake_sound`/`_led_ring`), zigbee2mqtt infra. Full list in scratchpad
`curated.json`. ⚠️ `switch.office_servers` ("Servers") is in-set — consider excluding from VOICE to
avoid accidental "turn off servers" (keep in HomeKit).

## Parity sync APPLIED + VERIFIED (2026-08-23)
- ✅ **27-entity curated set exposed to `conversation` (Assist/HA Voice) AND `cloud.google_assistant`.**
  Verified via entity-registry `options` (authoritative; the `.storage/homeassistant.exposed_entities`
  file lags ~10s so don't trust it for immediate verification). Assist exposure **3 → 27**.
  Functional check: Assist answered "is the patio door locked?" → "Yes".
- ✅ Converted `switch.office_overhead` → `fan.office_overhead_office_ceiling_fan` via **switch_as_x**
  (consistent with existing Carport/Porch switch→light conversions). Old switch auto-hidden.
- ✅ Media-center cleanup: `switch.living_room_media_console…_switch` → **"Media Center"** (unhidden,
  +aliases); hid `…_config_switch_10/_11` clutter. Renamed servers switch → **"Servers"**.
  Decision: Servers + Media Center fully exposed everywhere (Apple/Google/HA) — user accepts that
  HomeKit/Google exposure is inherently voice-toggleable too (can't split Siri from Home-app tile;
  only HA can separate dashboard-tap from Assist).
- ✅ Renamed `switch.exterior_back_porch_lights` → **"Front Door"**.

- ⚠️ **Storage-model note:** entity exposure is stored in the **entity registry `options`**
  (`{conversation:{should_expose}, cloud.google_assistant:{should_expose}}`) and mirrored to
  `homeassistant.exposed_entities`. Registry options = source of truth.

## Remaining work
- ✅ **HomeKit bridge refined** (2026-08-23): bridge `21064` switched to explicit **`include_entities`
  (30)** = curated non-media (19: lights/switches/fans/patio lock) **+ 11 scenes**. No media_players
  (Apple TVs/HomePods are native; TVs/soundbar/PS5 have their own accessory bridges). Applied via
  `.storage` edit during a clean `ha core stop`→swap→`start` (backup `.bak-20260823-210446`,
  `.preswap`). All homekit entries `loaded`, no errors. Also **deleted dead `Frontdoor Lock:21069`**
  accessory (pointed at nonexistent `lock.door_lock`; real patio lock now in the bridge).
- Voice target set = 27 curated + 11 scenes = **38** (media players INCLUDED for voice — Google/HA
  Voice can cast/play, unlike HomeKit). `conversation` already == 38.
- ✅ **Google trimmed + 3-way control parity VERIFIED** (2026-08-23): removed non-curated from Google;
  removed system clutter (`stt.home_assistant_cloud`, `zigbee2mqtt_bridge_permit_join`, wake_word
  select, cover-status binary_sensor, `unassigned_smart_away`) from voice; removed 16 hidden
  group-members/duplicates from local voice (Desk Left/Right/Under → covered by `light.desk`;
  LR lamp members → `light.living_room` "Living Room Lamps"; hid nameless Hue group `light.living_room_2`).
  **Final: HA Voice controllable = 38, Google = 38 (identical); HomeKit = 30 = voice−media_players.**
  Read-only niceties (temp/humidity sensors, weather, shopping-list todo) intentionally remain on
  local HA Voice only (enrich local assistant; not meaningful in HomeKit/Google).
- **Preferred Assist pipeline** unset; **"Office Assistant" satellite offline** (physical).
- ✅ **Exterior lights finalized** (2026-08-23): `light.exterior_street_porch`→**"Front Door"** (street
  side), `switch.exterior_back_porch_lights`→**"Patio"**, dimmer→**"Patio String Lights"**,
  `light.exterior_carport`→**"Carport"**, lock→**"Patio Door"** (main entrance). Hid
  `switch.exterior_lock…config_switch_40` clutter. ⚠️ Lock still carries a `front door` alias which
  collided with the street-side "Front Door" light — ✅ dropped; lock aliases now `patio door`/`patio lock`.
- **Music Assistant**: fix stale URL / locate correct container; then source validation + Spotify→Navidrome.
- **Dashboards** (HA + Apple Home room organization) — after labeling converges.

## Items 3 & 4 progress (2026-08-23)
- ✅ **Preferred Assist pipeline** was NOT unset (stale disk) — it's intentionally **Whisper (local)**.
  I briefly set it to Cloud then **reverted to Whisper** to respect the choice.
- ⚠️ **"Office Assistant" satellite offline** — physical (powered off/unplugged); power on at the device.
- 🔴 **`ollama` container NEVER started** (`docker inspect ollama` → status=created, StartedAt=0001) on
  willow (10.0.0.10). The **Whisper pipeline's conversation agent is ollama** → local voice
  understanding is currently non-functional. Either start ollama (`docker start ollama`, needs a model)
  or the local pipeline won't reason. (whisper-ai STT container IS up.)
- 🔴 **Music Assistant is not deployed at all** — no `music-assistant` stack exists in meerkat's compose
  set (`/mnt/user/appdata/meerkat/compose/` has 28 stacks incl. navidrome, plex, jellyfin, arr suite,
  but no MA). HA's MA integration points at dead `10.0.0.10:8095`. Media stacks don't run on willow
  (only dockge/ollama/syncthing/whisper-ai there); they deploy to a host that mounts `/mnt/willow/data/media`.
  **Plan (user-confirmed): author a `music-assistant` stack in meerkat, wire to Navidrome + Spotify, and
  reconnect HA's integration to the new address.** Local meerkat repo: `/Users/scottkey/code/meerkat`;
  per-service repos under `/Users/scottkey/code/argyle-labs/` (navidrome, homeassistant, ollama).

## Local voice — FIXED (2026-08-23)
- ✅ Started the `ollama` container on willow (was never started; `docker start ollama`; has models
  `qwen3.5:9b`, `deepseek-r1:8b`). But HA had **no** Ollama integration (Whisper pipeline pointed at
  nonexistent `conversation.ollama_conversation`).
- ✅ **Rewired the Whisper pipeline** `conversation_engine` → **`conversation.home_assistant`** (built-in
  local intent agent; controls all 38 exposed entities). Local voice now works fully local:
  `stt.faster_whisper` (whisper-ai container up) → HA intent → cloud TTS. Preferred pipeline = Whisper.
- ➕ Added a base Ollama integration in HA (`http://10.0.0.10:11434`) for optionally adding an LLM
  conversation agent later via UI (Settings→Devices→Ollama→Add conversation agent). Subentry flow is
  **UI-only** in 2026.8 (no working WS/REST path found — 4 endpoints tried).

## Music Assistant — DEPLOYED, onboarding pending (2026-08-23)
- ✅ **Deployed MA v2.9.13 on baldur (10.0.0.6:8095)** — new Dockge stack
  `/opt/stacks/music-assistant/docker-compose.yml` (image `ghcr.io/music-assistant/server`,
  `network_mode: host` for cast/DLNA/mDNS discovery, appdata `/opt/appdata/music-assistant`,
  music lib `${MEDIA_PATH}/music:/media/music:ro`). Container Up, `/info` returns 302/running.
- ✅ **HA integration connected** (`state=loaded`), MA admin user `skey` onboarded, OAuth token minted.
  (A stale OAuth callback link 500s harmlessly — the flow already completed; single clean entry.)
- ✅ **All 3 providers configured + VALIDATED** (via MA WS `auth/login`→`auth` JWT handshake; note
  `POST /auth/login` 500s, use the `/ws` command path):
  - **Subsonic→Navidrome** (`opensubsonic--uW9zndpL`, `10.0.0.6:4533`, user skey) — available.
  - **Audiobookshelf** (`audiobookshelf--2oT8exrQ`, `10.0.0.6:13378`, user skey) — available.
  - **Spotify** (`spotify--t4u6aRSo`, OAuth) — available/streaming.
- ✅ **Libraries synced:** 319 artists / 174 albums / **541 tracks** / 63 playlists (Navidrome),
  **223 audiobooks** (Audiobookshelf). 10 players discovered; 8 flowing into HA as media_players.
- Creds in 1Password: `navidrome (orca)`, `audiobookshelf (orca)`, `music assistant` (admin skey).
- ✅ **MA players made canonical** (2026-08-23): 10 MA `media_player`s named + area-assigned + voice/Google
  exposed as the music targets — Kitchen Speaker, Stereo (office), Soundbar (LR), Living Room Speaker,
  Living Room TV Speaker, Bedroom, Bedroom TV Speaker, Jarvis Speaker (LR), Mint (office), Everywhere
  (whole-house group). Raw Cast/DLNA speaker dups already hidden; un-exposed the raw esphome
  `home_assistant_voice_09e467_media_player` (MA "Jarvis Speaker" is canonical). TVs kept for video.
- 🔲 **Source-of-truth (meerkat repo)**: still to add — `compose/music-assistant/`, `.envrc` vars
  (`MUSIC_ASSISTANT_IMAGE_TAG/PORT/CONFIG_PATH`), Caddyfile route
  `music-assistant.scottkey.me → 10.0.0.6:8095`, `scripts/meerkat.d/registry.sh` entry,
  `docs/services/music-assistant.md`. (Repo's `compose/` files are currently empty in-tree; deployed
  copies live on the hosts.)
## Spotify → Navidrome library-building (#3, in progress 2026-08-23)
Architecture: **Lidarr Spotify Import List** → pulls artists/albums from Spotify → downloads via
Prowlarr indexers + qBittorrent/SABnzbd → imports to `/data/media/music` (= willow's music, served
by Navidrome). This is the canonical "capture Spotify → build Navidrome library" path.
- ✅ **Lidarr deployed** on freyr (10.0.0.15:8686) — the staged stack `/opt/stacks/lidarr` was down;
  brought up (`docker compose up -d`, ping 200). Mounts `/mnt/data/media`(CIFS from willow)→`/data/media`,
  `/mnt/downloads`. Shares the `media` docker network with sonarr/radarr/prowlarr.
- ✅ **Lidarr wired + verified** (2026-08-23): root folder `/data/media/music` (id 1, ~20TB free);
  SABnzbd download client (host `sabnzbd:8080`, category `music`, valid); Prowlarr→Lidarr app sync
  (2 indexers: NZBgeek, The Pirate Bay); quality profiles Any/Lossless/Standard. API keys — Lidarr
  `cf04c9c1…`, Prowlarr `ce0e6fb9…`, SAB `7fd3ca91…`. qBittorrent not added (WebUI pw hashed;
  add if usenet-only SAB isn't enough). Pipeline complete: Lidarr→indexers→SAB→`/data/media/music`
  →Navidrome→MA→voice/Apple/Google.
- ✅ **Spotify Import List connected** (2026-08-23): "Spotify Playlists" (SpotifyPlaylist), OAuth tokens
  set, **54 playlists** selected, qualityProfile=Lossless(2), rootFolder=/data/media/music,
  enableAutomaticAdd=True. **monitor=None (add-only) — user's choice**: Lidarr adds the artists but
  downloads nothing until the user manually selects albums (avoids runaway multi-TB pulls). No health
  errors. Sync (ImportListSync) triggered; artists populate as MusicBrainz matching completes.
- freyr *arr stacks symlink from `/opt/halvor/compose/` (not `/opt/meerkat`); willow shares `/data` +
  `/downloads` via CIFS (user `orca`).

## Media player disambiguation (open — needs physical mapping)
Many near-duplicate entities represent the SAME physical device via different integrations
(LG webOS + DLNA + GoogleCast + songpal). User input:
- "Living Room TV" = the **Apple TV** (what they actually use); Apple TV has another network alias.
- Living room stack entities to map: `media_player.living_room` ("Living Room Apple TV"),
  `media_player.lg_webos_tv_oled77c4aua`(_2/_3), `media_player.living_room_soundbar_dlna_2/_3`,
  `media_player.living_room_soundbar_google_cast`, songpal soundbars. Consolidate to one clear
  name per physical device; hide the redundant integration duplicates.

## Music Assistant workstream (added 2026-08-23)
- ⚠️ **MA integration is DOWN**: config entry URL `http://10.0.0.10:8095` points at the Unraid box
  (only 80/443 open); 0 MA-tagged players live. MA moved (likely `music.scottkey.me` behind 443)
  or container stopped. Fix address before source validation / Spotify→Navidrome work.
- MA server: `http://10.0.0.10:8095`, token in `music_assistant` config entry.
- Goals: (a) validate all sources reachable (Navidrome + Spotify + others);
  (b) expose MA playback so HA Voice speakers AND Google Home speakers can be told to play
  (treat Google like Apple); (c) capture Spotify library/playlists → generate download lists →
  build a Navidrome library. See also `music-assistant.md`.
- Speaker fleet (media_players): Apple TVs (bedroom/living room), Google Homes (kitchen mini,
  bedroom, chromecast audio "Stereo"), Sony songpal soundbars/speakers (10.0.0.3/5/26/174/180),
  DLNA renderers, HA Voice PE satellites (Jarvis online / Office Assistant offline), LG webOS + Samsung TVs.
