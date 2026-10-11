# Local video OCR

Open a History entry to use its full-container detail workspace, or right-click
and choose **Open OCR** to select **OCR Text** directly. Opening the tab never
starts analysis. **Start OCR Analysis**, **Run OCR Again**, **Retry Analysis** and
**Start Again** explicitly start a job. One analysis may run at a time.
**Back to History**, other tabs and the global navigation leave it running;
**Cancel Analysis** is the only navigation-independent cancellation action.
History preserves its query, filters, loaded entries and scroll position.
The header displays the thumbnail during analysis and after completion.
Detected observations are chronological, searchable locally and displayed in
pages of 100. **Copy All Text** copies every observation's precise timestamps,
including text excluded by the current search. All processing and persistence
remain local; no player, cloud service or transcription engine is added.

Selecting a timestamp or detected text shows its actual saved-video frame in the
fixed overview above the independently scrolling results, alongside the timestamp
and selected text. FFmpeg seeks on the same normalized local PTS timeline and
verifies the extracted timestamp, including variable frame intervals and nonzero
origins. Previews are bounded to 1280 × 720 and returned as PNG data URLs without
writing frames to disk. Source SHA-256 is verified once per unchanged file identity;
changed or missing videos produce a preview error while saved text remains readable.
Preview utilities have a 30-second process timeout and app-exit cleanup, independent
of OCR cancellation. Late replies cannot replace another selection or entry.

Each observation has a **Delete** button. It removes only that text occurrence at
its timestamp, retaining other observations of the same segment. Empty segments
are removed; saved readable text, search, counts and copying update together.
Deleting the last observation leaves a valid empty saved result. Deletion persists
across restart and is disabled during analysis. A later successful **Run OCR Again**
replaces the edited result with newly recognized observations.

Only the entry's saved absolute `output_path` is eligible. FFprobe must identify
a real video stream, excluding attached cover art. Missing files, audio,
transcripts and missing/ambiguous paths produce English errors. No sibling
filenames are guessed and no download is started. Stored OCR remains readable
after the video disappears. The analyzed video's own SHA-256 is computed; the
History hash is never reused.

## Recognition and sampling

