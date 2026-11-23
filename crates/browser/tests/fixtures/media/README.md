# MP4 playback fixture

`movie_5.mp4` is the Web Platform Tests file of the same name from
commit `9ef14f2a2923158e5d1ff065b9d41066ec174e73`. It contains H.264 video
and AAC audio. Playback tests mute the engine and verify decoded pixels
and time advancement without a network connection. [Upstream file](https://github.com/web-platform-tests/wpt/blob/9ef14f2a2923158e5d1ff065b9d41066ec174e73/media/movie_5.mp4)

SHA-256 `e2e2bd5b7641b88406a8db15410dc1ed55d89547cb29bd133491e4f90229e1e1`.

The segmented playback tests share the synthetic [video](../../../../engine/data/pages/mse-video.mp4) and [audio](../../../../engine/data/pages/mse-audio.mp4) used by `about:streaming`. The test pattern and sine tone use separate H.264 and AAC tracks with roughly one-second fragments.
