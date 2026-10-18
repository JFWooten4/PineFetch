# Keywords

Open a History video and select **Keywords**, after Overview, Transcript, OCR Text
and Social Insights. The tab mines the text already stored in the local SQLite
database. It makes no network request and opens no media or TXT files. It does
not start a transcription, OCR analysis or Social Insights fetch.

Only words occurring at least **5 times in total** across all saved sources are
included. Occurrences may come from one source or be spread across multiple
sources. Words occurring fewer than 5 times are excluded from the mining result.

The table shows occurrences per source and their total:

- **Transcript:** the saved speech transcription, with or without timestamps.
- **OCR:** text from every saved frame, including frames beyond the first viewer
  page and any unmapped legacy OCR text. Repeated text in different frames counts
  once per occurrence in each frame; remove unwanted duplicates in OCR first.
- **Captions:** all saved caption/post-description records for the video.

Matching ignores letter case, keeping the spelling of the first occurrence for
display. Words are Unicode alphanumeric sequences of 2–64 characters with at
least one letter; names such as Pokémon, AI, DSi and 4K are retained. Pure numbers,
timestamps and common German/English words are excluded. This includes articles:
**a, an, the** and **der, die, das, dem, den, des, ein, eine, einem, einen, einer,
eines**, regardless of letter case. Articles are excluded from all three sources,
even when repeated. This is word counting:
there is no stemming, plural merging, phrase extraction or semantic AI analysis.

Words are sorted by **Total descending**, with alphabetical order breaking ties. **Sources** filters to one source or all three;
**Search keywords** searches every term, including those beyond the initial 200
visible rows. **Load more keywords** expands the table in steps of 200.

Opening the tab or selecting **Refresh Keywords** reads a consistent snapshot of
the current saved texts, so deleted OCR frames and newly saved texts are reflected.
Derived keywords are not stored separately. Missing sources are shown explicitly;
if none have saved text, the tab explains what to create first. A failed refresh
keeps the previous results visible. Requests completing after navigation cannot
populate another video's results.

Mining is bounded to 128 MiB of saved UTF-8 text and 50,000 distinct accepted
terms per video. Exceeding either limit returns an error instead of silently
truncating the results.
