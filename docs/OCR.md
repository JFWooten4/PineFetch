# Local video OCR

Open a History entry to use its full-container detail workspace, or right-click
and choose **Open OCR** to select **OCR Text** directly. Opening the tab never
starts analysis. **Start OCR Analysis**, **Run OCR Again**, **Retry Analysis** and
**Start Again** explicitly start a job. One analysis may run at a time.
**Back to History**, other tabs and the global navigation leave it running;
**Cancel Analysis** is the only navigation-independent cancellation action.
History preserves its query, filters, loaded entries and scroll position.
The header displays the thumbnail during analysis and after completion.
Each sampled frame with retained OCR text is one saved result and one selectable
row, containing all of that frame's blocks with line breaks. Frames without text
have no result row; the independent progress counter still includes them. Distinct
frames remain distinct, including repeated captions and times within the same
whole display second. SQLite supplies pages of **100** frames in precise timestamp
order with stable ID tie-breaking. Search covers **all** saved frames using Unicode
case-insensitive literal substring matching, not just loaded pages. **Copy All Text**
exports all saved frames independently of filter and pagination:

```text
[00:00:08.020]
Der Preisbrecher kommt!!!
Die 16.11.
Kasse
Quelle: Archiv

[00:00:08.030]
...
```

Unstructured legacy text remains readable and copyable without an invented frame
or timestamp. All processing and persistence remain local.

Selecting a timestamp or detected text shows its actual saved-video frame in the
left side of the viewer. The upper action bar contains the analysis action,
explicit cancellation while running and **Copy All Text**. On desktop, the frame
occupies about 55% of the width; the complete searchable OCR list occupies the
right. Only the list scrolls, preserving the frame, search and compact analysis
footer. Narrow windows stack the columns within the available workspace height.
The timestamp sits directly below the frame; recognized text appears only in
the result rows. Arrow keys navigate focused results and load the next batch
when needed. Row selection updates in place without resetting the list's scroll.
Search retains a matching selection in the loaded page, otherwise selects the first match, and
clears the preview for an empty result. Late preview responses cannot override
a newer selection or search. FFmpeg seeks on the same normalized local PTS timeline and
verifies the extracted timestamp, including variable frame intervals and nonzero
origins. Previews are bounded to 1280 × 720 and returned as PNG data URLs without
writing frames to disk. Source SHA-256 is verified once per unchanged file identity;
changed or missing videos produce a preview error while saved text remains readable.
Previews have a 25-second native deadline covering cache-lock waiting, source
verification and both utility executions, plus app-exit cleanup independent of OCR
cancellation. The UI ends an unanswered request after 30 seconds with a retry
message; a late reply cannot replace that error, another selection or another entry.
SHA-256 compression is optimized in dev/test builds so large saved videos do not
spend tens of seconds in an unoptimized hash loop before their first preview.
The application itself keeps its normal debug build settings.

Each frame row has a keyboard-accessible **Delete frame text** icon and tooltip.
Its confirmation explicitly removes **all OCR blocks of that frame**, without
changing any other frame, video file or image. A short transaction removes the
row and advances the result revision together. Search counts, selection and full
export then reflect the saved rows. Deleting the selected frame selects a neighbor;
deleting another frame retains the selected ID where it remains in the loaded
page. Deleting the last frame leaves a valid empty successful result. Edits survive
restart and are disabled during analysis. A successful explicit **Run OCR Again**
atomically replaces them with newly recognized frames.

Only the entry's saved absolute `output_path` is eligible. FFprobe must identify
a real video stream, excluding attached cover art. Missing files, audio,
transcripts and missing/ambiguous paths produce English errors. No sibling
filenames are guessed and no download is started. Stored OCR remains readable
after the video disappears. The analyzed video's own SHA-256 is computed; the
History hash is never reused.

## Recognition and sampling

`ocr` orchestrates processes; `ocr_rules` owns frame-local reading order and validation;
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

New analyses still discard blocks shorter than four Unicode characters after
whitespace normalization **for the length check only**. Accepted original text,
internal line breaks and actually supplied confidence values are saved unchanged;
`minimum_text_characters` remains in settings. No correction, summarization or
semantic interpretation is applied. Text is not deduplicated across positions or
frames. `FrameBuilder` receives the helper's validated sample ID and real media PTS,
and creates exactly one result containing the frame's ordered blocks.

Vision boxes use normalized full-frame coordinates with a **bottom-left** origin
after orientation. Blocks are sorted by their **top edges**, grouped into approximate
lines by vertical overlap, then sorted left to right within each line. Equal-position
blocks retain delivery order. This deterministic geometric heuristic does not claim
to interpret complex posters or columns. Existing multiline block texts are kept.

Helper version **2** clips partially visible enclosing rectangles to the image;
wholly invisible observations are omitted, invalid sizes or non-finite coordinates
fail. Rust still rejects malformed replies and out-of-image boxes. The clipping
policy is recorded. Native blocks retain their actual box and confidence;
confidence is not shown as an accuracy percentage.

