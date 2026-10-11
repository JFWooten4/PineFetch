import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createHistoryOcrJobs, ocrActive, ocrStatus, ocrErrorMessage } from '../src/history-ocr-jobs.js';
import { ocrEntries, ocrCopyText } from '../src/history-ocr-view.js';

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

test('consecutive OCR observations share one chronological entry and copying keeps precise timestamps', () => {
    const segments = [
        { text: 'Repeat', observed_at_ms: [3020, 4030, 5040] },
        { text: '<img src=x> stays text', observed_at_ms: [6000] },
        { text: 'Repeat', observed_at_ms: [9000, 10010] },
    ];
    assert.deepEqual(
        ocrEntries(segments).map(entry => entry.time),
        [3020, 6000, 9000]
    );
    assert.match(ocrCopyText(segments), /00:00:03.020, 00:00:04.030, 00:00:05.040/);
    assert.match(ocrCopyText(segments), /00:00:09.000, 00:00:10.010/);
    assert.deepEqual(
        ocrEntries(segments).map(entry => entry.segmentIndex),
        [0, 1, 2]
    );
    assert.deepEqual(ocrEntries(null, 'Legacy saved text'), [{ time: null, text: 'Legacy saved text' }]);
    assert.deepEqual(ocrEntries([], ''), []);
});

test('grouped OCR entries retain source indexes after sorting and preserve separate positions', () => {
    const segments = [
        { text: 'Later', observed_at_ms: [9000, 10000] },
        { text: 'Same text', observed_at_ms: [4000, 3000, -1, NaN] },
        { text: 'Same text', observed_at_ms: [3000, 4000] },
        { text: '', observed_at_ms: [0] },
        { text: 'Legacy segment', observed_at_ms: [] },
    ];
    assert.deepEqual(ocrEntries(segments), [
        { time: 3000, text: 'Same text', segmentIndex: 1 },
        { time: 3000, text: 'Same text', segmentIndex: 2 },
        { time: 9000, text: 'Later', segmentIndex: 0 },
        { time: null, text: 'Legacy segment', segmentIndex: 4 },
    ]);
    assert.deepEqual(segments[1].observed_at_ms.slice(0, 2), [4000, 3000]);
});

test('long consecutive OCR runs produce one entry without expanding every frame', () => {
    const observed_at_ms = Array.from({ length: 150000 }, (_, index) => index * 1000);
    assert.deepEqual(ocrEntries([{ text: 'Static caption', observed_at_ms }]), [
        { time: 0, text: 'Static caption', segmentIndex: 0 },
    ]);
});
