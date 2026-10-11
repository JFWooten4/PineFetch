import assert from 'node:assert/strict';
import { once } from 'node:events';
import { readFile } from 'node:fs/promises';
import { chromium, webkit } from 'playwright';
import { createDevServer } from '../scripts/dev-server.mjs';
import { installScreenshotBridge } from '../scripts/screenshot-fixtures.mjs';

// Exercise the production markup and modules with isolated native API responses.
// No real downloads, history deletion, settings changes or external requests.
const { version } = JSON.parse(await readFile(new URL('../package.json', import.meta.url), 'utf8'));
const metadata = JSON.parse(await readFile(new URL('./fixtures/screenshots/videos.json', import.meta.url), 'utf8'));
const videos = await Promise.all(
    metadata.slice(0, 2).map(async video => ({
        ...video,
        duration: 510,
        thumbnail: `data:image/jpeg;base64,${(
            await readFile(new URL(`./fixtures/screenshots/${video.thumbnail}`, import.meta.url))
        ).toString('base64')}`,
    }))
);

const installUiResponses = () => {
    const invoke = globalThis.__TAURI__.core.invoke;
    const listen = globalThis.__TAURI__.event.listen;
    const listeners = new Map();
    let config;
    let historySources = ['youtube', 'tiktok'];
    let historyEntries = null;
    let historyCleared = false;
    let releasePresets;
    const presetsReady = new Promise(resolve => {
        releasePresets = resolve;
    });
    const ui = {
        commands: [],
        copied: '',
        ocrRecords: {},
        ocrProgress: {},
        ocrAttempt: 0,
        holdOcr: false,
        releaseOcr: null,
        holdFrame: false,
        releaseFrame: null,
        failNextFrame: false,
        failNextOcrDelete: false,
        failNextModelSave: false,
        releasePresets,
        setHistorySources: sources => {
            historySources = [...sources];
        },
        setHistoryEntries: entries => {
            historyEntries = entries;
        },
        emit: (name, payload) => {
            for (const handler of listeners.get(name) || []) handler({ payload });
        },
    };
    globalThis.__pinefetchUi = ui;
    Object.defineProperty(globalThis.navigator, 'clipboard', {
        value: {
            writeText: async text => {
                ui.copied = text;
            },
        },
    });
    globalThis.__TAURI__.event.listen = async (name, handler) => {
        if (!listeners.has(name)) listeners.set(name, new Set());
        listeners.get(name).add(handler);
        return listen(name, handler);
    };
    globalThis.__TAURI__.core.invoke = async (command, args = {}) => {
        ui.commands.push({ command, args });
        if (command === 'get_download_presets') await presetsReady;
        if (command === 'get_config') {
            config ||= {
                ...(await invoke(command, args)),
                selected_preset_key: 'audio_opus',
                faster_whisper_model: 'medium',
            };
            return { ...config };
        }
        if (command === 'patch_config') {
            if (ui.failNextModelSave && Object.hasOwn(args.changes, 'faster_whisper_model')) {
                ui.failNextModelSave = false;
                throw new Error('Fixture model save failed.');
            }
            config = { ...config, ...args.changes };
            return { ...config };
        }
        if (command === 'get_history_stats') {
            if (historyCleared) {
                return { video_count: 0, total_file_size_bytes: 0, total_duration_seconds: 0, source_counts: [] };
            }
            const stats = await invoke(command, args);
            return { ...stats, source_counts: historySources.map(source => ({ source, count: 1 })) };
        }
        if (command === 'get_history' && historyCleared) return { entries: [], has_more: false, total_count: 0 };
        if (command === 'get_history' && historyEntries) {
            const filtered = historyEntries.filter(entry => {
                if (args.source && entry.source !== args.source) return false;
                if (!args.query || args.searchField !== 'user') return true;
                if (args.exactUser) return entry.uploader === args.query;
                return entry.uploader?.toLowerCase().includes(args.query.toLowerCase());
            });
            const offset = args.offset || 0;
            const end = offset + args.limit;
            return {
                entries: filtered.slice(offset, end),
                has_more: filtered.length > end,
                total_count: filtered.length,
            };
        }
        if (command === 'get_history_details') {
            const customEntry = historyEntries?.find(entry => entry.id === args.id);
            if (customEntry)
                return { entry: customEntry, output_file_available: false, transcript: null, captions: [] };
            const details = await invoke(command, args);
            if (args.id === 'history-2')
                return { ...details, entry: { ...details.entry, thumbnail: null }, transcript: null, captions: [] };
            return details;
        }
        if (command === 'get_history_ocr') {
            const content = structuredClone(ui.ocrRecords[args.id] || null);
            if (content?.segments) content.result_revision = JSON.stringify(content.segments);
            if (ui.holdOcr) {
                ui.holdOcr = false;
                return new Promise(resolve => {
                    ui.releaseOcr = () => resolve(content);
                });
            }
            return content;
        }
        if (command === 'get_history_ocr_progress') return structuredClone(ui.ocrProgress[args.id] || null);
        if (command === 'get_history_ocr_frame') {
            if (ui.failNextFrame) {
                ui.failNextFrame = false;
                throw new Error('Fixture saved file is missing.');
            }
            const image = globalThis.document.querySelector('#historyDetailsThumbnail').src;
            if (ui.holdFrame) {
                ui.holdFrame = false;
                return new Promise(resolve => {
                    ui.releaseFrame = () => resolve(image);
                });
            }
            return image;
        }
        if (command === 'delete_history_ocr_observation') {
            if (ui.failNextOcrDelete) {
                ui.failNextOcrDelete = false;
                throw new Error('Fixture SQLite write failed.');
            }
            const content = ui.ocrRecords[args.id];
            if (content.last_attempt_status === 'running') throw new Error('Analysis is running');
            if (JSON.stringify(content.segments) !== args.selection.result_revision) throw new Error('Results changed');
            const segment = content.segments[args.selection.segment_index];
            segment.observed_at_ms = segment.observed_at_ms.filter(time => time !== args.selection.observed_at_ms);
            if (!segment.observed_at_ms.length) content.segments.splice(args.selection.segment_index, 1);
            content.text = '';
            return { ...structuredClone(content), result_revision: JSON.stringify(content.segments) };
        }
        if (command === 'start_history_ocr') {
            if (ui.ocrProgress[args.id]) return structuredClone(ui.ocrProgress[args.id]);
            if (Object.keys(ui.ocrProgress).length) throw new Error('An OCR analysis is already running.');
            const progress = {
                history_id: args.id,
                attempt_id: `ocr-${++ui.ocrAttempt}`,
                attempt_at: Date.now(),
                status: 'running',
                frames_checked: 0,
                total_duration_ms: null,
                total_frames: null,
                total_frames_is_estimate: false,
            };
            ui.ocrProgress[args.id] = progress;
            ui.ocrRecords[args.id] = {
                ...ui.ocrRecords[args.id],
                history_entry_id: args.id,
                last_attempt_id: progress.attempt_id,
                last_attempt_at: progress.attempt_at,
                last_attempt_status: 'running',
                last_error: null,
            };
            return structuredClone(progress);
        }
        if (command === 'cancel_history_ocr') {
            const progress = ui.ocrProgress[args.id];
            if (!progress || progress.attempt_id !== args.attemptId) throw new Error('Attempt is not running');
            ui.ocrRecords[args.id].last_attempt_status = 'cancelled';
            delete ui.ocrProgress[args.id];
            ui.emit('history:ocr', { ...progress, status: 'cancelled' });
            return;
        }
        if (command === 'get_history_transcript') {
            return {
                language: 'de',
                transcription_type: 'text',
                file_available: true,
                text: 'Transcript <img src=x onerror="alert(1)"> stays plain text.',
            };
        }
        if (command === 'get_history_caption') {
            return { format: 'txt', file_available: true, text: 'Caption content.' };
        }
        if (command === 'clear_history') {
            historyCleared = true;
            historySources = [];
            return true;
        }
        if (command === 'open_file_path') return true;
        if (command === 'revoke_link_dump_secret' || command === 'delete_link_dump_secret') return [];
        return invoke(command, args);
    };
};

