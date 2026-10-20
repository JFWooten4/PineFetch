# Account detail interface

The Account view uses PineFetch's existing dark theme, OJ surfaces, buttons,
badges, typography and spacing. It changes presentation only; account matching,
network requests, storage, pagination and download behavior retain their existing
contracts.

The header groups the platform, account name and saved-download total. Open
profile stays visible and disabled until a profile URL is available, with an
explanation. Once available, it opens the profile through the existing action.

Downloaded videos use compact rows with a file-type badge, title, filename and
Details action. Filenames truncate visually; their full text remains in the DOM
and native tooltip. On narrow windows, titles can occupy two lines. Each row has
one keyboard target and a visible focus ring.

Social Insights show KPI cards only for available values, including zero. Missing
profile metrics and their reasons remain accessible in an OJ disclosure. Opening,
refreshing and failed refreshes keep the existing snapshot behavior. No available
metrics produces a clear status and a compact list of missing values.

More videos is a separate public-profile section with initial, loading, empty,
error and populated states. A failed refresh preserves its previous rows. Loading
indicators use OJ's spinner and skeleton, hide decorative content from assistive
technology, expose busy state and prevent duplicate clicks. Selecting a video
still prepares its download in the existing composer.

Wide windows put downloads and public-profile discovery beside Social Insights.
Narrow windows use a single column in DOM and keyboard order. Lists scroll within
bounded regions, titles and filenames cannot force horizontal page overflow, and
the standard reduced-motion and touch-control rules remain in effect.

Validation uses `npm run check`, `npm run test:ui`, `npm run screenshots` and
`npm run build:check`. The offline Chromium/WebKit tests cover real keyboard focus,
compact unavailable metrics, zeroes, long filenames, pagination, profile opening,
local-read retry, loading/empty/error states, failed refresh retention and widths
of 390, 620, 800 and 1000 pixels. The Account screenshot is
`src/images/mockups/account.webp`.
