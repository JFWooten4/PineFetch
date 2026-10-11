import { formatLocalDateTime, formatMediaTime } from './formatters.js';
import { ocrActive, ocrErrorMessage, ocrStatus } from './history-ocr-jobs.js';
import { toast } from './vendor/oj/index.js';

const statusLabels = {
    queued: 'OCR pending',
    running: 'Checking for OCR…',
    completed: 'OCR complete',
    failed: 'OCR failed',
    cancelled: 'OCR cancelled',
    interrupted: 'OCR interrupted',
};

const timestamp = ms => {
    const value = Math.max(0, Math.round(Number(ms) || 0));
    return `${[Math.floor(value / 3600000), Math.floor(value / 60000) % 60, Math.floor(value / 1000) % 60]
        .map(part => String(part).padStart(2, '0'))
        .join(':')}.${String(value % 1000).padStart(3, '0')}`;
};

export const ocrCopyText = segments =>
    (Array.isArray(segments) ? segments : [])
        .map(segment => `[${(segment.observed_at_ms || []).map(timestamp).join(', ')}] ${segment.text || ''}`)
        .join('\n\n');

export const ocrEntries = (segments, fallbackText = '') => {
    const entries = (Array.isArray(segments) ? segments : [])
        .flatMap((segment, segmentIndex) => {
            const times = (segment.observed_at_ms || []).filter(time => Number.isFinite(time) && time >= 0);
            return (times.length ? times : [null]).map(time => ({ time, text: segment.text || '', segmentIndex }));
        })
        .filter(entry => entry.text);
    if (!entries.length && fallbackText) entries.push({ time: null, text: fallbackText });
    return entries.sort((a, b) => (a.time ?? Infinity) - (b.time ?? Infinity));
};

