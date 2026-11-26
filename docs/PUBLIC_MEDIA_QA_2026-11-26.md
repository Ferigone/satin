# Windows public media baseline — 2026-11-26

Source `6412310dc26404075920a85abf51329354206b79`, optimized alpha.45 binary
`7e8be4cf3313d53b2f1939559280d2761e70bf3eb186113aaa2b0462544da872`; Chromium
158.0.8127.88. Both used anonymous, fresh profiles at 1280×800. See
[procedure](PUBLIC_MEDIA_QA.md) for the public targets and controls.

| Site | Satin bootstrapping | Positive Chrome control |
| --- | --- | --- |
| Twitch | 118 executed scripts in both runs; the CDN script exhausted instructions. No video, player or chat marker in either final DOM. | From 1.047 to 6.242 s, 40 to 324 decoded frames, 13,115 to 111,435 decoded audio bytes, no ad marker in that window; five Blob workers observed. |
| YouTube | 50 executed scripts in both runs. Inline script 293 exhausted native work in run 1 and instructions in run 2. Run 2 also hit the script compilation memory limit. No bound player in either capture; a media node was observed at ready state 0 with no dimensions or decoded frames. | From 1.606 to 6.694 s, 53 to 206 decoded frames and 26,536 to 101,408 decoded audio bytes, no ad marker in that window. |

These are bootstrapping failures, not evidence that CSS has no other defects.
Neither public player reached an actionable state in Satin. The known-content
local progressive and three MSE playback scenarios did pass on this same
binary, including pause, seek, resume, finalized completion and rebuffering.
Audio counts do not establish audible output.

The offline capability fixture showed H.264/AAC advertised on both browsers,
but VP9 and AV1 were not advertised in this Satin environment. Satin also did
not expose `MediaCapabilities.decodingInfo`, `requestVideoFrameCallback`, or
`MediaSourceHandle`. Do not assume these are the primary public site blocker
without a trace from the actual site; do not claim support merely to get past
feature detection.

Local receipts: `target/public-media/satin-ZNzNnY/`,
`satin-VR2HlG/`, `chrome-oJb5X7/`, `capabilities-9V1jhs/` under
`target/public-media/`, and `target/native-media-VitGMJ/`.