`ocr` orchestrates processes; `ocr_rules` owns grouping and validation;
`ocr_store` owns short SQLite transactions. `native/vision-ocr.swift` uses
`VNRecognizeTextRequest`, `.accurate`, the full image and no minimum text-height
cutoff. Language correction is **disabled**. Supported languages are queried
for the actual request revision. The helper prefers `de-DE` and `en-US`, then
supported variants with the same language prefix. If neither is available it
uses supported request defaults or the first supported language. Actual languages
and fallback use are saved. No language model rewrites text. See Apple's
[request API](https://developer.apple.com/documentation/vision/vnrecognizetextrequest)
and [correction setting](https://developer.apple.com/documentation/vision/vnrecognizetextrequest/useslanguagecorrection).

`SAMPLE_INTERVAL_SECONDS` in `ocr_rules.rs` is the central sampling constant,
initially **1.0**. FFmpeg selects the first frame, then the first frame at least
that interval after the previous selection. Full-resolution PNG frames stream
through `image2pipe` with passthrough timing and FFmpeg autorotation. `showinfo`
associates each frame with its real PTS; `-copyts -start_at_zero` expresses it on
the local file timeline, including nonzero timestamp origins. Times are never
calculated from file numbers or an assumed frame rate. Cut-video times refer to
the local cut alone. Sampling can miss brief text and does not prove exact
display intervals.

Whitespace alone is normalized. Exact text at comparable positions in consecutive
sampled frames joins a segment (bounding-box intersection-over-union at least
0.6). Missing observations break the sequence. Later reappearances, different
positions, numbers, URLs, handles, punctuation and negations remain distinct.
Boxes use Vision's normalized full-frame coordinates with a **bottom-left**
origin after orientation. Vision's enclosing rectangles can extend beyond the
image for partially visible text. Helper version **2** intersects them with the
normalized image rectangle before emitting observations. Wholly invisible
observations are omitted; invalid native sizes or non-finite coordinates fail.
Rust still rejects malformed replies and out-of-image boxes. The clipping policy
is saved in settings; text and confidence remain unchanged. Segments store the first box and all observed times
in milliseconds. Confidence validates native responses and is not shown as an
accuracy percentage.

A single private, uniquely created temporary directory holds one image at a
time; processed images are promptly removed. Pipes and two-message channels
bound buffering. Frames above 128 MiB, helper responses above 8 MiB, FFmpeg
diagnostic lines above 64 KiB, or results above a 32 MiB grouping/storage budget
fail cleanly. Cleanup checks the owned
directory's Unix inode and runs on completion, handled failure and cancellation.
A forced OS kill can leave an owned directory in the OS temporary area; startup
does not broadly purge ambiguous files.

## Storage and lifecycle

Additive schema **3** introduces one `history_ocr` row per History ID with
`ON DELETE CASCADE`. Published migrations 1 and 2 are unchanged. Existing snapshot,
writer-lock, rollback, compatibility and strict-layout checks apply.

`text`, `segments_json`, `source_sha256`, `engine`, `engine_version`,
`settings_json` and `result_created_at` describe the latest successful result.
They are nullable before first success. Empty success stores `""`, `[]` and a
result timestamp. Engine is `apple_vision`; `engine_version` is JSON containing
the actual Vision request revision, macOS version and helper version, not an
invented Apple package version. Settings record languages, correction policy,
orientation, sampling and processed frame count.

`last_attempt_id`, `last_attempt_status`, `last_attempt_at` and `last_error`
describe the latest attempt separately; times use epoch milliseconds. Statuses
are `running`, `completed`, `error`, `cancelled` and `interrupted`. Only a current,
still-running attempt can replace the successful result in a short guarded
transaction. Failed/cancelled reruns keep the old result. A failed commit is an
error even if saving its failure status also fails. Deletion cannot be undone
by a late result. No DB mutex or transaction spans decoding, Vision or hashing.
Source inode, size, mtime/ctime and a second SHA-256 verify the same file version;
the source identity and saved History path are rechecked before commit.

Result DTOs include a derived `result_revision` digest of source identity, result
timestamp and stored segment JSON. Preview and delete selections contain that
revision, segment index and observed time. Deletion validates them in an immediate
transaction and rewrites segments and readable text atomically; stale selections,
running analyses and failed writes leave the result intact. This adds no column or
migration and preserves engine/source metadata and the analysis timestamp.

OCR has its own process registry and finalization lock. Accepted cancellation
before finalization prevents success. Exit kills/reaps its processes and joins
the worker. `ocr-owner.lock` is a persistent inode with a kernel advisory lock;
only its owner marks old `running` attempts interrupted at startup. A second
desktop can read OCR, but cannot start it or interrupt the first instance. The
file is never deleted or declared stale by age; this supplements existing CLI
startup coordination.

All commands and `history:ocr` events use `tauri-client.js`. Progress carries
History ID, attempt ID, attempt start, actual frame count, last media PTS,
local video duration and sampled-frame total. FFprobe reads duration from the
saved file, preferring the selected video stream and accounting for timestamp
origins, including Matroska end-time tags. History's online duration is not used.
The UI displays **Video time HH:MM:SS / HH:MM:SS** without milliseconds,
**Frames checked / total**, the entry's thumbnail and a determinate bar based
on media time divided by local video duration. The frame total initially uses
`ceil(duration / sample interval)`; actual sampling can differ with variable
timestamps or gaps. Extraction's final count replaces the estimate. Counters
use monospaced digits and reserve widths from the full duration and total's
digit count. Widths do not shrink when extraction supplies the final total.
The bar stays below 100% during final source checks and saving;
the status reads **Finishing OCR…**. Missing duration keeps the bar
indeterminate and unknown totals are shown explicitly. Missing or failed
thumbnails do not reserve an empty image column.
`history-ocr-jobs.js` owns one app-wide event subscription and normalized job
states: `not_started`, `queued`, `running`, `completed`, `failed`, `cancelled`
and `interrupted`. `queued` represents a start request awaiting acknowledgement,
not an additional native processing queue. The backend's `error` status remains
compatible with schema 3 and maps to `failed` in the UI. Duplicate starts return
the existing native attempt; pending frontend requests are also deduplicated.
History pages include compact OCR summaries via one LEFT JOIN, without loading
OCR text or issuing a query per entry. Events update visible History indicators
even when another detail or app view is selected. Lazy result/progress snapshots
use revisions and attempt guards to reject stale data. Listener cleanup occurs
on app-page exit, never on detail navigation. Results use `textContent` and
previous results remain available until a successful rerun replaces them.

The existing SQLite schema 3 is sufficient; this UI redesign adds no migration.
Completed results survive restart and interrupted native jobs retain the existing
orphan recovery behavior. Progress is live in memory rather than restored after
restart. Internal errors are logged; the UI shows concise recovery instructions.

## Build and checks

On macOS Cargo's `build.rs` invokes `scripts/prepare-ocr-runtime.sh` before Tauri
copies resources. It compiles Cargo's target architecture into `ocr-runtime` as
`vision-ocr-arm64` or `vision-ocr-x86_64`, keeping architecture resources distinct.
Unchanged source, script, architecture, deployment target, Swift compiler and SDK
inputs reuse the existing helper; its checksum also detects damaged artifacts.
Failed compilation/signing preserves the previous helper and cache record. The
generated directory is ignored by the Tauri dev watcher; native sources and build
scripts remain watched, so helper output cannot trigger a rebuild loop.
The hook covers development, `build:check` and real app builds. Helpers are signed
ad hoc and included in the existing app deep-signing/verification flow. Runtime
resolution uses Tauri resources only; end users need neither Swift, xcrun, Xcode
nor developer paths.

The app's minimum macOS version is unchanged. Vision OCR requires 10.15 and is
availability-guarded; arm64's platform minimum is 11. Non-macOS builds do not
compile Apple code and return a macOS-only error. The documented release target
remains Apple silicon. Intel cross-linking is not verified with the available
arm64-only Swift compatibility archives; it needs a matching SDK/toolchain.

```sh
npm run check
npm run build:check
npm run test:ui
node scripts/test-ocr-native.mjs
```

Rust tests use synthetic stores and isolated processes for migration/retry,
retained and empty results, stale attempts, deletion, final cancellation, failed
storage, changed sources, malformed/early-ending helpers, instance ownership
and download isolation. An edge-box regression uses the native response that
previously stopped a real 4K video at 9 and 48 seconds. Shared golden cases cover
all four edges, invisible text and invalid geometry. The explicit native smoke
compiles and exercises the actual Swift intersection code, while Rust tests
check the clipped protocol output and continued rejection of invalid replies.
UI tests check lazy loading, background continuation, progress/cancel,
safe timestamped copying, retained results, empty success and
late replies after entry changes.

The explicit native smoke generates a VFR video with a nonzero timestamp origin,
checks selected PTS against ffprobe, recognizes synthetic German text with Vision
and runs the helper from an isolated cwd/system PATH. Its optional first argument
is the packaged helper path. Developer tools generate this test fixture only;
the shipped helper does not use them. No release/publishing occurs.

The detail-workspace redesign passes the complete check/build gates (50 JavaScript
and 213 Rust tests, including 15 OCR tests), Chromium/WebKit background-navigation,
filter/scroll retention, retry, search, pagination and copying checks, and
1000 × 960 layout checks.
The frame overview and observation deletion additionally pass the complete gates
(50 JavaScript and 216 Rust tests), keyboard selection, out-of-order previews,
delete failure/persistence/count/copy checks and Chromium/WebKit 1000 × 960 layout
checks. The native preview regression compares PNGs from fast seeks with an
independent full decode of synthetic VFR frames with a nonzero timestamp origin,
then verifies changed-source and stale-selection rejection.
The unchanged native OCR engine was separately verified on macOS 26.7 arm64:
real Vision revision 3 with `de-DE`/`en-US`, VFR times 0/1120/2400/3520 ms, a real
Tauri app bundle, existing ad-hoc deep signing/strict verification, and the bundled
helper smoke with isolated cwd/PATH. Intel and
older-macOS runtime behavior remain unverified, as does an interactive native
WebView smoke; browser checks use isolated native API fixtures. Windows/Linux
builds were not run in this macOS environment. No notarization or release was
performed.

The edge-box fix also passed all 60 selected full-resolution frames in the first
minute of the reported 4K video, through the real Vision helper and strict Rust
response validation, including the former failures at 9 and 48 seconds. This
diagnostic did not modify the personal History database or save an OCR result;
the entry needs a new **Run OCR Again** action in the updated app.

The dev watcher regression was checked with `npm run dev -- -- -- --version`:
touching the generated helper after watching starts does not restart the build,
and the compiled app reaches its version command. Two repeated real helper
preparations keep the helper and cache contents/mtimes unchanged. Isolated build
fixtures cover source/toolchain invalidation, relative versus absolute invocation,
failed compilation/signing, and missing/damaged artifacts.