export const createHistoryOcrView = ({ els, api, appendLog, jobs }) => {
    const state = {
        id: null,
        request: 0,
        content: undefined,
        loading: false,
        startError: '',
        cancelling: false,
        disposed: false,
        layoutAttempt: null,
        timeCharacters: 8,
        frameCharacters: 1,
        rows: [],
        rowsContent: null,
        copyContent: null,
        copiedText: '',
        shown: 100,
        renderedRows: null,
        renderedQuery: null,
        renderedShown: 0,
        renderedControls: '',
        selected: null,
        frameRequest: 0,
        frameLoading: false,
        frameImage: '',
        frameError: '',
        deleting: false,
        deleteRequest: 0,
        actionError: '',
    };
    const visible = () => !els.historyDetailsView.hidden && els.historyOcrTab.getAttribute('aria-selected') === 'true';
    const hasResult = () => state.content?.result_created_at !== null && state.content?.result_created_at !== undefined;
    const copyText = () => {
        if (!hasResult()) return '';
        if (state.copyContent !== state.content) {
            state.copyContent = state.content;
            state.copiedText = ocrCopyText(state.content.segments) || state.content.text || '';
        }
        return state.copiedText;
    };
    const rowKey = row => `${row.segmentIndex}:${row.time}`;
    const selection = row => ({
        result_revision: state.content.result_revision,
        segment_index: row.segmentIndex,
        observed_at_ms: row.time,
    });
    const clearFrame = () => {
        state.frameRequest += 1;
        state.selected = null;
        state.frameLoading = false;
        state.frameImage = '';
        state.frameError = '';
    };
    const renderPreview = () => {
        els.historyOcrPreview.setAttribute('aria-busy', String(state.frameLoading));
        els.historyOcrFrame.hidden = !state.frameImage;
        if (state.frameImage) {
            if (els.historyOcrFrame.getAttribute('src') !== state.frameImage)
                els.historyOcrFrame.src = state.frameImage;
            els.historyOcrFrame.alt = `Saved video frame at ${formatMediaTime(state.selected.time)}`;
        } else els.historyOcrFrame.removeAttribute('src');
        els.historyOcrFrameStatus.hidden = Boolean(state.frameImage);
        els.historyOcrFrameStatus.textContent = state.frameLoading
            ? 'Loading selected frame…'
            : state.frameError || 'Select detected text below to view its video frame.';
        els.historyOcrFrameTime.textContent = state.selected ? formatMediaTime(state.selected.time) : '';
        els.historyOcrFrameTime.title = state.selected ? timestamp(state.selected.time) : '';
        els.historyOcrSelectedText.textContent = state.selected?.text || '';
        els.historyOcrActionNotice.hidden = !state.actionError;
        els.historyOcrActionNotice.textContent = state.actionError;
    };
    const selectFrame = async row => {
        if (row.time === null || !Number.isInteger(row.segmentIndex) || !state.content?.result_revision) return;
        const id = state.id;
        const selected = selection(row);
        const request = ++state.frameRequest;
        state.selected = { ...row, revision: selected.result_revision };
        state.frameLoading = true;
        state.frameImage = '';
        state.frameError = '';
        renderResults();
        let timer;
        try {
            const image = await Promise.race([
                api.getHistoryOcrFrame({ id, selection: selected }),
                new Promise((_, reject) => {
                    timer = setTimeout(() => reject(new Error('Frame preview timed out.')), 30000);
                }),
            ]);
            if (id !== state.id || request !== state.frameRequest || !visible() || state.disposed) return;
            state.frameImage = image;
        } catch (error) {
            appendLog(`[OCR frame] ${error}`, true);
            if (id !== state.id || request !== state.frameRequest || !visible() || state.disposed) return;
            state.frameError = /timed out/i.test(String(error))
                ? 'The frame took too long to load. Select the text to try again.'
                : /changed/i.test(String(error))
                  ? 'The saved video or OCR results changed. Reopen OCR or run the analysis again.'
                  : 'The selected frame is unavailable. Check that the saved video and FFmpeg are available.';
        } finally {
            clearTimeout(timer);
        }
        state.frameLoading = false;
        renderPreview();
    };
    const deleteRow = async row => {
        if (state.deleting || state.loading || ocrActive(jobs.get(state.id)?.status)) return;
        const id = state.id;
        const selected = selection(row);
        const request = ++state.deleteRequest;
        state.request += 1;
        state.deleting = true;
        state.actionError = '';
        render();
        try {
            const content = await api.deleteHistoryOcrObservation({ id, selection: selected });
            if (id !== state.id || request !== state.deleteRequest || state.disposed) return;
            state.content = content;
            clearFrame();
        } catch (error) {
            appendLog(`[OCR delete] ${error}`, true);
            if (id !== state.id || request !== state.deleteRequest || state.disposed) return;
            state.actionError = /changed|no longer/i.test(String(error))
                ? 'OCR results changed. Reopen OCR and try again.'
                : 'The detected text could not be deleted. Try again after analysis finishes.';
        }
        state.deleting = false;
        render();
        if (visible()) els.historyOcrText.querySelector('.pinefetch-ocr-select')?.focus({ preventScroll: true });
        const current = jobs.get(id);
        if (current?.attempt_id && current.attempt_id !== state.content?.last_attempt_id) void load();
    };
    const renderResults = () => {
        els.historyOcrResults.hidden = !hasResult();
        els.historyOcrCopyBtn.disabled = !copyText();
        if (!hasResult()) {
            renderPreview();
            return;
        }
        if (state.rowsContent !== state.content) {
            state.rowsContent = state.content;
            state.rows = ocrEntries(state.content.segments, state.content.text);
        }
        const query = els.historyOcrSearchInput.value.trim().toLocaleLowerCase();
        if (state.selected && state.selected.revision !== state.content.result_revision) clearFrame();
        const controls = `${state.loading}:${state.deleting}:${ocrActive(jobs.get(state.id)?.status)}:${state.selected ? rowKey(state.selected) : ''}`;
        renderPreview();
        if (
            state.renderedRows === state.rows &&
            state.renderedQuery === query &&
            state.renderedShown === state.shown &&
            state.renderedControls === controls
        )
            return;
        state.renderedRows = state.rows;
        state.renderedQuery = query;
        state.renderedShown = state.shown;
        state.renderedControls = controls;
        const rows = query ? state.rows.filter(entry => entry.text.toLocaleLowerCase().includes(query)) : state.rows;
        const shown = rows.slice(0, state.shown);
        const focused = els.historyOcrText.contains(document.activeElement)
            ? document.activeElement.closest('li')?.dataset.ocrKey
            : null;
        const fragment = document.createDocumentFragment();
        for (const row of shown) {
            const item = document.createElement('li');
            item.className = 'pinefetch-ocr-result';
            item.dataset.ocrKey = rowKey(row);
            const time = document.createElement('button');
            time.type = 'button';
            time.className = 'oj-button oj-button-ghost oj-mono pinefetch-ocr-select';
            time.textContent = row.time === null ? '—' : formatMediaTime(row.time);
            time.disabled = row.time === null || !Number.isInteger(row.segmentIndex) || !state.content.result_revision;
            time.setAttribute('aria-label', `Show video frame at ${time.textContent}`);
            if (row.time !== null) time.title = timestamp(row.time);
            const selected = state.selected && rowKey(state.selected) === rowKey(row);
            item.setAttribute('aria-current', String(Boolean(selected)));
            const text = document.createElement('p');
            text.className = 'oj-text';
            text.textContent = row.text;
            const remove = document.createElement('button');
            remove.type = 'button';
            remove.className = 'oj-button oj-button-danger pinefetch-ocr-delete';
            remove.textContent = 'Delete';
            remove.setAttribute('aria-label', `Delete detected text at ${time.textContent}: ${row.text}`);
            remove.disabled =
                time.disabled ||
                state.loading ||
                state.deleting ||
                ocrActive(jobs.get(state.id)?.status) ||
                state.content.last_attempt_status === 'running';
            remove.addEventListener('click', () => void deleteRow(row));
            item.addEventListener('click', event => {
                if (!time.disabled && !remove.contains(event.target)) void selectFrame(row);
            });
            item.append(time, text, remove);
            fragment.append(item);
        }
        els.historyOcrText.replaceChildren(fragment);
        if (focused) {
            [...els.historyOcrText.children]
                .find(item => item.dataset.ocrKey === focused)
                ?.querySelector('.pinefetch-ocr-select')
                ?.focus({ preventScroll: true });
        }
        els.historyOcrResultCount.textContent = `${rows.length.toLocaleString()}${query ? ' found' : ' detected entries'}${shown.length < rows.length ? ` · ${shown.length.toLocaleString()} shown` : ''}`;
        els.historyOcrEmpty.hidden = rows.length > 0;
        els.historyOcrEmpty.textContent = query
            ? 'No detected text matches your search.'
            : 'No text was detected in the sampled video frames.';
        els.historyOcrLoadMoreBtn.hidden = shown.length >= rows.length;
    };
    const render = () => {
        const content = state.content;
        const progress = jobs.get(state.id);
        const status = progress?.status || ocrStatus(content?.last_attempt_status);
        const active = ocrActive(status);
        const running = status === 'running';
        const duration = progress?.total_duration_ms;
        const hasDuration = Number.isFinite(duration) && duration > 0;
        const observedTime = Number.isFinite(progress?.observed_at_ms) ? Math.max(0, progress.observed_at_ms) : 0;
        const totalFrames = progress?.total_frames;
        const hasTotalFrames = Number.isSafeInteger(totalFrames) && totalFrames > 0;
        const finishing = running && hasTotalFrames && progress.total_frames_is_estimate === false;
        const statusLabel = finishing
            ? 'Finishing OCR…'
            : state.loading && !progress
              ? 'Loading OCR…'
              : statusLabels[status] || 'Not checked yet';
        if (els.historyOcrStatus.textContent !== statusLabel) els.historyOcrStatus.textContent = statusLabel;
        const error = state.startError || progress?.message || (!active ? content?.last_error : '');
        els.historyOcrNotice.textContent = error ? ocrErrorMessage(error) : '';
        els.historyOcrNotice.hidden = !error || (status === 'cancelled' && !state.startError);
        els.historyOcrStartBtn.hidden = active;
        els.historyOcrStartBtn.disabled = state.loading || state.deleting || !api.available;
        els.historyOcrStartBtn.textContent =
            status === 'failed' || status === 'interrupted'
                ? 'Retry Analysis'
                : status === 'cancelled'
                  ? 'Start Again'
                  : hasResult()
                    ? 'Run OCR Again'
                    : 'Start OCR Analysis';
        els.historyOcrProgress.hidden = !active;
        els.historyOcrAnalysis.hidden = !active;
        els.historyOcrProgress.max = 1;
        if (running && hasDuration) {
            // Follow actual media PTS, retaining final source checks below 100%.
            els.historyOcrProgress.value = finishing ? 0.999 : Math.min(0.999, observedTime / duration);
        } else {
            els.historyOcrProgress.removeAttribute('value');
        }
        const showCounters = running && (hasDuration || progress?.frames_checked > 0);
        els.historyOcrPreparing.textContent =
            status === 'queued' ? 'Waiting for OCR to start…' : 'Preparing the saved video…';
        els.historyOcrPreparing.hidden = !active || showCounters;
        els.historyOcrProgressText.hidden = !showCounters;
        els.historyOcrProgress.setAttribute(
            'aria-describedby',
            showCounters ? 'historyOcrProgressText' : 'historyOcrPreparing'
        );
        if (active && state.layoutAttempt !== progress?.attempt_id) {
            state.layoutAttempt = progress?.attempt_id;
            state.timeCharacters = 8;
            state.frameCharacters = 1;
        }
        if (showCounters) {
            const time = formatMediaTime(observedTime);
            const timeTotal = hasDuration ? formatMediaTime(duration) : 'Unknown';
            const frames = String(progress?.frames_checked || 0);
            const framesTotal = hasTotalFrames ? String(totalFrames) : 'Unknown';
            // Keep reserved widths if extraction's final total has fewer digits.
            state.timeCharacters = Math.max(state.timeCharacters, time.length, timeTotal.length);
            state.frameCharacters = Math.max(state.frameCharacters, frames.length, framesTotal.length);
            els.historyOcrProgressText.style.setProperty('--pinefetch-ocr-time-width', `${state.timeCharacters}ch`);
            els.historyOcrProgressText.style.setProperty('--pinefetch-ocr-frame-width', `${state.frameCharacters}ch`);
            els.historyOcrTime.textContent = time;
            els.historyOcrTimeTotal.textContent = timeTotal;
            els.historyOcrFrames.textContent = frames;
            els.historyOcrFramesTotal.textContent = framesTotal;
        }
        els.historyOcrCancelBtn.hidden = !active;
        els.historyOcrCancelBtn.disabled = !progress?.attempt_id || state.cancelling;
        renderResults();
        els.historyOcrEntryCount.textContent = hasResult() ? state.rows.length.toLocaleString() : '—';
        els.historyOcrAnalyzed.textContent = hasResult()
            ? formatLocalDateTime(new Date(content.result_created_at))
            : '—';
        els.historyOcrPreviousResult.hidden = !hasResult() || status === 'completed';
    };
    const deactivate = () => {
        state.request += 1;
        if (state.frameLoading) clearFrame();
    };
    const reset = id => {
        state.id = id;
        state.request += 1;
        state.content = undefined;
        state.loading = false;
        state.startError = '';
        state.cancelling = false;
        state.deleting = false;
        state.deleteRequest += 1;
        state.actionError = '';
        clearFrame();
        state.layoutAttempt = null;
        state.timeCharacters = 8;
        state.frameCharacters = 1;
        state.rows = [];
        state.rowsContent = null;
        state.copyContent = null;
        state.copiedText = '';
        state.renderedRows = null;
        state.shown = 100;
        els.historyOcrText.replaceChildren();
        els.historyOcrSearchInput.value = '';
        render();
    };
    const load = async () => {
        if (!state.id || !visible() || state.disposed || state.deleting) return;
        const id = state.id;
        const request = ++state.request;
        const revision = jobs.revision(id);
        state.loading = true;
        render();
        try {
            const [content, progress] = await Promise.all([
                api.getHistoryOcr({ id }),
                api.getHistoryOcrProgress({ id }),
            ]);
            if (id !== state.id || request !== state.request || !visible() || state.disposed) return;
            state.content = content || null;
            state.loading = false;
            const applied = jobs.snapshot(id, content, progress, revision);
            render();
            if (!applied && !ocrActive(jobs.get(id)?.status)) void load();
        } catch (error) {
            if (id !== state.id || request !== state.request || !visible() || state.disposed) return;
            state.loading = false;
            appendLog(`[OCR] ${error}`, true);
            state.startError = String(error);
            render();
        }
    };
    const bindEvents = () => {
        els.historyOcrStartBtn.addEventListener('click', async () => {
            const id = state.id;
            if (!id || state.loading || state.deleting) return;
            state.startError = '';
            try {
                await jobs.start(id);
            } catch (error) {
                appendLog(`[OCR] ${error}`, true);
                if (state.id === id) state.startError = String(error);
            }
            if (state.id === id) {
                render();
                void load();
            }
        });
        els.historyOcrCancelBtn.addEventListener('click', async () => {
            const id = state.id;
            const attemptId = jobs.get(id)?.attempt_id;
            if (!id || !attemptId) return;
            state.cancelling = true;
            render();
            try {
                await api.cancelHistoryOcr({ id, attemptId });
            } catch (error) {
                appendLog(`[OCR cancel] ${error}`, true);
                if (state.id === id) state.startError = String(error);
            }
            if (state.id === id) {
                state.cancelling = false;
                render();
                void load();
            }
        });
        els.historyOcrCopyBtn.addEventListener('click', async () => {
            const text = copyText();
            if (!text) return;
            try {
                await navigator.clipboard.writeText(text);
                toast('OCR text copied.', { type: 'success' });
            } catch (error) {
                appendLog(`[OCR copy] ${error}`, true);
            }
        });
        els.historyOcrSearchInput.addEventListener('input', () => {
            state.shown = 100;
            renderResults();
        });
        els.historyOcrLoadMoreBtn.addEventListener('click', () => {
            state.shown += 100;
            renderResults();
        });
        els.historyOcrFrame.addEventListener('error', () => {
            state.frameImage = '';
            state.frameError = 'The selected frame could not be displayed. Select the text to try again.';
            renderPreview();
        });
        const unsubscribe = jobs.subscribe((id, progress) => {
            if (id !== state.id || state.disposed) return;
            render();
            if (
                progress &&
                !ocrActive(progress.status) &&
                visible() &&
                (state.content?.last_attempt_id !== progress.attempt_id ||
                    ocrStatus(state.content?.last_attempt_status) !== progress.status)
            )
                void load();
        });
        window.addEventListener(
            'pagehide',
            () => {
                state.disposed = true;
                state.request += 1;
                unsubscribe();
            },
            { once: true }
        );
    };
    return Object.freeze({ reset, load, deactivate, bindEvents });
};
