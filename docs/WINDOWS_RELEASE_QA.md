# Windows x64 release QA

## Compare startup and idle memory

Run the same PowerShell command for each release executable. Pin the SHA-256
from the release asset metadata or the candidate build receipt, not from an
unverified download.

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\scripts\Measure-WindowsRelease.ps1 `
    -Executable '<path to Windows x64 browser executable>' `
    -Version '0.1.0-alpha.17' `
    -Sha256 '9B28B286851B50075979BA5E0604A10FDA7AD5CFC720CB722B29580058C77243'
```

The alpha.17 pin is for the browser, not the launcher. It matches the browser
entry of the `v0.1.0-alpha.17` installer ZIP (SHA-256
`0349151C2A3167E77C95D26248A77C052B860346DCBA387B11548CF8A9FAF3BF`).

- The probe copies the binary into a unique directory under
  `target/windows-measurements`, validates its digest and `--version`, and uses
  a new profile and process-local `LOCALAPPDATA` on every launch. It neither
  runs the installer nor changes a shortcut. The updater is not eligible.
- The workload is one native window showing a local, network-free, 240-card
  CSS grid, with a one-shot script. The probe measures process start to the
  first observed native window, then process start to the page-specific
  window title. The title is set from an animation-frame callback that checks
  the card count and layout height. This is a readiness proxy, not a hardware
  presentation timestamp.
- Exclude one warmup launch; report the median across five fresh-profile
  launches. For each launch, take the median of 20 memory samples 250 ms apart
  in the interval from 5 s to just before 10 s after title readiness. Report
  the main process's working set (resident, may include shared pages) and
  private bytes (private committed memory, not private resident pages)
  separately, in MiB. Neither counter includes separate OS/GPU processes.
- Compare only receipts with the same fixture digest, client dimensions and
  DPI. Use the same machine, logged-in desktop/monitor, display settings, and
  power mode. Do not interact with the windows. Stop unrelated builds and
  downloads where possible; repeat with the binaries in reverse order if
  timings matter. The OS file cache is not flushed. The JSON includes the
  executable and fixture hashes, geometry, per-run values and raw samples so
  an outlier or noisy run can be identified.

This measures one small local content load, not multi-tab residency, network
navigation, video playback, or support for the Twitch and YouTube players.

### Alpha.18 candidate comparison

The comparison uses the public alpha.17 executable pinned above and the
alpha.18 Windows x64 Release candidate from commit
`4daf9467e47f3aa3b13aa6daa2319e73f3e3f17e`, SHA-256
`9acb0041fd927709e526cac6db86a49f845fbdcbe8fe117a0944eef6532a3358`.
The common fixture SHA-256 is
`30d3bc8cb022d1e33be8b421ea35e4130238d0a4f8f60e486fcb803ca9409acd`.
Both binaries ran on Windows build 26200 (Core i7-7700K), with a
1586 × 992 px client area at 96 DPI (100% scale).

| November 23, 2026 series (five-run medians) | To window (ms) | To ready title (ms) | Working set (MiB) | Private bytes (MiB) |
| --- | ---: | ---: | ---: | ---: |
| Public alpha.17, original baseline | 115.04 | 394.23 | 44.64 | 27.02 |
| Release alpha.18 candidate | 72.34 | 278.23 | 44.91 | 27.24 |
| Public alpha.17, repeated immediately after candidate | 73.80 | 286.00 | 44.71 | 27.40 |

Against the same-session baseline, the candidate's median working set was
0.20 MiB higher and its private bytes were 0.16 MiB lower. The timing ranges
overlapped: 247–301 ms to the ready title for alpha.18 and 257–342 ms for
the alpha.17 repeat. Other builds were active, and host CPU snapshots around the
back-to-back series read 39–45%. The large difference from the original
baseline did not hold on the repeat; these runs do not establish a startup
speedup or a meaningful memory change beyond this fixture.

## Package and promotion gates

- `cargo metadata --format-version 1 --no-deps --locked` must report one
  workspace version. A pushed version tag must exactly equal `v` followed by
  that version. `--version` on the staged release browser and its launcher
  should report the same version. The launcher requires a staged
  `current.json` and browser under `versions/` to forward `--version`.
- `scripts/PackageRelease.ps1` emits `satin-windows-x64.exe` (browser only for
  in-app updates), `Satin-windows-x64.zip` (browser, launcher and installer
  scripts) and `SHA256SUMS.txt`. Check that the ZIP's browser entry and
  standalone executable are byte-identical, the ZIP has both Windows x64
  executables, and both hashes match `SHA256SUMS.txt`.
- `.github/workflows/release.yml` verifies PRs targeting `master`, pushes to
  `master`, and `v*` tags. Only a push in `Ferigone/satin` can run the
  contents-write publish job after verification. A **master push with an
  unpublished workspace version is a publication trigger**; a tag is not
  required. Published equal/newer SemVer releases cause a no-op. The
  prerelease numbers in `alpha.17` and `alpha.18` are compared numerically.
- The publish job accepts only a draft associated with that commit, verifies
  each GitHub asset's status, size and SHA-256, then makes the draft public.
  A draft is not an update channel. Publication is serialized, but GitHub
  concurrency is not a FIFO queue of all pending runs. If a run fails after
  creating a draft, investigate the draft and asset digests; don't assume a
  rebuild can safely resume a draft whose binaries differ.

## Installer, Preview, update and rollback

