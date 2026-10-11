import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createHistoryOcrJobs, ocrActive, ocrStatus, ocrErrorMessage } from '../src/history-ocr-jobs.js';
import { ocrEntries } from '../src/history-ocr-view.js';

const deferred = () => {
    let resolve;
    let reject;
    const promise = new Promise((yes, no) => {
        resolve = yes;
        reject = no;
    });
    return { promise, resolve, reject };
};
const progress = (status = 'running', overrides = {}) => ({
    history_id: 'video-a',
    attempt_id: 'attempt-a',
    attempt_at: 100,
    status,
    frames_checked: 12,
    observed_at_ms: 11000,
    ...overrides,
});
const fixture = () => {
    const reply = deferred();
    let handler;
    let starts = 0;
    let unsubscribed = false;
    const jobs = createHistoryOcrJobs({
        api: {
            eventsAvailable: true,
            startHistoryOcr: () => {
                starts += 1;
                return reply.promise;
            },
            onHistoryOcr: listener => {
                handler = listener;
                return Promise.resolve(() => {
                    unsubscribed = true;
                });
            },
        },
        appendLog: () => {},
    });
    jobs.bindEvents();
    return {
        jobs,
        reply,
        emit: value => handler({ payload: value }),
        starts: () => starts,
        unsubscribed: () => unsubscribed,
    };
};

test('OCR states and user errors preserve the existing backend contract without exposing traces', () => {
    assert.equal(ocrStatus(null), 'not_started');
    assert.equal(ocrStatus('error'), 'failed');
    for (const status of ['queued', 'running']) assert(ocrActive(status));
    for (const status of ['completed', 'failed', 'cancelled', 'interrupted']) assert.equal(ocrActive(status), false);
    assert.match(ocrErrorMessage('The saved video changed during OCR.'), /changed/);
    assert.match(ocrErrorMessage('The saved file is missing or unreadable.'), /unavailable/);
    assert.match(ocrErrorMessage('An OCR analysis is already running.'), /Another OCR analysis/);
    const internal = ocrErrorMessage('Traceback: private path /Users/example RuntimeError');
    assert.match(internal, /try again/);
    assert.doesNotMatch(internal, /Traceback|private|RuntimeError/);
});

test('queued start requests deduplicate and native acknowledgement adopts the original attempt', async () => {
    const { jobs, reply, starts } = fixture();
    const first = jobs.start('video-a');
    assert.equal(jobs.start('video-a'), first);
    await Promise.resolve();
    assert.equal(jobs.get('video-a').status, 'queued');
    assert.equal(starts(), 1);
    await assert.rejects(jobs.start('video-b'), /already running/);
    reply.resolve(progress());
    await first;
    assert.equal((await jobs.start('video-a')).attempt_id, 'attempt-a');
    assert.equal(starts(), 1);
});

test('events remain active after detail subscribers leave and stale replies cannot regress completion', async () => {
    const { jobs, reply, emit, unsubscribed } = fixture();
    const displayed = [];
    const leaveDetails = jobs.subscribe((id, record) => displayed.push([id, record?.status]));
    const started = jobs.start('video-a');
    await Promise.resolve();
    leaveDetails();
    emit(progress());
    const oldRevision = jobs.revision('video-a');
    emit(progress('completed'));
    reply.resolve(progress());
    await started;
    assert.equal(jobs.get('video-a').status, 'completed');
    assert.equal(jobs.snapshot('video-a', null, progress(), oldRevision), false);
    jobs.adoptSummaries([progress(), progress('running', { attempt_id: 'old', attempt_at: 50 })]);
    assert.equal(jobs.get('video-a').status, 'completed');
    assert.deepEqual(displayed, [['video-a', 'queued']]);
    jobs.dispose();
    assert(unsubscribed());
});

test('failed starts retain saved results; retries accept fresh attempts and cancellation is terminal', async () => {
    const { jobs, reply, emit } = fixture();
    jobs.adoptSummaries([progress('completed', { result_created_at: 99 })]);
    const started = jobs.start('video-a');
    await Promise.resolve();
    reply.reject(new Error('synthetic launch failure'));
    await assert.rejects(started, /launch failure/);
    assert.equal(jobs.get('video-a').result_created_at, 99);
    emit(progress('running', { attempt_id: 'rerun', attempt_at: 200 }));
    emit(progress('cancelled', { attempt_id: 'rerun', attempt_at: 200 }));
    emit(progress('running', { attempt_id: 'rerun', attempt_at: 200 }));
    assert.equal(jobs.get('video-a').status, 'cancelled');
    emit(progress('error', { attempt_id: 'retry', attempt_at: 300 }));
    assert.equal(jobs.get('video-a').status, 'failed');
});

test('batched stored summaries hydrate a fresh session without fetching individual OCR results', () => {
    const { jobs, starts } = fixture();
    jobs.adoptSummaries([
        progress('completed', { result_created_at: 90 }),
        progress('interrupted', { history_id: 'video-b' }),
    ]);
    assert.equal(jobs.get('video-a').status, 'completed');
    assert.equal(jobs.get('video-b').status, 'interrupted');
    assert.equal(jobs.get('unanalysed'), null);
    assert.equal(starts(), 0);
});

test('frame entries retain native identity, precise times, line breaks and repeated texts', () => {
    const frames = [
        { id: 'sample-1', timestamp_ms: 8020, full_text: 'Same text\nAnother block\nSame text' },
        { id: 'sample-2', timestamp_ms: 8030, full_text: 'Same text\nAnother block\nSame text' },
    ];
    assert.deepEqual(ocrEntries(frames), [
        { id: 'sample-1', time: 8020, text: 'Same text\nAnother block\nSame text' },
        { id: 'sample-2', time: 8030, text: 'Same text\nAnother block\nSame text' },
    ]);
});

test('unstructured legacy text remains readable without an invented frame selection', () => {
    assert.deepEqual(ocrEntries([], 'Legacy saved text'), [{ id: null, time: null, text: 'Legacy saved text' }]);
    assert.deepEqual(ocrEntries(null, ''), []);
});
