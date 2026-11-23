# Public media QA — 2026-11-23 UTC

See [procedure and evidence rules](PUBLIC_MEDIA_QA.md). This run used anonymous, fresh profiles on Windows 11 Pro N x64 (`10.0.26200`), Node.js `26.13.0`, 1280×800 headless browser lanes, and REA `6.10.1`. Targets: Twitch [`theburntpeanut_247`](https://www.twitch.tv/theburntpeanut_247) and YouTube [`aqz-KE-bpKQ`](https://www.youtube.com/watch?v=aqz-KE-bpKQ). No login or chat posting was attempted.

## Installed Satin baseline

`0.1.0-alpha.17`, source `2856cff150ed18925c0467249b797d7786b2b922`; executable SHA-256 `9b28b286851b50075979ba5e0604a10fda7ad5cfc720cb722b29580058c77243`, checked against the installed manifest. Headless launch used the exact versioned browser binary, not the launcher or official profile.

| Site | Start UTC | Executed scripts | Diagnostics (nesting) | Player / chat |
| --- | --- | ---: | ---: | --- |
| Twitch | `18:15:33.070Z` | 41 | 64 (49) | No `<video>`, no player marker, no active media status or chat line marker. |
| YouTube | `18:15:42.765Z` | 47 | 5 (3) | No `<video>`, no `#movie_player`, and no active media status. |

The first Twitch diagnostic was not classified by the sanitizer. The next was `ExecutionLimit: Expression nesting limit exceeded` at inline script #140, followed by more nesting errors. The first two YouTube diagnostics were `ExecutionLimit: Expression nesting limit exceeded`. Neither case reached a player to test time progress, frames, audio, or controls.

Sanitized receipt: `target/public-media/satin-8ZEUz6/summary.json`. A repeat at `18:20:59.453Z` (Twitch) and `18:21:09.521Z` (YouTube) returned the same script and diagnostic counts and no player: `target/public-media/satin-0RfHUw/summary.json`.

## Chrome reference, first pass

Google Chrome `157.0.8103.79`, executable SHA-256 `a527591d96df1f30bdabf5599d257b8b80c83d90c405ff997ddbc202ba5e9ada`. The precise-CDP script count is not the same counter as Satin's loader count.

| Site | Start UTC | Executed script IDs | First page error | Player and read-only chat |
| --- | --- | ---: | --- | --- |
| Twitch | `18:15:49.421Z` | 408 | `SecurityError` (cause not classified) | One player `<video>` at 640×360. Time `0 → 0 s`, decoded frames `16 → 16`, decoded audio bytes `5537 → 5537` over 5.035 s. Chrome reported one audio track with `FFmpegAudioDecoder`. Five rendered chat lines were read as a count only. |
| YouTube | `18:16:58.461Z` | 55 | None captured | One player `<video>` at 854×480. Time `1.457 → 1.457 s`, decoded frames `41 → 41`, decoded audio bytes `24572 → 24572` over 5.062 s. Chrome reported one audio track with `OpusAudioDecoder`. Live chat did not apply. |

Twitch's play click did not establish progress; pause and seek were not exercised. On YouTube the consent UI remained, the play control was unavailable to the probe, and seek did not complete. Neither first-pass result established sustained playback or audible sound. The REA subprocess returned success for both captures, but this version of the runner did not extract step evidence from the CLI envelope.

Sanitized receipt: `target/public-media/chrome-oCdotj/summary.json`.

## Chrome reference, follow-up

Same Chrome binary. The Playwright follow-up waited for Twitch metadata before trying to play, and broadened the YouTube optional-cookie button locator. Service workers were allowed.

| Site | Start UTC | Executed script IDs | First page error | Player and read-only chat |
| --- | --- | ---: | --- | --- |
| Twitch | `18:19:51.843Z` | 408 | `SecurityError` (cause not classified) | One player `<video>`. From `18:20:00.122Z` to `18:20:04.964Z` its time advanced `0.003 → 4.654 s`, decoded frames `8 → 227`, decoded audio bytes `3908 → 20466`, and resolution changed from 640×360 to 1280×720. No ad marker was detected. Five dedicated Blob workers were observed. Rendered chat-line count rose from one to seven; no text was retained. |
| YouTube | `18:21:07.992Z` | 55 | None captured | One player `<video>` at 854×480. After play, time changed `1.552 → 3.526 s`, decoded frames `51 → 111`, and decoded audio bytes `25967 → 53434`. The video was paused at the sampled checkpoints and held at `3.526 s` for the 4.949 s progress interval. Live chat did not apply. |

- **Twitch controls:** Autoplay was active. Pause reset this player's reported clock to `0 s`; it stayed paused at `0 s`, with 238 decoded frames, for a 1.216 s hold. After resuming the clock was `1.626 s`, the player was no longer paused, and there were 344 decoded frames. No usable seek control was identified; seek was not exercised.
- **YouTube controls:** The optional-cookie rejection and play clicks completed. The consent wrapper still matched the probe. A midpoint scrubber click changed `currentTime` to `317.283 s` while paused; it did not show continued frame or time progress at the new position. Sustained playback and an explicit pause interaction were not demonstrated.
- **Audio:** Chrome reported one audio track and a decoder on both sites (`FFmpegAudioDecoder` on Twitch, `OpusAudioDecoder` on YouTube). The decoded-byte increases establish decoding, not audible output.
- **REA:** Separate Chrome captures started at `18:19:20.864Z` (Twitch, `ev_f156a0006801ce820b865e808eb88799bdb1fd4274134445a06bb56a9adc0a83`) and `18:20:36.373Z` (YouTube, `ev_d7358d8d337cb6a26298728d1f4cfd5ffd6a54c57a7d79d27c9617ac93dd51fa`). Both completed the `media-shell` and `settle` steps and terminated the owned process. The REA shell checks do not prove playback.

Sanitized receipt: `target/public-media/chrome-hsAomw/summary.json`.

## Chrome reference, YouTube after consent dismissed

The same binary started a new YouTube run at `18:23:35.119Z`. This probe waited for the optional-cookie control to disappear before checking playback. The player had one real `<video>` at 854×480, autoplay was active, no ad marker was detected, and the consent control was no longer visible. Chrome measured 55 executed HTTP(S)-named script IDs, no page errors, and one audio track with `OpusAudioDecoder`.

- From `18:23:42.834Z` to `18:23:47.831Z`, time advanced `0.457 → 5.453 s`, decoded frames `18 → 168`, and decoded audio bytes `10784 → 81709`.
- The pause click held the video at `5.533 s` and 170 decoded frames for 1.208 s. The resume click advanced to `7.409 s` and 226 decoded frames. The midpoint scrubber click then changed `currentTime` to `317.283 s` (duration `634.601 s`). Sustained playback after that seek was not sampled.
- The separate REA shell capture started `18:23:21.425Z`; `ev_f012b24a4d1e15fa5b180eba01503f339efe5a6daa1e9e4540935a30bb233b58` completed all steps and terminated the owned process. Audio was not checked audibly, and live chat did not apply.

Sanitized receipt: `target/public-media/chrome-M4FBXN/summary.json`.

## Satin candidate

Not tested yet: no coordinator build was available to this worktree for the recorded runs.

## Earlier captures reviewed

The existing folder `%TEMP%\rb-youtube-qa-20261123` was read, not overwritten. Its `chrome-baseline.json` (`14:33:01.105Z`) observed the YouTube watch shell, one video, and consent; the clock displayed `0:00`. `chrome-playback.json` (`14:35:50.976Z`) and `chrome-consent-playback.json` did not satisfy their progress check (final displayed times `0:01` and `0:00`). `twitch-candidate-comparison.json` and `youtube-candidate.json` cover alpha.15, not the installed alpha.17 or the candidate in this comparison. A prior Chrome investigation reported IVS Blob workers and 32 MiB of Wasm memory; this pass did not measure worker Wasm memory.
