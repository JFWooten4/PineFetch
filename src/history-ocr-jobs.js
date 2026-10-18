// App-scoped job state. Navigation and tab lifetimes never own native jobs.
export const ocrStatus = status => (status === 'error' ? 'failed' : status || 'not_started');
export const ocrActive = status => status === 'queued' || status === 'running';
const jobStatuses = new Set(['queued', 'running', 'completed', 'failed', 'cancelled', 'interrupted']);

export const ocrErrorMessage = error => {
    const message = String(error || '');
    if (/macOS only/i.test(message)) return 'Apple Vision OCR is available on macOS only.';
    if (/already running|another PineFetch instance/i.test(message))
        return 'Another OCR analysis is active. Wait for it to finish or cancel it before starting this video.';
    if (/Several downloaded videos match/i.test(message))
        return "Several saved videos match this transcript. Open the video's own History entry or download it again.";
    if (/No downloaded video|supported video extension/i.test(message))
        return 'OCR requires a downloaded video. Enable “Keep video with transcript” and download it again, or download the video separately.';
    if (/supported video container|no usable video track/i.test(message))
        return 'This file does not contain a supported video. Text, audio and image files cannot be analyzed with OCR.';
    if (/FFmpeg|ffprobe|helper.*(missing|unavailable)/i.test(message))
        return 'The local OCR tools are unavailable. Check your installation and try again.';
    if (/missing|unreadable|saved video path|no longer exists/i.test(message))
        return 'The saved video is unavailable. Check its local file path before trying again.';
    if (/changed|path changed/i.test(message)) return 'The saved video changed during analysis. Please run OCR again.';
    if (/audio|video stream/i.test(message)) return 'OCR requires a saved file containing a video stream.';
    return 'The analysis could not be completed. Please try again. Technical details are available in Settings logs.';
};

export const createHistoryOcrJobs = ({ api, appendLog }) => {
    const records = new Map();
    const revisions = new Map();
    const pending = new Map();
    const listeners = new Set();
    let disposed = false;
    let cleanup;
    const get = id => records.get(id) || null;
    const revision = id => revisions.get(id) || 0;
    const publish = (id, record) => {
        if (record) records.set(id, record);
        else records.delete(id);
        revisions.set(id, revision(id) + 1);
        for (const listener of listeners) listener(id, record);
    };
    const adopt = value => {
        if (!value?.history_id || !value.attempt_id) return;
        const record = { ...value, status: ocrStatus(value.status) };
        if (!jobStatuses.has(record.status)) return;
        const current = get(record.history_id);
        if (current?.attempt_id) {
            if (current.attempt_id !== record.attempt_id && record.attempt_at <= current.attempt_at) return;
            if (current.attempt_id === record.attempt_id && !ocrActive(current.status) && ocrActive(record.status))
                return;
        }
        if (
            (record.status === 'failed' || record.status === 'interrupted') &&
            record.message &&
            (current?.attempt_id !== record.attempt_id || current?.message !== record.message)
        )
            appendLog(`[OCR ${record.history_id}] ${record.message}`, true);
        publish(record.history_id, {
            ...(current?.attempt_id === record.attempt_id ? current : {}),
            ...record,
        });
    };
    const snapshot = (id, content, progress, expectedRevision) => {
        if (revision(id) !== expectedRevision) return false;
        if (progress) adopt(progress);
        else if (content)
            adopt({
                history_id: id,
                attempt_id: content.last_attempt_id,
                attempt_at: content.last_attempt_at,
                status: content.last_attempt_status,
                result_created_at: content.result_created_at,
                message: content.last_error,
            });
        else if (!pending.has(id) && get(id)) publish(id, null);
        return true;
    };
    const start = id => {
        if (pending.has(id)) return pending.get(id);
        if (ocrActive(get(id)?.status)) return Promise.resolve(get(id));
        if (pending.size || [...records.values()].some(record => ocrActive(record.status)))
            return Promise.reject(new Error('An OCR analysis is already running.'));
        const previous = get(id);
        // queued means the start request is awaiting the native acknowledgement;
        // the existing backend continues to allow one analysis at a time.
        const request = Promise.resolve().then(async () => {
            publish(id, { history_id: id, status: 'queued', attempt_id: null, attempt_at: 0 });
            try {
                const progress = await api.startHistoryOcr({ id });
                adopt(progress);
                return get(id);
            } catch (error) {
                if (!get(id)?.attempt_id) publish(id, previous);
                throw error;
            } finally {
                pending.delete(id);
            }
        });
        pending.set(id, request);
        return request;
    };
    const bindEvents = () => {
        if (!api.eventsAvailable) return;
        void api
            .onHistoryOcr(event => {
                if (disposed) return;
                adopt(event.payload);
            })
            .then(unsubscribe => {
                if (disposed) unsubscribe();
                else cleanup = unsubscribe;
            })
            .catch(error => appendLog(`[OCR events] ${error}`, true));
    };
    const dispose = () => {
        disposed = true;
        cleanup?.();
        listeners.clear();
    };
    return Object.freeze({
        get,
        revision,
        snapshot,
        start,
        bindEvents,
        dispose,
        adoptSummaries: summaries => (summaries || []).forEach(adopt),
        subscribe: listener => {
            listeners.add(listener);
            return () => listeners.delete(listener);
        },
    });
};
