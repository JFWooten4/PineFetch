import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createYoutubeCookieSync } from '../src/youtube-cookie-sync.js';
const settle = () => new Promise(resolve => setImmediate(resolve));
const setup = overrides => {
    const element = () => ({
        value: 'brave',
        disabled: true,
        textContent: '',
        classList: { toggle() {} },
        addEventListener(event, handler) {
            this[event] = handler;
        },
    });
    const els = Object.fromEntries(
        ['syncYoutubeCookiesBtn', 'disconnectYoutubeCookiesBtn', 'youtubeCookieStatus', 'youtubeCookieBrowser'].map(
            key => [key, element()]
        )
    );
    let nextPoll;
    const api = { available: true, getYoutubeCookieSync: async () => ({ state: 'off' }), ...overrides };
    const view = createYoutubeCookieSync({
        els,
        api,
        schedule: callback => {
            nextPoll = callback;
        },
        cancel() {},
    });
    view.bindEvents();
    return { els, view, poll: () => nextPoll?.() };
};
test('opening only reads status; Enable waits for Link Dump and a later sync updates the page', async () => {
    let requests = 0;
    let current = { state: 'off' };
    const { els, view, poll } = setup({
        getYoutubeCookieSync: async () => current,
        beginYoutubeCookieSync: async args => {
            assert.deepEqual(args, { browser: 'brave' });
            requests++;
            return { state: 'waiting' };
        },
        disconnectYoutubeCookieSync: async () => ({ state: 'off' }),
    });
    await view.load();
    assert.equal(requests, 0);
    els.syncYoutubeCookiesBtn.click();
    await settle();
    assert.equal(requests, 1);
    assert.match(els.youtubeCookieStatus.textContent, /Waiting for Link Dump/);
    current = { state: 'ready', cookie_count: 1 };
    poll();
    await settle();
    assert.match(els.youtubeCookieStatus.textContent, /synced from Link Dump/);
    els.disconnectYoutubeCookiesBtn.click();
    await settle();
    assert.equal(els.disconnectYoutubeCookiesBtn.disabled, true);
});
test('failed changes preserve existing connection and do not expose backend details', async () => {
    const { els, view } = setup({
        getYoutubeCookieSync: async () => ({ state: 'ready' }),
        disconnectYoutubeCookieSync: async () => {
            throw new Error('private detail');
        },
    });
    await view.load();
    els.disconnectYoutubeCookiesBtn.click();
    await settle();
    assert.equal(els.disconnectYoutubeCookiesBtn.disabled, false);
    assert.match(els.youtubeCookieStatus.textContent, /could not be updated/);
    assert.doesNotMatch(els.youtubeCookieStatus.textContent, /private/);
});
test('a stale poll cannot reconnect the UI after disconnect', async () => {
    let reply;
    let calls = 0;
    const { els, view, poll } = setup({
        getYoutubeCookieSync: () =>
            ++calls === 1
                ? Promise.resolve({ state: 'ready' })
                : new Promise(resolve => {
                      reply = resolve;
                  }),
        disconnectYoutubeCookieSync: async () => ({ state: 'off' }),
    });
    await view.load();
    poll();
    els.disconnectYoutubeCookiesBtn.click();
    await settle();
    reply({ state: 'ready' });
    await settle();
    assert.match(els.youtubeCookieStatus.textContent, /Cookie sync is off/);
});

test('restores the fallback browser and explains when the native reader is active', async () => {
    const { els, view } = setup({
        getYoutubeCookieSync: async () => ({ state: 'reader', fallback_browser: 'firefox', source: 'browser' }),
    });
    await view.load();
    assert.equal(els.youtubeCookieBrowser.value, 'firefox');
    assert.match(els.youtubeCookieStatus.textContent, /Using the yt-dlp browser reader/);
    assert.equal(els.disconnectYoutubeCookiesBtn.disabled, false);
});
