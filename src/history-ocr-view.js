import { formatLocalDateTime } from './formatters.js';
import { toast } from './vendor/oj/index.js';

const statusLabels = {
    running: 'Checking for OCR…',
    completed: 'OCR complete',
    error: 'OCR failed',
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

export const createHistoryOcrView = ({ els, api, appendLog }) => {
    const state = { id: null, request: 0, revision: 0, content: undefined, progress: null, disposed: false };
    let unsubscribe;
    const visible = () => els.historyDetailsDialog.open && els.historyOcrTab.getAttribute('aria-selected') === 'true';
    const render = () => {
        const content = state.content;
        const progress = state.progress;
        const status = progress?.status || content?.last_attempt_status;
        const running = status === 'running';
        const hasResult = content?.result_created_at !== null && content?.result_created_at !== undefined;
        const text = hasResult ? ocrCopyText(content.segments) : '';
        els.historyOcrStatus.textContent = statusLabels[status] || 'Not checked yet';
        if (status === 'completed' && hasResult && !text) els.historyOcrStatus.textContent = 'No text detected';
        els.historyOcrNotice.textContent = progress?.message || content?.last_error || '';
        els.historyOcrNotice.hidden = !els.historyOcrNotice.textContent;
        els.historyOcrProgress.hidden = !running;
        // Frame count and actual media PTS are observed progress; no guessed percentages.
        els.historyOcrProgressText.textContent =
            running && progress?.frames_checked
                ? `${progress.frames_checked} frames checked · Local video time ${timestamp(progress.observed_at_ms)}`
                : running
                  ? 'Preparing the saved video…'
                  : '';
        els.historyOcrCancelBtn.hidden = !running;
        els.historyOcrCancelBtn.disabled = !running;
        els.historyOcrText.textContent = text;
        els.historyOcrText.hidden = !text;
        els.historyOcrCopyBtn.disabled = !text;
        els.historyOcrMeta.textContent = hasResult
            ? `Saved result: ${formatLocalDateTime(new Date(content.result_created_at))}${status !== 'completed' ? ' · Previous result remains available' : ''}`
            : '';
        els.historyOcrSamplingNotice.hidden = !hasResult;
    };
    const reset = id => {
        state.id = id;
        state.request += 1;
        state.revision += 1;
        state.content = undefined;
        state.progress = null;
        render();
    };
    const load = async () => {
        if (!state.id || !visible() || state.disposed) return;
        const id = state.id;
        const request = ++state.request;
        const revision = state.revision;
        try {
            const [content, progress] = await Promise.all([
                api.getHistoryOcr({ id }),
                api.getHistoryOcrProgress({ id }),
            ]);
            if (id !== state.id || request !== state.request || !visible() || state.disposed) return;
            if (revision !== state.revision) {
                void load();
                return;
            }
            state.content = content || null;
            const terminal =
                state.progress &&
                state.progress.status !== 'running' &&
                state.progress.attempt_id === content?.last_attempt_id;
            state.progress = progress || (terminal ? state.progress : null);
            render();
        } catch (error) {
            if (id !== state.id || request !== state.request || !visible() || state.disposed) return;
            appendLog(`[OCR] ${error}`, true);
            els.historyOcrStatus.textContent = 'OCR could not be loaded';
            els.historyOcrNotice.textContent = `${error}`;
            els.historyOcrNotice.hidden = false;
        }
    };
    const bindEvents = () => {
        els.historyOcrCancelBtn.addEventListener('click', async () => {
            const id = state.id;
            const attemptId = state.progress?.attempt_id || state.content?.last_attempt_id;
            if (!id || !attemptId) return;
            els.historyOcrCancelBtn.disabled = true;
            try {
                await api.cancelHistoryOcr({ id, attemptId });
            } catch (error) {
                appendLog(`[OCR] ${error}`, true);
                if (id === state.id && visible()) toast(`${error}`, { type: 'error', root: els.historyDetailsDialog });
            }
            if (id === state.id) void load();
        });
        els.historyOcrCopyBtn.addEventListener('click', async () => {
            const text = ocrCopyText(state.content?.segments);
            if (!text) return;
            try {
                await navigator.clipboard.writeText(text);
                toast('OCR text copied.', { type: 'success', root: els.historyDetailsDialog });
            } catch (error) {
                appendLog(`[OCR copy] ${error}`, true);
            }
        });
        if (api.eventsAvailable) {
            void api
                .onHistoryOcr(event => {
                    const progress = event.payload;
                    if (
                        !progress ||
                        progress.history_id !== state.id ||
                        !progress.attempt_id ||
                        !statusLabels[progress.status]
                    )
                        return;
                    if (!visible() || state.disposed) return;
                    const currentId = state.progress?.attempt_id || state.content?.last_attempt_id;
                    const currentTime = state.progress?.attempt_at || state.content?.last_attempt_at || 0;
                    if (currentId && currentId !== progress.attempt_id && progress.attempt_at <= currentTime) return;
                    const currentStatus = state.progress?.status || state.content?.last_attempt_status;
                    if (
                        currentId === progress.attempt_id &&
                        currentStatus &&
                        currentStatus !== 'running' &&
                        progress.status === 'running'
                    )
                        return;
                    state.revision += 1;
                    state.progress = progress;
                    render();
                    if (progress.status !== 'running') void load();
                })
                .then(cleanup => {
                    if (state.disposed) cleanup();
                    else unsubscribe = cleanup;
                })
                .catch(error => appendLog(`[OCR events] ${error}`, true));
        }
        window.addEventListener(
            'pagehide',
            () => {
                state.disposed = true;
                state.request += 1;
                unsubscribe?.();
            },
            { once: true }
        );
        els.historyDetailsDialog.addEventListener('oj:close', () => {
            state.request += 1;
        });
    };
    return Object.freeze({ reset, load, bindEvents });
};