- Only run `Install.ps1` or packaged `Install.cmd` when intentionally installing
  for the current desktop user. Changing `LOCALAPPDATA` is not enough to
  isolate it: the script also resolves the Windows Desktop known folder for
  `Satin.lnk`. Run native/headless smokes on a staged binary with a disposable
  profile instead. The installer stages the browser, runs smokes, and uses
  the shared publication lock and `File.Replace` to retain `previous.json`
  when there is an existing `current.json`.
- Preview uses a different profile and stores hash-named snapshots, not
  installed versions. For a pinned release snapshot, validate the digest
  after copying and reject mismatches without falling back to an unpinned
  build. Perform any staging check with an isolated `LOCALAPPDATA` and
  `-NoLaunch`. Creating or retargeting the desktop Preview shortcut is a
  separate action.
- Only a normal Windows x64 run of the current installed build qualifies for
  in-app updates (`current.json` version and absolute executable must
  match). Headless, smoke-window and unmanaged executable launches do not
  qualify. Updater selection requires a newer SemVer and a non-draft GitHub
  release; alpha prereleases are intentionally admitted. The standalone
  asset must pass official HTTPS URL, size, SHA-256, and PE machine checks. The
  startup check fetches only release metadata. **Download update** stages the
  verified binary without changing either manifest. **Restart and update**
  re-verifies and holds both executables, closes the browser, releases its
  profile and trial-launches the exact staged executable. The trial acquires
  the same profile and acknowledges its first native frame over a fresh
  private pipe. Only then does the updater switch `current.json` under the
  publication lock. The unchanged desktop launcher still resolves the old
  build until publication. See [interactive updater QA](#interactive-updater-qa).
- `Rollback.ps1` verifies the previous target's SHA-256, then swaps the
  manifests under the same lock. Relaunch to use it. When a newer installed
  version is now in `previous.json`, that version is the updater's floor, so
  it won't immediately undo the rollback. A simulated manifest swap is not
  proof of an actual release download.

### Interactive updater QA

Run the offline regression tests on Windows x64:

```powershell
cargo test --locked -p rust-browser updater::tests
```

These tests use disposable manifests and a loopback stream. They check passive
discovery, duplicate/stale commands, intermediate progress, corrupt bytes,
unchanged manifests after staging, active-version/rollback-floor checks at
apply, and failing publication with a live `current.json` file lock. They also
cover a replaced pending payload, verification and pinning of the prior
executable, staged ticket identity, immediate spawn failure and recovery
refusing to overwrite a superseding manifest.

For the offline **process** smoke, compile the stand-alone probe and use an
unmodified workspace launcher binary. The test copies both into a temporary
installation; it never uses the actual user's install, shortcut or profile.
Use absolute paths and a disposable output folder:

```powershell
rustc --edition 2024 crates/browser/tests/fixtures/updater_probe.rs -o "$scratch\updater-probe.exe"
$env:SATIN_TEST_UPDATER_PROBE = "$scratch\updater-probe.exe"
$env:SATIN_TEST_LAUNCHER = 'C:\path\to\target\debug\rust-browser-launcher.exe'
cargo test --locked -p rust-browser supervised_restart_requires_ack_and_reopens_exact_profile -- --ignored
```

The probe must receive only the original effective profile (no startup URL),
take its exclusive lock, and read the saved two-tab session with the correct
active tab. It makes no network requests. The unchanged launcher queries
`--version` while a trial is waiting for acknowledgment; it must route to
the old executable until a successful commit. Early exit, timeout, an
incorrect receipt, and an acknowledged trial whose manifest rename fails
must all leave `current.json` on the old executable and reopen it with the
saved session. The launcher must still route to the old executable after
each failed trial.

The startup acknowledgment is bounded (25 seconds, with up to 5 seconds to
confirm termination after failure). Older candidate builds without the
current readiness protocol cannot be activated. The nonce is per trial, and
its receipt must come from the live, supervised executable's private pipe;
there is no named event or durable receipt to reuse. This is not protection
against a hostile process with the same user's ability to modify the install
or inject into that executable. The trial and any recovery session deliberately
skip update discovery for their whole lifetime; an ordinary launch checks
again. If termination cannot be confirmed or current changed out of band,
the updater stops automatic reopening. This is not a persistent journal:
forced termination of the updater itself or power loss can interrupt recovery,
and a forced exit during the two manifest renames can leave `previous.json`
different even though `current.json` points to a verified build.

For manual UI QA when two authentic builds with the readiness protocol already
exist, stage the older interactive browser, its original launcher
(`RustBrowser.exe`), and a matching
`current.json` under a disposable process-local `LOCALAPPDATA\RustBrowser`.
Launch the versioned executable directly, with a relative, nondefault
`--profile` and a working directory different from that of the launcher.
Do **not** run the installer or publish a release for this check.

1. Open two tabs. Wait for the update icon; check the hover label and **Alt+U**.
   Verify that neither manifest changed and there is no new version directory
   before selecting **Download update**.
2. Select **Download update**. Verify that progress is visible, the panel can be
   dismissed with **Esc** and reopened, and repeated clicks don't start another
   job. When the panel says the update is ready, verify `current.json` and
   `previous.json` are unchanged and the staged hash matches the release.
3. To test late publication failure, hold `current.json` open without delete
   sharing during the trial. Verify the trial stops and the old build reopens
   with the same tabs. Remove the lock; a verified stage can be reused. The
   old window should stay open if preflight fails before the trial begins.
4. Set normal startup to **Start page**, then select **Restart and update**.
   Verify `previous.json` is the original current and the new native window
   opens the same profile, tabs and active tab without a profile-lock error.
   An ordinary subsequent launch must still respect the Start page preference.
   Repeat in headless/smoke-window mode or from an unmanaged copy and verify
   no updater check or icon.
