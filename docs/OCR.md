# Local video OCR

Right-click a History entry and choose **Check for OCR**. An accepted start opens
History Details on **OCR text**. Opening that tab by itself only loads saved data.
One analysis may run at a time. Closing the dialog lets it continue; **Cancel**
stops only OCR. **Copy text** includes all observed timestamps. All processing
and persistence are local; no player, gallery, automatic analysis, search or export
is added.

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
Contents load on demand; entry/request/attempt
guards reject late data. Subscriptions are cleaned up on page exit. OCR is
rendered with `textContent`, including when copied with readable timestamps.

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

Verified on macOS 26.7 arm64: the complete check/build gates (44 JavaScript and
211 Rust tests, including 13 OCR tests), Chromium/WebKit interaction checks and
1000 × 960 layout checks, real Vision revision 3 with `de-DE`/`en-US`, VFR times
0/1120/2400/3520 ms, a real Tauri app bundle, existing ad-hoc deep signing/strict
verification, and the bundled helper smoke with isolated cwd/PATH. Intel and
older-macOS runtime behavior remain unverified, as does an interactive native
WebView smoke; browser checks use isolated native API fixtures. Windows/Linux
builds were not run in this macOS environment. No notarization or release was
performed.

The edge-box fix also passed all 60 selected full-resolution frames in the first
minute of the reported 4K video, through the real Vision helper and strict Rust
response validation, including the former failures at 9 and 48 seconds. This
diagnostic did not modify the personal History database or save an OCR result;
the entry needs a new **Check for OCR** run in the updated app.

The dev watcher regression was checked with `npm run dev -- -- -- --version`:
touching the generated helper after watching starts does not restart the build,
and the compiled app reaches its version command. Two repeated real helper
preparations keep the helper and cache contents/mtimes unchanged. Isolated build
fixtures cover source/toolchain invalidation, relative versus absolute invocation,
failed compilation/signing, and missing/damaged artifacts.
