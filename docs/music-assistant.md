# Unified media playback: audio + video, any source → any player (mapping / long-term)

> **Status: MAP ONLY.** The unifying orca domain and Music Assistant (MA) are
> **not yet built/deployed**. This documents the target so it can be built
> incrementally and turned into plugin code. Home Assistant is live at
> `10.0.0.13:8123` (see [home-assistant.md](home-assistant.md); creds in
> 1Password `Home Assistant (orca)`). Audio engine = Music Assistant; video =
> the existing Plex (mimir/njord) + Jellyfin servers; HA exposes the players;
> **orca is the single pane over all of it.**

## Goal

A **holistic view of our music, like everything else in orca** — one unified
domain over every music **source** and every music **player**, so we can play
**any source → any player, all players, or synced groups**, from a single pane.

> This is the **playback** half of the unified `media` domain. The **catalog /
> convergence** half — one de-duplicated title view across files, both Plex
> instances, Jellyfin, the *arr managers, and Bazarr subtitles — is in
> [arr/docs/media-catalog-convergence.md](../../arr/docs/media-catalog-convergence.md).
> Same domain: the catalog answers "what media exists / add it / manage subs,"
> playback answers "play it here."

Home Assistant exposes the players; Music Assistant is the audio engine that
speaks every source and player protocol and does multi-room sync; **orca unifies
them into one music domain** — the same generic model orca applies to pods,
services, and storage ([[orca-single-pane-of-glass-vision]],
[[orca-defines-what-plugins-define-how]]).

## Holistic media-playback domain (orca)

**Both audio and video** are modeled the same way — one generic playback domain
over every source and every player, tagged by `mediaType` (`audio` | `video`).
Plugins define *how*; orca defines *what*
([[orca-defines-what-plugins-define-how]]).

| Noun | Is | audio backed by | video backed by |
|------|----|-----------------|-----------------|
| **source** | where content comes from (provider + library) | MA providers: Navidrome, Plex, Spotify, local | Plex (mimir/njord), Jellyfin |
| **player** | a single output (room / screen / cast device) | MA players + HA `media_player` | TVs, Chromecast, Apple TV, Plex/JF clients — all HA `media_player` |
| **group / target set** | one or more players fed the **same source** (ad-hoc or named) | MA sync groups / Snapcast (tight sync) | multiple Plex/JF sessions to N screens |
| **now-playing / queue** | what's playing where, and what's next | MA queue | Plex/JF sessions |

**Any source → one player, several players, or all — for both audio and video.**
The target is always a **set** of players (size 1..all); a single player is just a
set of one. Examples:
- Audio: "play this album in the kitchen" · "play this song on the kitchen +
  patio + office" · "…everywhere."
- Video: "play this movie on the living-room TV" · "**play this movie on the
  living-room AND bedroom TVs**" · "resume this show on the bedroom TV."

**Sync is a convergence loop, not just a shared start.** The domain always allows
N destinations and actively keeps them aligned; the *tightness* depends on the
players, but every case is best-effort *synced*, not fire-and-forget:
- **Audio** → tight sample-accurate sync via MA groups / Snapcast (shared clock).
- **Video** → a **sync controller** keeps N independent Plex/JF (or HA) sessions
  aligned even without a shared clock: pick a master, poll every session's
  `media_position` (Plex/JF sessions API, HA `media_player.media_position`),
  compute drift, and correct the laggards — **micro-seek** to
  `master_pos + latency`, or nudge **playback rate** (0.97–1.03×) where the player
  supports it (smoother than seeking). Loop every ~1–3 s within a tolerance
  (e.g. ±300 ms); coordinated play/pause/seek transport to all at once. This is
  the same **detect→correct/converge** pattern orca uses everywhere
  ([[plugins-detect-and-remediate-incident-loop]]) — applied to playback position.

So "a movie on two TVs, in sync" is real: best-effort, actively held, degrading
gracefully to loose alignment only on players that expose neither seek nor rate.

Proposed surface (one generic noun family; `mediaType` selects the backend):
`media.source.list`, `media.player.list [--type audio|video]`,
`media.play {source, target}` where **`target` is a set of 1..N players** —
`[player…]` (ad-hoc, e.g. two TVs), a named `group`, or `all`;
`media.group.{create,add,remove}` (persist a set / audio sync zone),
`media.player.{pause,seek,volume,…}`, `media.now`. Mesh-aware: players live on
different hosts/rooms but present as one set
([[mesh-tool-surface-must-expose-peer-tools]]).

This is the **single pane** ([[orca-single-pane-of-glass-vision]]): ask orca
"what's playing / play X in the kitchen / put this movie on the TV / play this
everywhere," and it drives MA (audio) or Plex/Jellyfin (video) via HA underneath
— no per-app juggling.

## Why Music Assistant