const server = createDevServer();
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const origin = `http://127.0.0.1:${server.address().port}`;
try {
    for (const engine of [chromium, webkit]) {
        console.log(`[test:ui] Checking ${engine.name()}...`);
        const browser = await engine.launch();
        try {
            const context = await browser.newContext({
                viewport: { width: 1400, height: 960 },
                timezoneId: 'Europe/Vienna',
                reducedMotion: 'reduce',
            });
            await context.route('**/*', route => {
                const url = route.request().url();
                if (new URL(url).origin === origin) return route.continue();
                if (new URL(url).hostname === 'i.ytimg.com' && route.request().resourceType() === 'image') {
                    return route.fulfill({
                        contentType: 'image/jpeg',
                        body: Buffer.from(videos[0].thumbnail.split(',')[1], 'base64'),
                    });
                }
                if (url === 'https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest') {
                    return route.fulfill({ json: { tag_name: '2026.09.19' } });
                }
                return route.abort('blockedbyclient');
            });
            await context.addInitScript({
                content: `(${installScreenshotBridge.toString()})(${JSON.stringify({ videos, version })}); (${installUiResponses.toString()})();`,
            });
            const page = await context.newPage();
            const errors = [];
            const loadedFonts = [];
            page.on('pageerror', error => errors.push(error.message));
            page.on('console', message => {
                if (message.type() === 'error') errors.push(message.text());
            });
            page.on('response', response => {
                if (response.url().endsWith('.woff2')) {
                    assert.equal(response.status(), 200, response.url());
                    assert(response.url().startsWith(`${origin}/vendor/oj/assets/`));
                    loadedFonts.push(response.url());
                }
            });
            await page.goto(origin);
            const presetTrigger = page.locator('#presetTrigger');
            const presetMenu = page.locator('#presetMenu');
            assert(await presetTrigger.isDisabled());
            assert(await page.locator('#startDownloadBtn').isDisabled());
            await page.evaluate(() => globalThis.__pinefetchUi.releasePresets());
            await page.waitForFunction(() => globalThis.__pinefetchScreenshot?.ready);
            await page.evaluate(() => globalThis.document.fonts.ready);
            assert.match(
                await page.locator('body').evaluate(node => globalThis.getComputedStyle(node).fontFamily),
                /Comfortaa/
            );
            assert(await page.evaluate(() => globalThis.document.fonts.check('14px Comfortaa')));
            assert.match(
                await page
                    .locator('#viewSettingsBtn i')
                    .evaluate(node => globalThis.getComputedStyle(node, '::before').content),
                /[^"\s]/
            );
            await page.locator('#magicImportTrigger').focus();
            await page.locator('.oj-tooltip').waitFor();
            assert(await page.locator('#magicImportTrigger').getAttribute('aria-describedby'));
            await page.keyboard.press('Escape');
            await page.locator('.oj-tooltip').waitFor({ state: 'hidden' });

            // Saved format restoration and the library's radio-menu keyboard contract.
            assert.equal(await presetTrigger.isDisabled(), false);
            assert.equal(await page.locator('#presetValue').textContent(), 'Audio only (opus)');
            assert.equal(await page.locator('#presetSelect').count(), 0);
            assert.equal(await presetMenu.getByRole('menuitemradio', { includeHidden: true }).count(), 6);
            assert.equal(await presetMenu.locator('[aria-checked="true"]').count(), 1);
            assert.equal(await presetMenu.locator('[data-oj-value="audio_opus"]').getAttribute('aria-checked'), 'true');
            assert.equal(await page.locator('#transcriptionOptions').isVisible(), false);
            const assertPresetFocus = async key => {
                await page.waitForFunction(
                    value =>
                        globalThis.document.querySelector(`#presetMenu [data-oj-value="${value}"]`) ===
                        globalThis.document.activeElement,
                    key
                );
            };
            await presetTrigger.focus();
            await page.keyboard.press('ArrowDown');
            await presetMenu.waitFor();
            const triggerBox = await presetTrigger.boundingBox();
            const menuBox = await presetMenu.boundingBox();
            assert(Math.abs(menuBox.x - triggerBox.x) < 1);
            assert(menuBox.width <= triggerBox.width);
            assert.equal(await presetTrigger.getAttribute('aria-expanded'), 'true');
            await assertPresetFocus('best');
            await page.keyboard.press('ArrowDown');
            await assertPresetFocus('1080');
            await page.keyboard.press('End');
            await assertPresetFocus('text_timestamps');
            await page.keyboard.press('Home');
            await assertPresetFocus('best');
            await page.keyboard.press('ArrowUp');
            await assertPresetFocus('text_timestamps');
            await page.keyboard.press('t');
            await assertPresetFocus('text');
            await page.keyboard.press('t');
            await assertPresetFocus('text_timestamps');
            await page.keyboard.press('Escape');
            await presetMenu.waitFor({ state: 'hidden' });
            assert(await presetTrigger.evaluate(node => node === globalThis.document.activeElement));
            assert.equal(await presetTrigger.getAttribute('aria-expanded'), 'false');
            await page.keyboard.press('ArrowUp');
            await presetMenu.waitFor();
            await assertPresetFocus('text_timestamps');
            await page.keyboard.press('Escape');
            await presetMenu.waitFor({ state: 'hidden' });
            await presetTrigger.click();
            await presetMenu.waitFor();
            await page.locator('#urlInput').click();
            await presetMenu.waitFor({ state: 'hidden' });
            assert.equal(await presetTrigger.getAttribute('aria-expanded'), 'false');
            assert.equal(await page.locator('#presetValue').textContent(), 'Audio only (opus)');
            const selectPreset = async (key, label, transcription, keyboard = false) => {
                await presetTrigger.click();
                await presetMenu.waitFor();
                const item = presetMenu.locator(`[data-oj-value="${key}"]`);
                if (keyboard) {
                    await item.focus();
                    await page.keyboard.press('Enter');
                } else {
                    await item.click();
                }
                await presetMenu.waitFor({ state: 'hidden' });
                await page.waitForFunction(
                    value =>
                        globalThis.__pinefetchUi.commands.some(
                            call => call.command === 'patch_config' && call.args.changes.selected_preset_key === value
                        ),
                    key
                );
                assert.equal(await page.locator('#presetValue').textContent(), label);
                assert.equal(await presetMenu.locator('[aria-checked="true"]').count(), 1);
                assert.equal(await presetMenu.locator(`[data-oj-value="${key}"]`).getAttribute('aria-checked'), 'true');
                assert.equal(await page.locator('#transcriptionOptions').isVisible(), transcription);
                assert.equal(await presetTrigger.getAttribute('aria-expanded'), 'false');
                assert(await presetTrigger.evaluate(node => node === globalThis.document.activeElement));
            };
            await selectPreset('best', 'Best (bestvideo+bestaudio)', false);
            await selectPreset('audio_mp3', 'Audio only (mp3)', false);
            await selectPreset('text', 'Transcribe to text', true, true);
            await selectPreset('text_timestamps', 'Transcribe with timestamps', true);

            // Queue rendering and live native progress updates remain attached to jobs.
            await page.locator('#urlInput').fill(videos[0].url);
            await page.locator('#startDownloadBtn').click();
            await page.locator('.pinefetch-queue-item').waitFor();
            const transcriptRequest = await page.evaluate(
                () => globalThis.__pinefetchUi.commands.find(call => call.command === 'enqueue_download').args.request
            );
            assert.equal(transcriptRequest.format, 'ba/b');
            assert.equal(transcriptRequest.extract_audio, true);
            assert.equal(transcriptRequest.audio_format, 'mp3');
            assert.equal(transcriptRequest.transcribe_text, true);
            assert.equal(transcriptRequest.transcribe_timestamps, true);
            assert.equal(transcriptRequest.filename_suffix, '_timestamps');
            await page.evaluate(() => {
                globalThis.__pinefetchUi.emit('download:state', { id: 'screenshot-1', state: 'downloading' });
                globalThis.__pinefetchUi.emit('download:progress', { id: 'screenshot-1', percent: 42 });
            });
            await page.waitForFunction(() => globalThis.document.querySelector('progress.oj-progress')?.value === 42);
            await page.evaluate(() =>
                globalThis.__pinefetchUi.emit('download:progress', { id: 'screenshot-1', percent: 73 })
            );
            await page.waitForFunction(() => globalThis.document.querySelector('progress.oj-progress')?.value === 73);
            await page.evaluate(() =>
                globalThis.__pinefetchUi.emit('download:state', { id: 'screenshot-1', state: 'transcribing' })
            );
            await page.locator('.oj-progress-indeterminate').waitFor();
            const more = page.locator('.pinefetch-queue-more-btn');
            await more.click();
            await page.locator('#queueContextMenu').waitFor();
            await page.waitForFunction(() =>
                globalThis.document.getElementById('queueContextMenu').contains(globalThis.document.activeElement)
            );
            await page.keyboard.press('End');
            assert.equal(
                await page
                    .locator('#queueContextMenu button:not([hidden]):not(:disabled)')
                    .last()
                    .evaluate(node => node === globalThis.document.activeElement),
                true
            );
            await page.keyboard.press('Escape');
            assert(await more.evaluate(node => node === globalThis.document.activeElement));
            await more.click();
            await page.waitForFunction(() =>
                globalThis.document.getElementById('queueContextMenu').contains(globalThis.document.activeElement)
            );
            await page.keyboard.press('Home');
            await page.keyboard.press('Enter');
            await page.waitForFunction(url => globalThis.__pinefetchUi.copied === url, videos[0].url);
            await page.waitForFunction(
                () =>
                    globalThis.document.querySelector('.pinefetch-queue-more-btn') === globalThis.document.activeElement
            );

            await selectPreset('audio_opus', 'Audio only (opus)', false);
            await page.locator('#urlInput').fill(videos[1].url);
            await page.locator('#startDownloadBtn').click();
            await page.waitForFunction(
                () => globalThis.document.querySelectorAll('.pinefetch-queue-item').length === 2
            );
            const audioRequest = await page.evaluate(
                () =>
                    globalThis.__pinefetchUi.commands.filter(call => call.command === 'enqueue_download').at(-1).args
                        .request
            );
            assert.equal(audioRequest.format, 'ba/b');
            assert.equal(audioRequest.extract_audio, true);
            assert.equal(audioRequest.audio_format, 'opus');
            assert.equal(audioRequest.transcribe_text, false);
            assert.equal(audioRequest.transcribe_timestamps, false);
            assert.equal(audioRequest.filename_suffix, null);

            // History dropdowns preserve filters and clear a source that disappears.
            await page.locator('#viewHistoryBtn').click();
            assert.equal(await page.locator('#viewHistoryBtn').getAttribute('aria-current'), 'page');
            const searchInput = page.locator('#historySearchInput');
            const resultCount = page.locator('#historyResultCount');
            assert.equal(await resultCount.isVisible(), false);
            const searchFieldMenu = page.locator('#historySearchFieldMenu');
            const sourceMenu = page.locator('#historySourceMenu');
            await sourceMenu.locator('[data-oj-value="tiktok"]').waitFor({ state: 'attached' });
            assert.equal(await page.locator('#historySearchFieldSelect, #historySourceSelect').count(), 0);
            assert.equal(await page.locator('#historySearchFieldValue').textContent(), 'Title');
            assert.equal(await page.locator('#historySourceValue').textContent(), 'All sources');
            assert.equal(await searchFieldMenu.locator('[aria-checked="true"]').count(), 1);
            assert.equal(await sourceMenu.locator('[aria-checked="true"]').count(), 1);
            assert.deepEqual(
                await sourceMenu
                    .locator('[role="menuitemradio"]')
                    .evaluateAll(nodes => nodes.map(node => node.dataset.ojValue)),
                ['', 'youtube', 'tiktok']
            );
            const assertHistoryFilterFocus = async (menuId, value) => {
                await page.waitForFunction(
                    expected =>
                        globalThis.document.querySelector(`#${expected.menuId} [data-oj-value="${expected.value}"]`) ===
                        globalThis.document.activeElement,
                    { menuId, value }
                );
            };
            for (const [prefix, first, next, last] of [
                ['historySearchField', 'title', 'description', 'transcript'],
                ['historySource', '', 'youtube', 'tiktok'],
            ]) {
                const trigger = page.locator(`#${prefix}Trigger`);
                const menu = page.locator(`#${prefix}Menu`);
                await trigger.focus();
                await page.keyboard.press('ArrowDown');
                await menu.waitFor();
                await assertHistoryFilterFocus(`${prefix}Menu`, first);
                await page.keyboard.press('ArrowDown');
                await assertHistoryFilterFocus(`${prefix}Menu`, next);
                await page.keyboard.press('End');
                await assertHistoryFilterFocus(`${prefix}Menu`, last);
                await page.keyboard.press('Home');
                await assertHistoryFilterFocus(`${prefix}Menu`, first);
                await page.keyboard.press('Escape');
                await menu.waitFor({ state: 'hidden' });
                assert(await trigger.evaluate(node => node === globalThis.document.activeElement));
                assert.equal(await trigger.getAttribute('aria-expanded'), 'false');
                await page.keyboard.press('ArrowUp');
                await menu.waitFor();
                await assertHistoryFilterFocus(`${prefix}Menu`, last);
                await page.keyboard.press('Escape');
                await menu.waitFor({ state: 'hidden' });
                await trigger.click();
                await menu.waitFor();
                await searchInput.click();
                await menu.waitFor({ state: 'hidden' });
                assert.equal(await trigger.getAttribute('aria-expanded'), 'false');
            }
            const historyCommandCount = () => page.evaluate(() => globalThis.__pinefetchUi.commands.length);
            const historyReadCount = () =>
                page.evaluate(
                    () => globalThis.__pinefetchUi.commands.filter(call => call.command === 'get_history').length
                );
            const waitForHistoryRequest = async (after, searchField, source = '', query = '') => {
                await page.waitForFunction(
                    expected =>
                        globalThis.__pinefetchUi.commands
                            .slice(expected.after)
                            .some(
                                call =>
                                    call.command === 'get_history' &&
                                    call.args.searchField === expected.searchField &&
                                    (expected.source
                                        ? call.args.source === expected.source
                                        : !Object.hasOwn(call.args, 'source')) &&
                                    (expected.query
                                        ? call.args.query === expected.query
                                        : !Object.hasOwn(call.args, 'query'))
                            ),
                    { after, searchField, source, query }
                );
            };
            const selectHistoryFilter = async (prefix, value, label, keyboard = false) => {
                const trigger = page.locator(`#${prefix}Trigger`);
                const menu = page.locator(`#${prefix}Menu`);
                await trigger.click();
                await menu.waitFor();
                const item = menu.locator(`[data-oj-value="${value}"]`);
                if (keyboard) {
                    await item.focus();
                    await page.keyboard.press('Enter');
                } else {
                    await item.click();
                }
                await menu.waitFor({ state: 'hidden' });
                assert.equal(await page.locator(`#${prefix}Value`).textContent(), label);
                assert.equal(await menu.locator('[aria-checked="true"]').count(), 1);
                assert.equal(await menu.locator(`[data-oj-value="${value}"]`).getAttribute('aria-checked'), 'true');
                assert(await trigger.evaluate(node => node === globalThis.document.activeElement));
            };
            let beforeHistoryRequest = await historyCommandCount();
            await searchInput.fill('  history query  ');
            await waitForHistoryRequest(beforeHistoryRequest, 'title', '', 'history query');
            beforeHistoryRequest = await historyCommandCount();
            await selectHistoryFilter('historySearchField', 'description', 'Description');
            assert.equal(await searchInput.getAttribute('placeholder'), 'Search descriptions');
            await waitForHistoryRequest(beforeHistoryRequest, 'description', '', 'history query');
            beforeHistoryRequest = await historyCommandCount();
            await selectHistoryFilter('historySearchField', 'transcript', 'Transcript', true);
            assert.equal(await searchInput.getAttribute('placeholder'), 'Search transcripts');
            await waitForHistoryRequest(beforeHistoryRequest, 'transcript', '', 'history query');
            beforeHistoryRequest = await historyCommandCount();
            await selectHistoryFilter('historySource', 'youtube', 'YouTube');
            await waitForHistoryRequest(beforeHistoryRequest, 'transcript', 'youtube', 'history query');
            beforeHistoryRequest = await historyCommandCount();
            await selectHistoryFilter('historySource', '', 'All sources');
            await waitForHistoryRequest(beforeHistoryRequest, 'transcript', '', 'history query');
            beforeHistoryRequest = await historyCommandCount();
            await selectHistoryFilter('historySearchField', 'user', 'User', true);
            assert.equal(await searchInput.getAttribute('placeholder'), 'Search users');
            await waitForHistoryRequest(beforeHistoryRequest, 'user', '', 'history query');
            beforeHistoryRequest = await historyCommandCount();
            await selectHistoryFilter('historySource', 'youtube', 'YouTube');
            await waitForHistoryRequest(beforeHistoryRequest, 'user', 'youtube', 'history query');
            beforeHistoryRequest = await historyCommandCount();
            await selectHistoryFilter('historySource', 'tiktok', 'TikTok', true);
            await waitForHistoryRequest(beforeHistoryRequest, 'user', 'tiktok', 'history query');
            beforeHistoryRequest = await historyCommandCount();
            await page.evaluate(() => {
                globalThis.__pinefetchUi.setHistorySources(['youtube']);
                globalThis.__pinefetchUi.emit('history:changed', {});
            });
            await sourceMenu.locator('[data-oj-value="tiktok"]').waitFor({ state: 'detached' });
            await waitForHistoryRequest(beforeHistoryRequest, 'user', '', 'history query');
            assert.equal(await page.locator('#historySourceValue').textContent(), 'All sources');
            assert.equal(await sourceMenu.locator('[aria-checked="true"]').count(), 1);
            assert.equal(await sourceMenu.locator('[data-oj-value=""]').getAttribute('aria-checked'), 'true');
            beforeHistoryRequest = await historyCommandCount();
            await searchInput.fill('');
            await waitForHistoryRequest(beforeHistoryRequest, 'user');
            beforeHistoryRequest = await historyCommandCount();
            await selectHistoryFilter('historySearchField', 'title', 'Title');
            await waitForHistoryRequest(beforeHistoryRequest, 'title');
            assert.equal(await searchInput.getAttribute('placeholder'), 'Search titles');
            await page.waitForFunction(
                () => globalThis.document.querySelectorAll('.pinefetch-history-item').length === 2
            );

            // Creator shortcuts reset pagination and scope the exact stored name to its platform.
            await page.evaluate(() => {
                const creators = Array.from({ length: 25 }, (_, index) => ({
                    id: `creator-${index}`,
                    url: `https://example.com/${index}`,
                    title: `Blender tutorial ${index}`,
                    uploader: 'Blender',
                    source: 'youtube',
                }));
                globalThis.__pinefetchUi.setHistoryEntries([
                    { ...creators[0], id: 'similar-name', uploader: 'Blender Guru' },
                    { ...creators[0], id: 'other-platform', source: 'tiktok' },
                    { ...creators[0], id: 'missing-name', uploader: null },
                    ...creators,
                ]);
                globalThis.__pinefetchUi.emit('history:changed', {});
            });
            await page.waitForFunction(
                () => globalThis.document.querySelectorAll('.pinefetch-history-item').length === 20
            );
            await page.locator('#loadMoreHistoryBtn').click();
            await page.locator('[data-history-id="creator-24"]').waitFor();
            beforeHistoryRequest = await historyCommandCount();
            const lastCreator = page.locator('[data-history-id="creator-24"] .pinefetch-history-open-btn');
            await lastCreator.scrollIntoViewIfNeeded();
            await page.evaluate(
                () =>
                    new Promise(resolve =>
                        globalThis.requestAnimationFrame(() => globalThis.requestAnimationFrame(resolve))
                    )
            );
            await lastCreator.click({ button: 'right' });
            const creatorAction = page.getByRole('menuitem', {
                name: 'Show saved videos by this creator',
                exact: true,
            });
            assert(await creatorAction.isEnabled());
            await creatorAction.click();
            await waitForHistoryRequest(beforeHistoryRequest, 'user', 'youtube', 'Blender');
            await page.waitForFunction(() => {
                const entries = [...globalThis.document.querySelectorAll('.pinefetch-history-item')];
                return entries.length === 20 && entries.every(entry => entry.dataset.historyId.startsWith('creator-'));
            });
            assert.equal(await searchInput.inputValue(), 'Blender');
            assert.equal(await searchInput.getAttribute('placeholder'), 'Search users');
            assert.equal(await page.locator('#historySearchFieldValue').textContent(), 'User');
            assert.equal(await page.locator('#historySourceValue').textContent(), 'YouTube');
            assert.equal(await page.locator('#historyContextMenu').isVisible(), false);
            assert(await searchInput.evaluate(node => node === globalThis.document.activeElement));
            const latestHistoryArgs = () =>
                page.evaluate(
                    () => globalThis.__pinefetchUi.commands.findLast(call => call.command === 'get_history').args
                );
            assert.equal((await latestHistoryArgs()).offset, 0);
            assert.equal((await latestHistoryArgs()).exactUser, true);

            assert.equal(await resultCount.textContent(), '25 found');
            assert(await resultCount.isVisible());
            await page.setViewportSize({ width: 1000, height: 960 });
            const titleBounds = await page.locator('#leftPanelTitle').boundingBox();
            const countBounds = await resultCount.boundingBox();
            assert(countBounds.x > titleBounds.x + titleBounds.width);
            assert(Math.abs(countBounds.y + countBounds.height / 2 - titleBounds.y - titleBounds.height / 2) < 6);
            assert(await page.evaluate(() => globalThis.document.documentElement.scrollWidth <= globalThis.innerWidth));
            await page.setViewportSize({ width: 1400, height: 960 });
            await page.locator('#loadMoreHistoryBtn').click();
            await page.locator('[data-history-id="creator-24"]').waitFor();
            assert.equal(await page.locator('.pinefetch-history-item').count(), 25);
            assert.equal((await latestHistoryArgs()).offset, 20);
            assert.equal(await resultCount.textContent(), '25 found');
            assert.equal((await latestHistoryArgs()).exactUser, true);

            // Detail navigation preserves the exact query, all loaded rows and
            // the actual list nodes/scroll position without a History reload.
            await page.locator('#historyList').evaluate(list => {
                list.scrollTop = list.scrollHeight;
            });
            const retainedScroll = await page.locator('#historyList').evaluate(list => list.scrollTop);
            assert(retainedScroll > 0);
            const retainedRequests = await historyReadCount();
            await page.evaluate(() => {
                globalThis.__retainedHistoryRow = globalThis.document.querySelector('.pinefetch-history-item');
            });
            await page.locator('[data-history-id="creator-24"] .pinefetch-history-open-btn').click();
            await page.locator('#historyDetailsContent').waitFor();
            assert.equal(await page.locator('#historyDetailsTitle').textContent(), 'Blender tutorial 24');
            assert.equal(await page.locator('#historyDetailsView').evaluate(element => element.tagName), 'SECTION');
            assert.equal(await page.locator('dialog[open]').count(), 0);
            assert(await page.locator('#viewDownloadBtn').isVisible());
            const fullContainer = await page.locator('#historyDetailsView').boundingBox();
            assert(fullContainer.width >= 900);
            await page.locator('#historyDetailsBackBtn').click();
            await page.locator('#historyView').waitFor();
            assert.equal(await searchInput.inputValue(), 'Blender');
            assert.equal(await page.locator('#historySourceValue').textContent(), 'YouTube');
            assert.equal(await page.locator('#historySearchFieldValue').textContent(), 'User');
            assert.equal(await page.locator('.pinefetch-history-item').count(), 25);
            assert.equal(await historyReadCount(), retainedRequests);
            assert.equal(await page.locator('#historyList').evaluate(list => list.scrollTop), retainedScroll);
            assert(
                await page.evaluate(
                    () =>
                        globalThis.__retainedHistoryRow === globalThis.document.querySelector('.pinefetch-history-item')
                )
            );

            // A refresh and a view switch keep the shortcut; manual edits restore partial matching.
            beforeHistoryRequest = await historyCommandCount();
            await page.evaluate(() => globalThis.__pinefetchUi.emit('history:changed', {}));
            await waitForHistoryRequest(beforeHistoryRequest, 'user', 'youtube', 'Blender');
            assert.equal((await latestHistoryArgs()).exactUser, true);
            await page.locator('#viewDownloadBtn').click();
            assert.equal(await resultCount.isVisible(), false);
            await page.locator('#viewHistoryBtn').click();
            await resultCount.waitFor({ state: 'visible' });
            assert.equal(await resultCount.textContent(), '25 found');
            assert.equal(await searchInput.inputValue(), 'Blender');
            assert.equal(await page.locator('#historySourceValue').textContent(), 'YouTube');
            beforeHistoryRequest = await historyCommandCount();
            await searchInput.fill('Blend');
            await waitForHistoryRequest(beforeHistoryRequest, 'user', 'youtube', 'Blend');
            await page.locator('[data-history-id="similar-name"]').waitFor();
            assert.equal(await resultCount.textContent(), '26 found');
            assert.equal(Object.hasOwn(await latestHistoryArgs(), 'exactUser'), false);
            assert.equal((await latestHistoryArgs()).offset, 0);

            beforeHistoryRequest = await historyCommandCount();
            await searchInput.fill('No creator matches');
            await waitForHistoryRequest(beforeHistoryRequest, 'user', 'youtube', 'No creator matches');
            await page.waitForFunction(
                () => globalThis.document.getElementById('historyResultCount').textContent === '0 found'
            );
            assert(await resultCount.isVisible());

            // No creator metadata leaves the action disabled, including keyboard navigation.
            beforeHistoryRequest = await historyCommandCount();
            await searchInput.fill('');
            await waitForHistoryRequest(beforeHistoryRequest, 'user', 'youtube');
            assert.equal(await resultCount.isVisible(), false);
            await page.locator('[data-history-id="missing-name"] .pinefetch-history-open-btn').focus();
            await page.evaluate(
                () =>
                    new Promise(resolve =>
                        globalThis.requestAnimationFrame(() => globalThis.requestAnimationFrame(resolve))
                    )
            );
            await page.keyboard.press('Shift+F10');
            await page.locator('#historyContextMenu').waitFor();
            assert(await creatorAction.isDisabled());
            await page.waitForFunction(
                () => globalThis.document.getElementById('historyShowMoreDataBtn') === globalThis.document.activeElement
            );
            await page.keyboard.press('End');
            await page.waitForFunction(
                () => globalThis.document.getElementById('historyOpenOcrBtn') === globalThis.document.activeElement
            );
            await page.keyboard.press('Escape');
            await page.locator('#historyContextMenu').waitFor({ state: 'hidden' });

            // Restore the standard history fixtures for the detail and deletion checks.
            await page.evaluate(() => {
                globalThis.__pinefetchUi.setHistoryEntries(null);
                globalThis.__pinefetchUi.emit('history:changed', {});
            });
            beforeHistoryRequest = await historyCommandCount();
            await selectHistoryFilter('historySource', '', 'All sources');
            await waitForHistoryRequest(beforeHistoryRequest, 'user');
            beforeHistoryRequest = await historyCommandCount();
            await selectHistoryFilter('historySearchField', 'title', 'Title');
            await waitForHistoryRequest(beforeHistoryRequest, 'title');
            await page.locator('[data-history-id="history-2"]').waitFor();

            // Library tabs own roving focus, activation and lazy text fetching.
            await page.locator('.pinefetch-history-open-btn').first().focus();
            await page.keyboard.press('Shift+F10');
            await page.locator('#historyShowMoreDataBtn').click();
            await page.locator('#historyDetailsContent').waitFor();
            await page.locator('#historyDetailsOpenFileBtn').click();
            await page.locator('#historyDetailsOpenBrowserBtn').click();
            assert(
                await page.evaluate(() =>
                    globalThis.__pinefetchUi.commands.some(call => call.command === 'open_file_path')
                )
            );
            await page.locator('#historyOverviewTab').focus();
            await page.keyboard.press('ArrowRight');
            await page.locator('#historyTranscriptText').waitFor();
            assert.equal(await page.locator('#historyTranscriptTab').getAttribute('aria-selected'), 'true');
            assert.equal(await page.locator('#historyTranscriptText img').count(), 0);
            await page.locator('#historyTranscriptCopyBtn').click();
            await page.locator('.oj-toast').first().waitFor();
            assert.match(await page.evaluate(() => globalThis.__pinefetchUi.copied), /^Transcript <img/);
            await page.locator('#historyCaptionText').waitFor();
            await page.locator('#historyTranscriptTab').focus();
            await page.keyboard.press('ArrowRight');
            assert.equal(await page.locator('#historyOcrTab').getAttribute('aria-selected'), 'true');
            await page.keyboard.press('Home');
            await page.keyboard.press('ArrowRight');
            assert.equal(
                await page.evaluate(
                    () =>
                        globalThis.__pinefetchUi.commands.filter(call => call.command === 'get_history_transcript')
                            .length
                ),
                1
            );
            await page.locator('#historyDetailsBackBtn').click();
            await page.locator('#historyDetailsView').waitFor({ state: 'hidden' });
            await page.waitForFunction(
                () =>
                    globalThis.document.querySelector('.pinefetch-history-open-btn') ===
                    globalThis.document.activeElement
            );
            await page.locator('.pinefetch-history-open-btn').nth(1).focus();
            await page.keyboard.press('Shift+F10');
            await page.locator('#historyShowMoreDataBtn').click();
            await page.locator('#historyDetailsContent').waitFor();
            assert.equal(await page.locator('#historyOverviewTab').getAttribute('aria-selected'), 'true');
            assert.equal(await page.locator('#historyTranscriptTab').isDisabled(), false);
            assert(await page.locator('#historyTranscriptTab').isVisible());
            await page.locator('#historyTranscriptTab').click();
            assert.equal(
                await page.locator('#historyTranscriptStatus').textContent(),
                'No transcript is saved for this video.'
            );
            await page.locator('#historyDetailsBackBtn').click();
            await page.locator('#historyDetailsView').waitFor({ state: 'hidden' });

            // OCR is lazy, safe text, explicitly cancellable, and entry/request scoped.
            const openOcr = async (id, start = false) => {
                // Allow focus and layout to settle before opening the keyboard menu.
                await page.evaluate(
                    () =>
                        new Promise(resolve =>
                            globalThis.requestAnimationFrame(() => globalThis.requestAnimationFrame(resolve))
                        )
                );
                await page.locator(`[data-history-id="${id}"] .pinefetch-history-open-btn`).focus();
                await page.evaluate(
                    () =>
                        new Promise(resolve =>
                            globalThis.requestAnimationFrame(() => globalThis.requestAnimationFrame(resolve))
                        )
                );
                await page.keyboard.press('Shift+F10');
                assert.equal(
                    await page
                        .locator(`[data-history-id="${id}"] .pinefetch-history-open-btn`)
                        .getAttribute('aria-expanded'),
                    'true'
                );
                await page.locator('#historyOpenOcrBtn').click();
                await page.locator('#historyDetailsContent').waitFor();
                if (start) {
                    await page.locator('#historyOcrStartBtn').waitFor();
                    await page.locator('#historyOcrStartBtn').click();
                }
                await page.waitForFunction(
                    () => globalThis.document.getElementById('historyOcrTab').getAttribute('aria-selected') === 'true'
                );
            };
            const closeDetails = async () => {
                await page.locator('#historyDetailsBackBtn').click();
                await page.locator('#historyDetailsView').waitFor({ state: 'hidden' });
            };
            const expectOcrStatus = status =>
                page.waitForFunction(
                    expected => globalThis.document.getElementById('historyOcrStatus').textContent === expected,
                    status
                );
            const ocrProgressText = async () =>
                (await page.locator('#historyOcrProgressText').textContent()).replace(/\s+/g, ' ').trim();
            const ocrCounterLayout = () =>
                page.evaluate(() =>
                    [
                        'historyOcrTime',
                        'historyOcrTimeTotal',
                        'historyOcrFrames',
                        'historyOcrFramesTotal',
                        'historyOcrProgressText',
                        'historyOcrCancelBtn',
                        'historyDetailsThumbnail',
                    ].map(id => {
                        const { x, y, width, height } = globalThis.document.getElementById(id).getBoundingClientRect();
                        return { id, x, y, width, height };
                    })
                );
            const checkOcrLayout = async (state, { results = true } = {}) => {
                while (await page.locator('.oj-toast-dismiss').count())
                    await page.locator('.oj-toast-dismiss').first().click();
                for (const viewport of [
                    { width: 1000, height: 960 },
                    { width: 1600, height: 1100 },
                ]) {
                    await page.setViewportSize(viewport);
                    await page.locator('#historyOcrPanel').evaluate(el => {
                        el.scrollTop = 0;
                    });
                    const layout = await page.evaluate(() => {
                        const panel = globalThis.document.getElementById('historyOcrPanel');
                        const rect = selector => panel.querySelector(selector).getBoundingClientRect().toJSON();
                        const info = panel.querySelector('.pinefetch-ocr-preview-info');
                        return {
                            actions: rect('.pinefetch-ocr-actions'),
                            start: rect('#historyOcrStartBtn'),
                            copy: rect('#historyOcrCopyBtn'),
                            startVisible: !panel.querySelector('#historyOcrStartBtn').hidden,
                            viewer: rect('#historyOcrPreview'),
                            frame: rect('.pinefetch-ocr-frame-stage'),
                            info: rect('.pinefetch-ocr-preview-info'),
                            results: rect('#historyOcrResults'),
                            heading: rect('#historyOcrResultsTitle'),
                            search: rect('#historyOcrSearchInput'),
                            count: rect('#historyOcrResultCount'),
                            list: rect('#historyOcrText'),
                            rightSide: [
                                'historyOcrPreviewTitle',
                                'historyOcrFrameTime',
                                'historyOcrSelectedText',
                                'historyOcrMeta',
                                'historyOcrNotice',
                                'historyOcrActionNotice',
                                'historyOcrPreviousResult',
                                'historyOcrAnalysis',
                            ].every(id => info.contains(globalThis.document.getElementById(id))),
                            overflow: panel.scrollWidth > panel.clientWidth,
                            nestedScrollers: [...panel.querySelectorAll('*')].filter(
                                el =>
                                    /auto|scroll/.test(globalThis.getComputedStyle(el).overflowY) &&
                                    el.scrollHeight > el.clientHeight
                            ).length,
                            directSections: [...panel.children].map(el => el.id || el.className),
                        };
                    });
                    assert(layout.rightSide);
                    assert.equal(layout.overflow, false);
                    assert.equal(layout.nestedScrollers, 0);
                    assert.deepEqual(layout.directSections, [
                        'oj-toolbar pinefetch-ocr-actions',
                        'historyOcrPreview',
                        'historyOcrResults',
                    ]);
                    assert(layout.viewer.top >= layout.actions.bottom);
                    assert(layout.info.left >= layout.frame.right);
                    assert(Math.abs(layout.frame.width / (layout.frame.width + layout.info.width) - 0.64) < 0.01);
                    assert(layout.frame.height > 240);
                    assert(layout.frame.height <= viewport.height * 0.4 + 2);
                    assert(Math.abs(layout.copy.right - layout.actions.right) < 1);
                    assert(layout.copy.height >= 36);
                    if (layout.startVisible) {
                        assert.equal(layout.start.height, layout.copy.height);
                        assert(layout.start.right < layout.copy.left);
                    }
                    if (results) {
                        assert(layout.results.top >= layout.viewer.bottom);
                        assert(layout.results.top - layout.viewer.bottom <= 24);
                        assert(Math.abs(layout.results.width - layout.viewer.width) < 1);
                        assert(layout.search.top >= layout.heading.bottom);
                        assert(layout.count.top >= layout.search.bottom);
                        assert(layout.list.top >= layout.count.bottom);
                    }
                    assert(
                        await page
                            .locator('#historyOcrCopyBtn')
                            .evaluate(el => el.classList.contains('oj-button-primary'))
                    );
                    assert(
                        await page
                            .locator('#historyOcrStartBtn')
                            .evaluate(el => el.classList.contains('oj-button-secondary'))
                    );
                    assert.equal(await page.locator('#historyOcrCopyBtn').count(), 1);
                    assert.equal(await page.locator('#historyOcrStartBtn').count(), 1);
                    assert.doesNotMatch(
                        await page.locator('#historyOcrPanel').textContent(),
                        /Extract text appearing in video frames using Apple Vision|Times are observations from sampled frames|Analysis runs in the background/
                    );
                    assert.equal(
                        await page
                            .locator('#historyOcrPanel')
                            .getByRole('heading', { name: 'OCR Analysis', exact: true })
                            .count(),
                        0
                    );
                    await page.screenshot({
                        path: `/tmp/pinefetch-ocr-layout-${state}-${viewport.width}-${engine.name()}.png`,
                    });
                }
                await page.setViewportSize({ width: 1400, height: 960 });
            };
            await openOcr('history-1');
            await expectOcrStatus('Not checked yet');
            assert.equal(
                await page.evaluate(
                    () => globalThis.__pinefetchUi.commands.filter(call => call.command === 'start_history_ocr').length
                ),
                0
            );
            await closeDetails();
            await openOcr('history-1', true);
            await expectOcrStatus('Checking for OCR…');
            assert.deepEqual(await page.evaluate(() => Object.keys(globalThis.__pinefetchUi.ocrProgress)), [
                'history-1',
            ]);
            assert.equal(await page.locator('#historyOcrProgress').getAttribute('value'), null);
            assert(await page.locator('#historyOcrPreparing').isVisible());
            assert.equal(await page.locator('#historyOcrProgressText').isVisible(), false);
            assert(await page.locator('#historyDetailsThumbnail').isVisible());
            assert(await page.locator('#historyDetailsThumbnail').getAttribute('src'));
            await page.evaluate(() => {
                const progress = globalThis.__pinefetchUi.ocrProgress['history-1'];
                progress.frames_checked = 3;
                progress.observed_at_ms = 3020;
                progress.total_duration_ms = 60000;
                progress.total_frames = 60;
                progress.total_frames_is_estimate = true;
                globalThis.__pinefetchUi.emit('history:ocr', progress);
            });
            assert.equal(await ocrProgressText(), 'Video time 00:00:03 / 00:01:00 · Frames 3 / 60');
            assert(
                Math.abs(Number(await page.locator('#historyOcrProgress').getAttribute('value')) - 3020 / 60000) < 1e-9
            );
            await page.setViewportSize({ width: 1000, height: 960 });
            await page.evaluate(() => {
                const progress = globalThis.__pinefetchUi.ocrProgress['history-1'];
                Object.assign(progress, {
                    frames_checked: 741,
                    observed_at_ms: 740000,
                    total_duration_ms: 1305621,
                    total_frames: 1306,
                });
                globalThis.__pinefetchUi.emit('history:ocr', progress);
            });
            assert.equal(await ocrProgressText(), 'Video time 00:12:20 / 00:21:45 · Frames 741 / 1306');
            assert(await page.locator('#historyDetailsThumbnail').isVisible());
            assert(await page.locator('#historyOcrPanel').evaluate(el => el.scrollWidth <= el.clientWidth));
            await page.evaluate(() => globalThis.document.fonts.ready);
            const counterLayout = await ocrCounterLayout();
            for (const frames of [9, 10, 99, 100, 999, 1000]) {
                await page.evaluate(framesChecked => {
                    const progress = globalThis.__pinefetchUi.ocrProgress['history-1'];
                    Object.assign(progress, { frames_checked: framesChecked, observed_at_ms: framesChecked * 1000 });
                    globalThis.__pinefetchUi.emit('history:ocr', progress);
                }, frames);
                assert.deepEqual(await ocrCounterLayout(), counterLayout, `Counters shifted at ${frames} frames`);
                assert.equal(
                    await ocrProgressText(),
                    `Video time ${[Math.floor(frames / 3600), Math.floor(frames / 60) % 60, frames % 60].map(part => String(part).padStart(2, '0')).join(':')} / 00:21:45 · Frames ${frames} / 1306`
                );
            }
            // Final extraction can reduce the total's digit count without moving
            // separators, controls or the thumbnail.
            await page.evaluate(() => {
                const progress = globalThis.__pinefetchUi.ocrProgress['history-1'];
                Object.assign(progress, { frames_checked: 999, total_frames: 999, total_frames_is_estimate: false });
                globalThis.__pinefetchUi.emit('history:ocr', progress);
            });
            await expectOcrStatus('Finishing OCR…');
            assert.deepEqual(await ocrCounterLayout(), counterLayout);
            await page.evaluate(() => {
                const progress = globalThis.__pinefetchUi.ocrProgress['history-1'];
                Object.assign(progress, {
                    frames_checked: 741,
                    observed_at_ms: 740000,
                    total_frames: 1306,
                    total_frames_is_estimate: true,
                });
                globalThis.__pinefetchUi.emit('history:ocr', progress);
            });
            await page.screenshot({ path: `/tmp/pinefetch-ocr-progress-${engine.name()}.png` });
            await checkOcrLayout('running', { results: false });
            await page.setViewportSize({ width: 1400, height: 960 });
            await closeDetails();
            assert.equal(
                await page.evaluate(
                    () => globalThis.__pinefetchUi.commands.filter(call => call.command === 'cancel_history_ocr').length
                ),
                0
            );
            const backgroundRequests = await historyReadCount();
            assert.match(
                await page.locator('[data-history-id="history-1"] .pinefetch-history-ocr-indicator').textContent(),
                /processing/
            );
            await page.locator('#viewDownloadBtn').click();
            await page.evaluate(() => {
                const progress = globalThis.__pinefetchUi.ocrProgress['history-1'];
                Object.assign(progress, { frames_checked: 800, observed_at_ms: 800000 });
                globalThis.__pinefetchUi.emit('history:ocr', progress);
            });
            await page.locator('#viewHistoryBtn').click();
            await page.locator('#historyView').waitFor();
            assert.equal(await historyReadCount(), backgroundRequests);
            await page.locator('[data-history-id="history-2"] .pinefetch-history-open-btn').click();
            await page.locator('#historyDetailsContent').waitFor();
            assert.equal(await page.locator('#historyOverviewTab').getAttribute('aria-selected'), 'true');
            await page.locator('#historyOcrTab').click();
            await expectOcrStatus('Not checked yet');
            await page.locator('#historyOcrStartBtn').click();
            assert.match(await page.locator('#historyOcrNotice').textContent(), /Another OCR analysis is active/);
            assert.equal(
                await page.evaluate(
                    () => globalThis.__pinefetchUi.commands.filter(call => call.command === 'start_history_ocr').length
                ),
                1
            );
            assert.equal(
                await page.evaluate(() => globalThis.__pinefetchUi.ocrProgress['history-1'].frames_checked),
                800
            );
            await closeDetails();
            await openOcr('history-1');
            await expectOcrStatus('Checking for OCR…');
            assert.equal(await ocrProgressText(), 'Video time 00:13:20 / 00:21:45 · Frames 800 / 1306');
            await page.locator('#historyOverviewTab').click();
            await page.locator('#historyTranscriptTab').click();
            await page.locator('#historyOcrTab').click();
            await expectOcrStatus('Checking for OCR…');
            assert.equal(
                await page.evaluate(
                    () => globalThis.__pinefetchUi.commands.filter(call => call.command === 'start_history_ocr').length
                ),
                1
            );
            await page.locator('#historyOcrCancelBtn').click();
            await expectOcrStatus('OCR cancelled');
            await closeDetails();
            await openOcr('history-1', true);
            await page.evaluate(() => {
                const progress = globalThis.__pinefetchUi.ocrProgress['history-1'];
                Object.assign(progress, {
                    frames_checked: 4,
                    observed_at_ms: 3520,
                    total_duration_ms: 3560,
                    total_frames: 4,
                    total_frames_is_estimate: false,
                });
                globalThis.__pinefetchUi.emit('history:ocr', progress);
            });
            await expectOcrStatus('Finishing OCR…');
            assert.equal(await ocrProgressText(), 'Video time 00:00:03 / 00:00:03 · Frames 4 / 4');
            assert(Number(await page.locator('#historyOcrProgress').getAttribute('value')) < 1);
            await closeDetails();
            await openOcr('history-2');
            await expectOcrStatus('Not checked yet');
            await page.evaluate(() => {
                const ui = globalThis.__pinefetchUi;
                const progress = ui.ocrProgress['history-1'];
                ui.ocrRecords['history-1'] = {
                    ...ui.ocrRecords['history-1'],
                    last_attempt_status: 'completed',
                    result_created_at: 1700000000000,
                    text: 'plain text',
                    segments: [
                        { text: '<img src=x onerror=alert(1)> 12!', observed_at_ms: [3020, 9030] },
                        { text: 'Second text', observed_at_ms: [4030] },
                    ],
                };
                delete ui.ocrProgress['history-1'];
                ui.emit('history:ocr', { ...progress, status: 'completed' });
            });
            assert.equal(await page.locator('#historyOcrStatus').textContent(), 'Not checked yet');
            await closeDetails();
            assert.match(
                await page.locator('[data-history-id="history-1"] .pinefetch-history-ocr-indicator').textContent(),
                /completed/
            );
            await openOcr('history-1');
            await expectOcrStatus('OCR complete');
            assert(await page.locator('#historyDetailsThumbnail').isVisible());
            await page.locator('#historyOcrText').waitFor();
            assert.equal(await page.locator('#historyOcrText img').count(), 0);
            assert.equal(await page.locator('#historyOcrText li').count(), 3);
            assert.deepEqual(await page.locator('#historyOcrText .pinefetch-ocr-select').allTextContents(), [
                '00:00:03',
                '00:00:04',
                '00:00:09',
            ]);
            await page.locator('#historyOcrSearchInput').fill('second');
            assert.equal(await page.locator('#historyOcrText li').count(), 1);
            assert.equal(await page.locator('#historyOcrResultCount').textContent(), '1 found');
            await page.locator('#historyOcrSearchInput').fill('');
            await page.setViewportSize({ width: 1000, height: 960 });
            assert(await page.locator('#historyOcrPanel').evaluate(el => el.scrollWidth <= el.clientWidth));
            await page.screenshot({ path: `/tmp/pinefetch-ocr-results-${engine.name()}.png` });
            await page.setViewportSize({ width: 1400, height: 960 });
            await page.locator('#historyOcrSearchInput').fill('No OCR matches');
            assert.equal(await page.locator('#historyOcrText li').count(), 0);
            assert.match(await page.locator('#historyOcrEmpty').textContent(), /No detected text matches/);
            await page.locator('#historyOcrCopyBtn').click();
            assert.match(
                await page.evaluate(() => globalThis.__pinefetchUi.copied),
                /^\[00:00:03.020, 00:00:09.030\] <img/
            );
            await page.locator('#historyOcrSearchInput').fill('');
            assert(await page.locator('#historyOcrPreview').isVisible());
            assert(await page.locator('#historyOcrFrame').isHidden());
            await page.locator('.pinefetch-ocr-select').first().focus();
            await page.keyboard.press('Enter');
            await page.locator('#historyOcrFrame').waitFor();
            assert(
                await page
                    .locator('.pinefetch-ocr-select')
                    .first()
                    .evaluate(el => el === globalThis.document.activeElement)
            );
            assert.equal(await page.locator('#historyOcrFrameTime').textContent(), '00:00:03');
            assert.match(await page.locator('#historyOcrSelectedText').textContent(), /^<img/);
            assert.equal(await page.locator('#historyOcrSelectedText img').count(), 0);
            const frameCall = await page.evaluate(() =>
                globalThis.__pinefetchUi.commands.findLast(call => call.command === 'get_history_ocr_frame')
            );
            assert.equal(frameCall.args.id, 'history-1');
            assert.equal(frameCall.args.selection.segment_index, 0);
            assert.equal(frameCall.args.selection.observed_at_ms, 3020);
            await page.evaluate(() => {
                globalThis.__pinefetchUi.holdFrame = true;
            });
            await page.locator('.pinefetch-ocr-select').nth(2).click();
            await page.waitForFunction(() => Boolean(globalThis.__pinefetchUi.releaseFrame));
            await page.locator('.pinefetch-ocr-select').nth(1).click();
            await page.locator('#historyOcrFrame').waitFor();
            await page.evaluate(() => globalThis.__pinefetchUi.releaseFrame());
            assert.equal(await page.locator('#historyOcrFrameTime').textContent(), '00:00:04');
            assert.equal(await page.locator('#historyOcrSelectedText').textContent(), 'Second text');
            assert.equal(await page.locator('#historyOcrEntryCount').textContent(), '3');
            assert(await page.locator('#historyOcrAnalyzed').textContent());
            assert(await page.locator('#historyOcrPreviousResult').isHidden());
            assert.equal(
                await page.locator('#historyOcrFrame').evaluate(el => globalThis.getComputedStyle(el).objectFit),
                'contain'
            );
            await checkOcrLayout('completed');
            await page.setViewportSize({ width: 1000, height: 960 });
            assert(await page.locator('#historyOcrPanel').evaluate(el => el.scrollWidth <= el.clientWidth));
            await page.screenshot({ path: `/tmp/pinefetch-ocr-frame-${engine.name()}.png` });
            await page.setViewportSize({ width: 1400, height: 960 });
            await page.clock.install();
            await page.evaluate(() => {
                globalThis.__pinefetchUi.holdFrame = true;
                globalThis.__pinefetchUi.releaseFrame = null;
            });
            await page.locator('.pinefetch-ocr-select').first().click();
            await page.waitForFunction(() => Boolean(globalThis.__pinefetchUi.releaseFrame));
            assert.equal(await page.locator('#historyOcrFrameStatus').textContent(), 'Loading selected frame…');
            await page.clock.fastForward(30001);
            await page.clock.resume();
            await page.waitForFunction(() =>
                globalThis.document.querySelector('#historyOcrFrameStatus').textContent.includes('too long')
            );
            assert.equal(await page.locator('#historyOcrPreview').getAttribute('aria-busy'), 'false');
            assert(await page.locator('#historyOcrFrame').isHidden());
            await page.evaluate(() => globalThis.__pinefetchUi.releaseFrame());
            assert(await page.locator('#historyOcrFrame').isHidden());
            await page.locator('.pinefetch-ocr-select').nth(1).click();
            await page.locator('#historyOcrFrame').waitFor();
            assert.equal(await page.locator('#historyOcrFrameTime').textContent(), '00:00:04');
            await page.evaluate(() => {
                globalThis.__pinefetchUi.failNextOcrDelete = true;
            });
            await page.locator('.pinefetch-ocr-delete').nth(1).click();
            await page.locator('#historyOcrActionNotice').waitFor();
            assert.equal(await page.locator('#historyOcrText li').count(), 3);
            assert.equal(await page.locator('#historyOcrSelectedText').textContent(), 'Second text');
            await page.locator('.pinefetch-ocr-delete').nth(1).click();
            await page.waitForFunction(() => globalThis.document.querySelectorAll('#historyOcrText li').length === 2);
            assert.equal(await page.locator('#historyOcrResultCount').textContent(), '2 detected entries');
            await page.locator('#historyOcrCopyBtn').click();
            assert.doesNotMatch(await page.evaluate(() => globalThis.__pinefetchUi.copied), /Second text/);
            await page.locator('#historyOcrSearchInput').fill('second');
            assert.equal(await page.locator('#historyOcrResultCount').textContent(), '0 found');
            await page.locator('#historyOcrSearchInput').fill('');
            await page.locator('.pinefetch-ocr-delete').first().click();
            await page.waitForFunction(() => globalThis.document.querySelectorAll('#historyOcrText li').length === 1);
            assert.equal(await page.locator('.pinefetch-ocr-select').textContent(), '00:00:09');
            await closeDetails();
            await openOcr('history-1');
            await expectOcrStatus('OCR complete');
            assert.equal(await page.locator('#historyOcrText li').count(), 1);
            await page.evaluate(() => {
                globalThis.__pinefetchUi.failNextFrame = true;
            });
            await page.locator('.pinefetch-ocr-select').click();
            await page.waitForFunction(() =>
                globalThis.document.querySelector('#historyOcrFrameStatus').textContent.includes('unavailable')
            );
            assert(await page.locator('#historyOcrFrame').isHidden());
            await closeDetails();
            await openOcr('history-1', true);
            assert(await page.locator('.pinefetch-ocr-delete').isDisabled());
            await page.evaluate(() => {
                const ui = globalThis.__pinefetchUi;
                const progress = ui.ocrProgress['history-1'];
                ui.ocrRecords['history-1'].last_attempt_status = 'error';
                ui.ocrRecords['history-1'].last_error = 'The saved video changed.';
                delete ui.ocrProgress['history-1'];
                ui.emit('history:ocr', { ...progress, status: 'error' });
            });
            await expectOcrStatus('OCR failed');
            await page.locator('#historyOcrText').waitFor();
            assert(await page.locator('#historyOcrPreviousResult').isVisible());
            assert.match(await page.locator('#historyOcrNotice').textContent(), /saved video changed/);
            assert(await page.locator('#historyOcrCopyBtn').isEnabled());
            assert.equal(await page.locator('#historyOcrStartBtn').textContent(), 'Retry Analysis');
            await page.locator('#historyOcrStartBtn').click();
            await expectOcrStatus('Checking for OCR…');
            assert(await page.locator('#historyOcrCopyBtn').isEnabled());
            await page.locator('#historyOcrCancelBtn').click();
            await expectOcrStatus('OCR cancelled');
            assert(await page.locator('#historyOcrPreviousResult').isVisible());
            await checkOcrLayout('cancelled-retry');
            await page.locator('.pinefetch-ocr-delete').click();
            await page.waitForFunction(
                () => globalThis.document.querySelector('#historyOcrResultCount').textContent === '0 detected entries'
            );
            assert(await page.locator('#historyOcrFrame').isHidden());
            assert(await page.locator('#historyOcrCopyBtn').isDisabled());
            assert.match(await page.locator('#historyOcrEmpty').textContent(), /No text was detected/);
            await closeDetails();
            await page.evaluate(() => {
                globalThis.__pinefetchUi.holdOcr = true;
            });
            await openOcr('history-1');
            await page.waitForFunction(() => Boolean(globalThis.__pinefetchUi.releaseOcr));
            await closeDetails();
            await openOcr('history-2');
            await expectOcrStatus('Not checked yet');
            await page.evaluate(() => globalThis.__pinefetchUi.releaseOcr());
            await page.evaluate(
                () =>
                    new Promise(resolve =>
                        globalThis.requestAnimationFrame(() => globalThis.requestAnimationFrame(resolve))
                    )
            );
            assert.equal(await page.locator('#historyOcrText').textContent(), '');
            assert.equal(await page.locator('#historyOcrStatus').textContent(), 'Not checked yet');
            assert.equal(await page.locator('#historyDetailsThumbnail').getAttribute('src'), null);
            await page.evaluate(() => {
                const ui = globalThis.__pinefetchUi;
                ui.ocrRecords['history-2'] = {
                    last_attempt_id: 'empty',
                    last_attempt_status: 'completed',
                    result_created_at: 1700000000000,
                    segments: [],
                    text: '',
                };
            });
            await page.locator('#historyOverviewTab').click();
            await page.locator('#historyOcrTab').click();
            await expectOcrStatus('OCR complete');
            assert.match(await page.locator('#historyOcrEmpty').textContent(), /No text was detected/);
            assert(await page.locator('#historyOcrCopyBtn').isDisabled());
            await page.evaluate(() => {
                const content = globalThis.__pinefetchUi.ocrRecords['history-2'];
                content.segments = Array.from({ length: 151 }, (_, index) => ({
                    text: `Detected entry ${index}`,
                    observed_at_ms: [index * 1000],
                }));
                content.text = 'Saved long OCR result';
            });
            await page.locator('#historyOverviewTab').click();
            await page.locator('#historyOcrTab').click();
            await page.waitForFunction(() => globalThis.document.querySelectorAll('#historyOcrText li').length === 100);
            await page.locator('#historyOcrLoadMoreBtn').click();
            assert.equal(await page.locator('#historyOcrText li').count(), 151);
            await page.locator('#historyOcrSearchInput').fill('entry 150');
            assert.equal(await page.locator('#historyOcrText li').count(), 1);
            assert.equal(await page.locator('#historyOcrLoadMoreBtn').isVisible(), false);
            await closeDetails();

            await openOcr('history-2', true);
            await expectOcrStatus('Checking for OCR…');
            assert.equal(await page.locator('#historyDetailsThumbnail').isVisible(), false);
            assert(
                await page.locator('#historyDetailsHeader').evaluate(el => el.classList.contains('pinefetch-no-media'))
            );
            await page.evaluate(() => {
                const progress = globalThis.__pinefetchUi.ocrProgress['history-2'];
                Object.assign(progress, { frames_checked: 1, observed_at_ms: 1310 });
                globalThis.__pinefetchUi.emit('history:ocr', progress);
            });
            assert.equal(await page.locator('#historyOcrProgress').getAttribute('value'), null);
            assert.equal(await ocrProgressText(), 'Video time 00:00:01 / Unknown · Frames 1 / Unknown');
            await page.locator('#historyOcrCancelBtn').click();
            await expectOcrStatus('OCR cancelled');
            await closeDetails();

            // A long saved result remains readable and fully copyable after an
            // interrupted retry, including observations beyond the rendered page.
            const longOcrText = `${'Vollständiger erkannter Text mit mehreren Zeilen und Umlauten. '.repeat(8)}\n${'SehrLangesWort'.repeat(20)}\nLetzte Zeile des ausgewählten Textes.`;
            await page.evaluate(text => {
                const content = globalThis.__pinefetchUi.ocrRecords['history-1'];
                content.segments = Array.from({ length: 801 }, (_, index) => ({
                    text: index === 0 ? text : `Saved observation ${index}`,
                    observed_at_ms: [index * 1000],
                }));
                content.text = 'Saved OCR result';
            }, longOcrText);
            await openOcr('history-1', true);
            await expectOcrStatus('Checking for OCR…');
            assert.equal(await page.locator('#historyOcrText li').count(), 100);
            assert(await page.locator('#historyOcrPreviousResult').isVisible());
            await page.locator('#historyOcrCopyBtn').click();
            assert.match(await page.evaluate(() => globalThis.__pinefetchUi.copied), /Saved observation 800/);
            await page.evaluate(() => {
                const ui = globalThis.__pinefetchUi;
                const progress = ui.ocrProgress['history-1'];
                ui.ocrRecords['history-1'].last_attempt_status = 'interrupted';
                ui.ocrRecords['history-1'].last_error = 'The application stopped before analysis finished.';
                delete ui.ocrProgress['history-1'];
                ui.emit('history:ocr', { ...progress, status: 'interrupted' });
            });
            await expectOcrStatus('OCR interrupted');
            await page.waitForFunction(
                () => globalThis.document.querySelector('#historyOcrEntryCount').textContent === '801'
            );
            assert(await page.locator('#historyOcrNotice').isVisible());
            assert(await page.locator('#historyOcrPreviousResult').isVisible());
            assert.equal(await page.locator('#historyOcrStartBtn').textContent(), 'Retry Analysis');
            await page.locator('#historyOcrSearchInput').fill('Letzte Zeile');
            assert.equal(await page.locator('#historyOcrResultCount').textContent(), '1 found');
            await page.locator('.pinefetch-ocr-select').click();
            await page.locator('#historyOcrFrame').waitFor();
            assert.equal(await page.locator('#historyOcrSelectedText').textContent(), longOcrText);
            await checkOcrLayout('interrupted-long-text');
            await page.locator('#historyOcrCopyBtn').click();
            const copiedSavedResult = await page.evaluate(() => globalThis.__pinefetchUi.copied);
            assert(copiedSavedResult.includes(longOcrText));
            assert.match(copiedSavedResult, /\[00:13:20\.000\] Saved observation 800/);
            await page.locator('#historyOcrSearchInput').fill('Saved observation 1');
            await page.locator('.pinefetch-ocr-select').first().click();
            await page.locator('#historyOcrFrame').waitFor();
            await checkOcrLayout('interrupted-retry');
            await page.locator('#historyOcrSearchInput').fill('');
            assert.equal(
                await page.locator('#historyOcrResultCount').textContent(),
                '801 detected entries · 100 shown'
            );
            await page.locator('#historyOcrLoadMoreBtn').click();
            assert.equal(await page.locator('#historyOcrText li').count(), 200);
            await page.setViewportSize({ width: 800, height: 960 });
            assert(await page.locator('#historyOcrPanel').evaluate(el => el.scrollWidth <= el.clientWidth));
            const stacked = await page.evaluate(() => {
                const frame = globalThis.document.querySelector('.pinefetch-ocr-frame-stage').getBoundingClientRect();
                const info = globalThis.document.querySelector('.pinefetch-ocr-preview-info').getBoundingClientRect();
                return info.top >= frame.bottom && Math.abs(info.width - frame.width) < 1;
            });
            assert(stacked);
            await page.setViewportSize({ width: 1400, height: 960 });
            await closeDetails();

            // Confirmation defaults to Cancel; cancellation never invokes deletion.
            await page.locator('#clearHistoryBtn').click();
            const confirmation = page.locator('dialog.oj-dialog[open]');
            await confirmation.waitFor();
            assert.equal(await page.evaluate(() => globalThis.document.activeElement.textContent), 'Cancel');
            await page.keyboard.press('Escape');
            await confirmation.waitFor({ state: 'hidden' });
            assert.equal(
                await page.evaluate(
                    () => globalThis.__pinefetchUi.commands.filter(call => call.command === 'clear_history').length
                ),
                0
            );
            await page.waitForFunction(
                () => globalThis.document.getElementById('clearHistoryBtn') === globalThis.document.activeElement
            );

            // Confirmed clearing resets both controls, the query and subsequent requests.
            beforeHistoryRequest = await historyCommandCount();
            await selectHistoryFilter('historySearchField', 'description', 'Description');
            await waitForHistoryRequest(beforeHistoryRequest, 'description');
            beforeHistoryRequest = await historyCommandCount();
            await selectHistoryFilter('historySource', 'youtube', 'YouTube');
            await waitForHistoryRequest(beforeHistoryRequest, 'description', 'youtube');
            beforeHistoryRequest = await historyCommandCount();
            await searchInput.fill('clear this search');
            await waitForHistoryRequest(beforeHistoryRequest, 'description', 'youtube', 'clear this search');
            beforeHistoryRequest = await historyCommandCount();
            await page.locator('#clearHistoryBtn').click();
            await page
                .locator('dialog.oj-dialog[open]')
                .getByRole('button', { name: 'Clear history', exact: true })
                .click();
            await page.waitForFunction(() => !globalThis.document.querySelector('dialog[open]'));
            await waitForHistoryRequest(beforeHistoryRequest, 'title');
            assert.equal(await searchInput.inputValue(), '');
            assert.equal(await searchInput.getAttribute('placeholder'), 'Search titles');
            assert.equal(await page.locator('#historySearchFieldValue').textContent(), 'Title');
            assert.equal(await page.locator('#historySourceValue').textContent(), 'All sources');
            assert.equal(await searchFieldMenu.locator('[aria-checked="true"]').count(), 1);
            assert.equal(await searchFieldMenu.locator('[data-oj-value="title"]').getAttribute('aria-checked'), 'true');
            assert.equal(await sourceMenu.locator('[aria-checked="true"]').count(), 1);
            assert.equal(await sourceMenu.locator('[data-oj-value=""]').getAttribute('aria-checked'), 'true');
            assert.equal(
                await page.evaluate(
                    () => globalThis.__pinefetchUi.commands.filter(call => call.command === 'clear_history').length
                ),
                1
            );

            await page.locator('#viewLinkDumpBtn').click();
            await page.locator('.pinefetch-link-dump-secret-item').waitFor();
            const revoke = page.getByRole('button', { name: 'Revoke', exact: true });
            await revoke.evaluate(node => {
                node.click();
                node.click();
            });
            await page.locator('dialog.oj-dialog[open]').waitFor();
            assert.equal(await page.locator('dialog.oj-dialog[open]').count(), 1);
            await page.keyboard.press('Escape');
            await page.waitForFunction(() => !globalThis.document.querySelector('dialog[open]'));
            assert.equal(
                await page.evaluate(
                    () =>
                        globalThis.__pinefetchUi.commands.filter(call => call.command === 'revoke_link_dump_secret')
                            .length
                ),
                0
            );
            await page.getByRole('button', { name: 'Delete', exact: true }).click();
            await page.locator('dialog.oj-dialog[open]').getByRole('button', { name: 'Delete', exact: true }).click();
            await page.locator('.pinefetch-link-dump-secret-item').waitFor({ state: 'hidden' });
            assert.equal(
                await page.evaluate(
                    () =>
                        globalThis.__pinefetchUi.commands.filter(call => call.command === 'delete_link_dump_secret')
                            .length
                ),
                1
            );

            await page.locator('#viewSettingsBtn').click();
            const modelTrigger = page.locator('#fasterWhisperModelTrigger');
            const modelMenu = page.locator('#fasterWhisperModelMenu');
            const modelValue = page.locator('#fasterWhisperModelValue');
            const saveStatus = page.locator('#settingsSaveStatus');
            assert.equal(await page.locator('#fasterWhisperModel').count(), 0);
            assert.equal(await modelValue.textContent(), 'High (medium)');
            assert.deepEqual(
                await modelMenu
                    .locator('[role="menuitemradio"]')
                    .evaluateAll(nodes =>
                        nodes.map(node => ({ value: node.dataset.ojValue, label: node.textContent.trim() }))
                    ),
                [
                    { value: 'base', label: 'Fast (base)' },
                    { value: 'small', label: 'Balanced (small)' },
                    { value: 'medium', label: 'High (medium)' },
                    { value: 'large-v3', label: 'Best (large-v3)' },
                ]
            );
            assert.equal(await modelMenu.locator('[aria-checked="true"]').count(), 1);
            assert.equal(await modelMenu.locator('[data-oj-value="medium"]').getAttribute('aria-checked'), 'true');
            await modelTrigger.focus();
            await page.keyboard.press('ArrowDown');
            await modelMenu.waitFor();
            await assertHistoryFilterFocus('fasterWhisperModelMenu', 'base');
            await page.keyboard.press('ArrowDown');
            await assertHistoryFilterFocus('fasterWhisperModelMenu', 'small');
            await page.keyboard.press('End');
            await assertHistoryFilterFocus('fasterWhisperModelMenu', 'large-v3');
            await page.keyboard.press('Home');
            await assertHistoryFilterFocus('fasterWhisperModelMenu', 'base');
            await page.keyboard.press('ArrowUp');
            await assertHistoryFilterFocus('fasterWhisperModelMenu', 'large-v3');
            await page.keyboard.press('h');
            await assertHistoryFilterFocus('fasterWhisperModelMenu', 'medium');
            await page.keyboard.press('Escape');
            await modelMenu.waitFor({ state: 'hidden' });
            assert(await modelTrigger.evaluate(node => node === globalThis.document.activeElement));
            assert.equal(await modelTrigger.getAttribute('aria-expanded'), 'false');
            assert.equal(await modelValue.textContent(), 'High (medium)');
            await page.keyboard.press('ArrowUp');
            await modelMenu.waitFor();
            await assertHistoryFilterFocus('fasterWhisperModelMenu', 'large-v3');
            await page.keyboard.press('Escape');
            await modelMenu.waitFor({ state: 'hidden' });
            await modelTrigger.click();
            await modelMenu.waitFor();
            await page.locator('#ytDlpPath').click();
            await modelMenu.waitFor({ state: 'hidden' });
            assert.equal(await modelTrigger.getAttribute('aria-expanded'), 'false');
            const waitForModelPatch = async (after, value) => {
                await page.waitForFunction(
                    expected =>
                        globalThis.__pinefetchUi.commands
                            .slice(expected.after)
                            .some(
                                call =>
                                    call.command === 'patch_config' &&
                                    Object.keys(call.args.changes).length === 1 &&
                                    call.args.changes.faster_whisper_model === expected.value
                            ),
                    { after, value }
                );
            };
            const selectModel = async (value, label, keyboard = false) => {
                const before = await historyCommandCount();
                await modelTrigger.click();
                await modelMenu.waitFor();
                const item = modelMenu.locator(`[data-oj-value="${value}"]`);
                if (keyboard) {
                    await item.focus();
                    await page.keyboard.press('Enter');
                } else {
                    await item.click();
                }
                await modelMenu.waitFor({ state: 'hidden' });
                await waitForModelPatch(before, value);
                await page.waitForFunction(
                    () =>
                        globalThis.document.getElementById('settingsSaveStatus').textContent ===
                        'Changes saved. They apply to new downloads.'
                );
                assert.equal(await modelValue.textContent(), label);
                assert.equal(await modelMenu.locator('[aria-checked="true"]').count(), 1);
                assert.equal(await item.getAttribute('aria-checked'), 'true');
                assert(await modelTrigger.evaluate(node => node === globalThis.document.activeElement));
            };
            await selectModel('base', 'Fast (base)');
            await selectModel('small', 'Balanced (small)', true);
            await selectModel('large-v3', 'Best (large-v3)');

            // A failed model save restores the persisted model and permits a retry.
            const beforeModelFailure = await historyCommandCount();
            await page.evaluate(() => {
                globalThis.__pinefetchUi.failNextModelSave = true;
            });
            await modelTrigger.click();
            await modelMenu.locator('[data-oj-value="medium"]').click();
            await modelMenu.waitFor({ state: 'hidden' });
            await waitForModelPatch(beforeModelFailure, 'medium');
            await page.waitForFunction(() =>
                globalThis.document
                    .getElementById('settingsSaveStatus')
                    .textContent.includes('Fixture model save failed.')
            );
            assert.match(await saveStatus.getAttribute('class'), /oj-status-error/);
            assert.equal(await modelValue.textContent(), 'Best (large-v3)');
            assert.equal(await modelMenu.locator('[aria-checked="true"]').count(), 1);
            assert.equal(await modelMenu.locator('[data-oj-value="large-v3"]').getAttribute('aria-checked'), 'true');
            assert(
                await page.evaluate(
                    after => globalThis.__pinefetchUi.commands.slice(after).some(call => call.command === 'get_config'),
                    beforeModelFailure
                )
            );
            await selectModel('medium', 'High (medium)', true);
            assert.equal(await saveStatus.evaluate(node => node.classList.contains('oj-status-error')), false);

            const setting = page.locator('#saveCaptions');
            await setting.focus();
            await page.keyboard.press('Space');
            await page.waitForFunction(() =>
                globalThis.__pinefetchUi.commands.some(
                    call => call.command === 'patch_config' && call.args.changes.save_captions === false
                )
            );
            await page.waitForFunction(() => !globalThis.document.getElementById('saveCaptions').disabled);
            assert.equal(await setting.isChecked(), false);

            // Verify desktop minimum size and compact browser layout after font loading.
            for (const width of [1000, 520]) {
                await page.setViewportSize({ width, height: 960 });
                assert(
                    await page.evaluate(() => globalThis.document.documentElement.scrollWidth <= globalThis.innerWidth)
                );
            }
            assert(loadedFonts.some(url => url.includes('comfortaa')));
            assert(loadedFonts.some(url => url.includes('jetbrains')));
            assert(loadedFonts.some(url => url.includes('fa-solid')));
            assert.deepEqual(errors, []);
            console.log(
                `[test:ui] ${engine.name()}: offline assets, preset restoration/keyboard/persistence, queue requests/progress, history filters/creator/reset, transcription model/save rollback, menus, tabs, full-container details, OCR background navigation/search, preserved History state, confirmations and switches passed.`
            );
        } finally {
            await browser.close();
        }
    }
} finally {
    await new Promise(resolve => server.close(resolve));
}