A single private, uniquely created temporary directory holds one image at a
time; processed images are promptly removed. Pipes and two-message channels
bound buffering. Frames above 128 MiB, helper responses above 8 MiB, FFmpeg
diagnostic lines above 64 KiB, or results above a 32 MiB frame/storage budget
fail cleanly. Cleanup checks the owned
directory's Unix inode and runs on completion, handled failure and cancellation.
A forced OS kill can leave an owned directory in the OS temporary area; startup
does not broadly purge ambiguous files.

## Storage and lifecycle

Additive schema **4** preserves published migrations 1–3 and the existing
`history_ocr` analysis/result metadata. It adds `result_revision` and `result_format`
to that parent and the related `history_ocr_frames` table:

- `id`: stable UUID of a saved frame result.
- `history_entry_id`: foreign key to `history_ocr`, with cascading deletion.
- `timestamp_ms`: actually observed local media timestamp, never rounded for grouping.
- `frame_key`: original sampled-frame number, nullable for unknown legacy identity.
- `full_text`: deterministic newline join of the ordered original block texts.
- `blocks_json`: original text, box, confidence and provenance for **one frame only**.

A chronological `(history_entry_id, timestamp_ms, id)` index supports pages; a
unique `(history_entry_id, frame_key)` index prevents duplicate native sample rows.
`full_text` is never independently editable. New results retire `segments_json`
and keep the old `text` column null; that column is used only for genuinely
unstructured or untimed legacy text, never as a competing structured result cache.
`source_sha256`, `engine`, `engine_version`, `settings_json` and `result_created_at`
continue describing the last successfully saved result. Empty success publishes a
revision and creation time with zero frame rows. Engine/version settings describe
the actual Vision request revision, OS and helper, languages, correction policy,
orientation, sampling, total checked frames and local duration.

The migration reads **every stored observation time** in each legacy segment and
groups only **exactly equal millisecond timestamps**. Equal text from distinct
segments remains separate blocks. Deleted observations are absent from the source
and are not recreated. No video is opened and no new analysis starts. Legacy
`frame_key`, precise `bounds` and `confidence` remain unknown. The old run's box
is retained explicitly as `legacy_segment_bounds`, with
`metadata_source: legacy_segment`, rather than presented as each later frame's
original box. Untimed segments remain readable as unmapped text; text-only results
have `result_format: legacy_text`. No timestamp patterns are inferred from text.

Migration uses the existing required pre-upgrade SQLite snapshot and one
transaction. Each inserted frame is read back and verified, and counts are checked
before the source JSON is retired. Verification compares the stored text and the
exact serialized block JSON, avoiding false failures from rounding when parsing
high-precision legacy coordinates again. Rollback retains the original schema and data;
versioning prevents duplicate transfers on restart. Legacy expansion is streamed
one frame at a time using references to the bounded source segments, without
materializing the entire expanded video text. Legacy observations are preserved
when expansion exceeds their old compressed size; **new** analyses still fail
cleanly at the existing 32 MiB budget. Frame text, encoded blocks and row overhead
are included in that budget. Tests cover two hours / 7,201 repeated frames and
hitting the limit without accepting an oversized frame.

`last_attempt_id`, `last_attempt_status`, `last_attempt_at` and `last_error` describe
the latest attempt separately. Only a current running attempt can publish a new
result, using prepared inserts in one short transaction **after** decoding and
source validation. Old rows are replaced only if that whole transaction commits.
Failed, cancelled, interrupted and failed-save attempts retain the prior rows and
revision. No transaction or DB mutex spans video decoding, Vision or hashing.
Source inode, size, mtime/ctime and a second SHA-256 verify the same file version;
the source identity and saved History path are rechecked before commit.

`get_history_ocr` returns metadata, total/matched frame counts and at most 100 rows
from a SQLite read snapshot (`limit`, `offset`, `search`, optional `resultRevision`).
It never reads the full structured video result. `get_history_ocr_text` separately
exports all rows chronologically. A small scalar function on the existing rusqlite
dependency preserves Unicode substring matching; no FTS or new dependency is added.
Preview and `delete_history_ocr_frame_text` accept a stable `frame_id` plus
`result_revision`. The backend resolves the timestamp itself and rejects stale
references after a successful rerun or deletion. Preview checks the reference again
after extracting the image; frontend request guards reject late pages and images.
Deletion changes only the chosen frame and revision, preserving engine/source
metadata and the original analysis time.

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
compatible with schema 4 and maps to `failed` in the UI. Duplicate starts return
the existing native attempt; pending frontend requests are also deduplicated.
History pages include compact OCR summaries via one LEFT JOIN, without loading
OCR text or issuing a query per entry. Events update visible History indicators
even when another detail or app view is selected. Lazy result/progress snapshots
use revisions and attempt guards to reject stale data. Listener cleanup occurs
on app-page exit, never on detail navigation. Results use `textContent` and
previous results remain available until a successful rerun replaces them.

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

Frame storage regressions cover multi-block frames, equal displayed seconds,
repeated text at different positions, empty results, all legacy observation times,
unknown metadata, migration interruption/rollback/restart, missing videos,
search outside the first page, complete export, frame-specific deletion,
atomic save failures, stale references and storage limits. Browser checks use
isolated native API responses and exercise search, pagination, copying,
confirmations, keyboard navigation, late pages/images and fixed/narrow layouts.
Native preview tests independently compare synthetic VFR PNGs by full decode.

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