MA sits between **music providers** (where audio comes from) and **players**
(where it comes out), with a real queue engine, transcoding, and gapless/multi-room
sync — things raw HA `media_player` entities don't do well. It runs as an HA
add-on or a standalone container and exposes a first-class HA integration.

## Target architecture

```
 Providers  ──►  Music Assistant  ──►  Players ──► rooms/devices
 (our music)     (queues, sync)        (protocols)
```

### Music providers (map to what we already run)

| Provider | Source | Notes |
|----------|--------|-------|
| **Navidrome (Subsonic)** | `http://10.0.0.6:4533` | our self-hosted library; creds in `navidrome (orca)`. Primary. |
| **Plex** | mimir/njord | optional, if we want Plex music too |
| **Spotify / others** | cloud | optional streaming providers |
| Local filesystem | willow `/data/media/music` | fallback / direct |

### Players (protocols MA speaks)

Chromecast/Google, Sonos, AirPlay, DLNA/UPnP, Snapcast (multi-room sync),
Squeezelite/Slimproto, and **HA `media_player` entities** (so anything HA already
controls becomes an MA target). Inventory the household devices and bucket each
into one of these.

### Home Assistant role

- MA registers as an HA integration → each MA player becomes an HA `media_player`.
- HA automations/scenes/voice (Assist) drive playback: "play X in the kitchen",
  presence-based audio, alarms, TTS ducking.
- The orca **homeassistant** plugin already exposes `home-assistant.*` (entities,
  call-service, automations) — that's the lever to script playback from orca.

### Video: same model

Video needs no new engine — the existing servers are the sources and HA already
exposes the players:

- **Sources:** Plex (mimir/njord) and Jellyfin, controlled via their APIs (or the
  Plex/JF HA integrations).
- **Players:** TVs, Chromecast, Apple TV, and Plex/Jellyfin client apps all appear
  as HA `media_player` entities — the same target set as audio.
- **Route any video → one or several players, actively synced:** "cast this movie
  to the living-room TV", or the **same movie to two TVs at once** (living-room +
  bedroom) held in sync by the video sync controller (position-reconciliation
  loop — see below), not just started together.

### Player discovery & fallback (no dead ends)

HA already exposes **Apple TV, Samsung TV, LG TV, Google TV** (and more) as
`media_player` entities. So orca never needs a "capable" native (MA / Chromecast)
player for every device: **discover the full player set** = HA `media_player`
entities ∪ MA players, unify them, and route to any of them. When a target has no
native/cast player, **fall back to its HA TV integration** as the player. Player
discovery is: enumerate → classify (audio/video, native vs HA-only) → present as
one set → pick the best control path per target.

So orca's `media.*` surface spans both: `mediaType=audio` dispatches to MA,
`mediaType=video` dispatches to Plex/JF — both through HA `media_player` targets.

## orca plugin plan

- **Unified `media.*` domain in orca** — one noun family (source/player/group/now)
  over both types, `mediaType` selecting the backend. This is the "like everything
  else" single-pane surface ([[orca-single-pane-of-glass-vision]]).
- **New `music-assistant` plugin** (service adapter, same shape as navidrome/komga).
  Deploy via `deploy_target` (baldur, next to Navidrome). `configure` wires audio
  providers (Navidrome creds from orca vault) + player discovery; `status` reports
  providers/players/queues/sync-groups.
- **Video via existing plugins** — the Plex/Jellyfin plugins expose sources +
  drive playback to clients; no new engine.
- **homeassistant plugin** is the common exposure/control layer: every player
  (audio and video) is an HA `media_player`; orca drives them via `home-assistant.*`
  call-service. MA↔HA and Plex/JF↔HA are integration configs the plugins set.
- Reuse the media-notify work: Navidrome is already the unified music library
  ([scan dispatcher](../../sabnzbd/docs/scan-dispatcher.md), [[media-scan-on-import-dispatcher]])
  — MA points at the same Navidrome, one source of truth.

## Build phases (incremental / additive)

1. **Deploy MA** (compose on baldur) — bare instance, HA integration added.
2. **Wire Navidrome provider** — MA plays our library (creds from `navidrome (orca)`).
3. **Enumerate + add players** — audio (MA protocols) and video (Plex/JF clients,
   TVs, cast) as HA `media_player` entities; verify per room.
4. **Audio sync groups** — whole-home + zones (MA/Snapcast); "play everywhere".
5. **Video routing + sync controller** — any Plex/JF title → any set of video
   players; position-reconciliation loop (poll `media_position` → micro-seek /
   rate-nudge to a master) to hold multiple TVs best-effort in sync.
6. **HA automations/voice** — presence/scene/Assist for both.
7. **orca `media.*` domain + `music-assistant` plugin** — codify the unified
   surface + deploy/`configure`/`status`; retire any hand config.

## Credentials

To create when deployed: `music-assistant (orca)` in the orca vault (its own admin
+ any provider tokens). Navidrome provider reuses `navidrome (orca)`. HA link uses
a long-lived HA token (`Home Assistant (orca)`).
