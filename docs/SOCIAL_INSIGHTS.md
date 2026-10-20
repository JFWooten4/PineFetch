# Social Insights

Each History video detail has a **Social Insights** tab next to Overview,
Transcript and OCR Text. Opening it reads the saved SQLite snapshot only. It
never starts a request, browser, login, download or scheduled refresh.

**Load Social Insights** explicitly fetches public metrics from that entry's
original URL. **Refresh Social Insights** performs another explicit request.
The result is saved locally and remains readable after app restarts and while
offline. A request already started continues when navigating away; its reply
cannot replace another entry's view. Reopening the original entry reads its
committed snapshot. Repeated clicks cannot start duplicate requests for the
same entry in one app instance.

The UI shows views, likes, comments, shares, saves, followers, following, posts
and subscribers. Supported fields depend on the platform and URL kind. A known
zero remains **0**. Unknown values remain **—**, with their availability reason.
Approximate public counts are labeled. The footer records fetch time and platform. The Fetchary version is retained
with the stored snapshot. These are saved snapshots, not continuously live counters.

## Fetchary integration

`native/social-insights.cjs` calls the **Fetchary library**, pinned in npm's
lockfile. It explicitly uses **browser mode**, a 20-second per-phase timeout,
a 5-second post-load wait, a 64 MiB page/resource limit and `cacheTtlMs: 0`. Each deliberate refresh performs a fresh capture. Fetchary
owns platform recognition, public-page extraction, redirect/identity checks and
private-network restrictions. PineFetch does not duplicate its platform parsers.

Each explicit load renders the public page in bundled headless Chromium, including
JavaScript and supported consent-overlay handling. Chromium uses a fresh temporary
profile, which is removed after the capture. It does not read personal browser
cookies or import a signed-in session. Hidden and private metrics may remain unavailable. Unsupported URL/platform kinds
are reported as unsupported. Network failures, blocked access, login requirements,
invalid replies and failed SQLite writes preserve the previous snapshot.

Node 26.10.0, the locked production dependencies and the Chromium headless shell
revision pinned by Puppeteer are bundled with the app; end users do not need
Node, Fetchary or an installed browser. Online extraction requires the bundled
Chromium runtime and its platform system libraries. The Node macOS binary
requires **macOS 13.5 or newer** for online Social Insights extraction. The app's
other features and reading saved snapshots retain their existing requirements.
Windows/Linux runtime preparation is implemented for native arm64/x64 hosts;
those app builds were not validated in this macOS workspace.

`npm run prepare:social-runtime` downloads Node's official archive, verifies its
SHA-256 against the versioned official checksum list, installs locked production
dependencies with lifecycle scripts disabled, explicitly downloads the pinned
Chromium headless shell into app resources, and verifies JavaScript rendering.
A second smoke test checks the relocated runtime before the previous runtime
is removed. Preparation is wired into Tauri dev/build and `build:check`.
Input fingerprints allow reuse; a failed preparation preserves the prior valid
runtime. Generated files are ignored by Git and the Tauri watcher. The adapter
runs from bundled resources, independently of PATH and the working directory.

## SQLite and command boundaries

Additive schema **5** creates `history_social_insights`, leaving published
migrations 1–4 unchanged. The existing pre-migration backup, writer lock,
transaction, layout validation and rollback mechanisms apply. Each row belongs
to a History entry through a cascading foreign key and contains:

- `history_entry_id`: one current snapshot per entry.
- `source_url`: the requested original video URL.
- `fetched_at`: local capture completion time in milliseconds.
- `fetchary_version`: the actual library version.
- `payload_json`: normalized metric values, status, sources and missing reasons.

Only normalized insights are stored; page HTML, authentication material and
Fetchary CLI history are not saved. Output and storage are limited to 256 KiB.
Metric counts must be nonnegative safe integers or null. A save rechecks the
History entry and its URL inside one SQLite transaction, so a deleted or changed
entry cannot be recreated or updated by a late response. History deletion/clearing
cascades to the snapshot. Fetching does not modify the video, transcript or OCR.

`get_history_social_insights` is a SQLite-only read.
`load_history_social_insights` is the explicit network-and-save action. The native
command runs off the UI thread, has a 90-second outer process deadline, rejects
concurrent requests for the same entry and uses the existing app-exit process
cleanup. On macOS/Linux, Chromium stays in the utility process group so native timeout/app-exit
cleanup terminates its child processes as well. There is no automatic collection at download completion or app startup.

## Validation

```sh
npm run check
npm run test:ui
npm run prepare:social-runtime
npm run test:social-browser
npm run build:check
```

Coverage includes real Fetchary fixture extraction, exact video identity, zero and
missing values, malformed replies, duplicate requests, failed refresh/save
retention, source changes, cascading deletion, schema-4 migration/backup/rollback,
and Chromium/WebKit tab, keyboard, loading, navigation, offline snapshot and
narrow-layout checks. Browser/native tests use isolated fixtures; no test writes
the personal History database.

Account profiles have their own [Account view and snapshots](ACCOUNTS.md),
separate from each video’s Social Insights.
