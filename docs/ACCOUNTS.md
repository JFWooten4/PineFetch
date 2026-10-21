# Accounts in History

Open a History entry, then click its account breadcrumb or its Creator in
Overview. The Account view lists that creator's saved downloads,
with **Load more saved videos** for accounts with more than 20 entries. A saved
video opens its existing details. Breadcrumbs show **History / Account** on an
account page and **History / Account / Post title** on a saved post, including
posts opened directly from History. Click the account breadcrumb to open its
saved downloads or **History** to return to the list with search, filters,
loaded entries and scroll position retained. The current page is plain text;
long names and titles are truncated with their full text available as a tooltip.
Posts without a saved creator omit the account breadcrumb.

Saved downloads are grouped by the exact stored creator name and platform, as
with History's existing creator filter. Similar names and names on different
platforms stay separate. Older History records do not contain a stable account
ID: two distinct accounts with the same display name on the same platform
cannot be distinguished from those records alone. No account URL is guessed
from a display name.

**Load Social Insights** resolves the selected video's public `channel_url` or
`uploader_url` through yt-dlp, validates that it is a profile on that platform,
then renders the public account page using Fetchary's **browser mode** and
bundled headless Chromium to extract its metrics. Refresh is
explicit. Available values, zeroes and missing reasons are displayed using the
same cards as video Social Insights. Account cards focus on followers, following,
posts and subscribers, show other metrics when available, and put available counts first. YouTube account insights have Fetchary's
`channel` kind; other supported profiles use `profile`.

For TikTok, PineFetch prefers `uploader_url`, which contains the public handle;
yt-dlp's `channel_url` can contain an internal account ID that the public page
does not expose as its handle. Refreshing unavailable insights resolves the
profile address again, including snapshots saved with that internal ID.
An identity mismatch preserves the previous snapshot instead of saving the
failed result.

Opening an account only reads SQLite. Saved snapshots remain readable offline
and after restarting PineFetch. A failed request, failed write or changed/deleted
History entry preserves previous snapshots. The native process has the existing
90-second metadata deadline and 90-second Fetchary process deadline, including
browser startup, navigation and a 5-second post-load wait. No cookies, HTML,
credentials or Fetchary CLI history are stored.

**Load profile videos** separately requests up to 50 public video references at
a time via yt-dlp. **Load more profile videos** advances through the accessible
list. YouTube uses the channel's Videos tab; Shorts and livestream tabs are not
included in this additional discovery list. The locally saved list includes all
media types/tabs already downloaded. Other platforms depend on yt-dlp's profile
support. Login walls, rate limits and missing profile metadata can prevent
listing. This feature does not guarantee a complete archive of an account.

Choose a public video to open the Download composer and load its metadata; the
usual preset and Start Download control apply. Discovered video references are
not saved to History or counted as downloads until a download completes.

The History statistics panel labels existing per-platform completed-download
counts **Downloads by platform**. Counts cover retained History entries,
including audio and transcript jobs; clearing History resets these totals.
The existing `PineFetch stats` CLI reports the same totals and platform counts.

## Storage and commands

Additive SQLite schema 6 adds `history_account_insights`, keyed by `(source,
uploader)`, with `profile_url`, `fetched_at`, `fetchary_version` and normalized
`payload_json` (maximum 256 KiB). The existing migration workflow creates a
backup before upgrading an existing database and rolls back the whole migration
on failure. An index on History's `(source, uploader)` supports account lookup.
Snapshots are shared by all matching History entries. Removing the last matching
entry or clearing History removes the account snapshot in the same transaction.

- `get_history_account`: local saved page and optional account snapshot.
- `load_history_account_insights`: explicit profile resolution, Fetchary fetch
  and guarded atomic snapshot write.
- `load_history_account_videos`: explicit, paginated yt-dlp profile discovery.

Tests use isolated databases, native process fixtures, real Fetchary extraction
on synthetic account HTML, and offline Chromium/WebKit UI sessions. No tests
modify the personal History database or download from a real social account.
