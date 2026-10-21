import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { test } from 'node:test';
const require = createRequire(import.meta.url);
const { loadSocialInsights } = require('../native/social-insights.cjs');
const { extractSocialInsights, FetcharyBrowserError } = require('fetchary');
const { dependencies } = JSON.parse(await readFile(new URL('../package.json', import.meta.url), 'utf8'));
const url = 'https://www.youtube.com/watch?v=AbC123';

test('Fetchary library extracts only matching public video metrics and preserves zero and unavailable values', async () => {
    const html = await readFile(new URL('./fixtures/social-insights/youtube.html', import.meta.url), 'utf8');
    let requests = 0;
    const result = await loadSocialInsights(url, (target, options) => {
        assert.equal(target, url);
        assert.equal(options.mode, 'browser');
        assert.equal(options.waitAfterLoad, 5000);
        assert.equal(typeof options.launchBrowser, 'function');
        assert.equal(options.cacheTtlMs, 0);
        assert.equal(options.timeout, 20000);
        assert.equal(options.cookiesFromBrowser, undefined);
        assert.equal(options.session, undefined);
        return extractSocialInsights(target, {
            ...options,
            // Keep parser fixtures transport-only; the browser integration suite renders JS.
            mode: 'http',
            lookup: async () => [{ address: '93.184.216.34', family: 4 }],
            fetch: async () => {
                requests++;
                return new Response(html, { headers: { 'content-type': 'text/html' } });
            },
        });
    });
    assert.equal(requests, 1);
    assert.equal(result.fetchary_version, dependencies.fetchary);
    assert.equal(result.data.metrics.views, 12345);
    assert.equal(result.data.metrics.likes, 0);
    assert.equal(result.data.metrics.comments, 56);
    assert.equal(result.data.metrics.shares, null);
    assert.equal(result.data.unavailable.shares, 'unsupported');
    assert.equal(result.data.status, 'available');
});

test('adapter rejects nonpublic URL forms before invoking Fetchary and propagates failed refreshes', async () => {
    for (const target of ['file:///tmp/video', 'https://user:password@example.com/video']) {
        await assert.rejects(
            loadSocialInsights(target, () => assert.fail('Fetchary must not run')),
            /public video URL/
        );
    }
    await assert.rejects(
        loadSocialInsights(url, async () => {
            throw new Error('offline');
        }),
        /offline/
    );
});

test('the same Fetchary adapter extracts account metrics with channel identity, zero and missing counts', async () => {
    const target = 'https://www.youtube.com/@creator';
    const html =
        '<html><head><link rel="canonical" href="https://www.youtube.com/@creator"></head><body><script>var ytInitialData = {"header":{"c4TabbedHeaderRenderer":{"channelId":"UCfixture","subscriberCountText":{"simpleText":"0 subscribers"}}}};</script></body></html>';
    const result = await loadSocialInsights(target, (url, options) =>
        extractSocialInsights(url, {
            ...options,
            // Keep parser fixtures transport-only; the browser integration suite renders JS.
            mode: 'http',
            lookup: async () => [{ address: '93.184.216.34', family: 4 }],
            fetch: async () => new Response(html, { headers: { 'content-type': 'text/html' } }),
        })
    );
    assert.equal(result.data.kind, 'channel');
    assert.equal(result.data.metrics.subscribers, 0);
    assert.equal(result.data.metrics.views, null);
    assert.equal(result.data.url, target);
});

test('TikTok public handles return profile counts and exclude other accounts in the page', async () => {
    const target = 'https://www.tiktok.com/@creator';
    const html = await readFile(new URL('./fixtures/social-insights/tiktok-profile.html', import.meta.url), 'utf8');
    const result = await loadSocialInsights(target, (url, options) =>
        extractSocialInsights(url, {
            ...options,
            mode: 'http',
            lookup: async () => [{ address: '93.184.216.34', family: 4 }],
            fetch: async () => new Response(html, { headers: { 'content-type': 'text/html' } }),
        })
    );
    assert.equal(result.data.kind, 'profile');
    assert.equal(result.data.status, 'available');
    assert.equal(result.data.url, target);
    assert.equal(result.data.metrics.followers, 15683);
    assert.equal(result.data.metrics.following, 30);
    assert.equal(result.data.metrics.posts, 858);
    assert.equal(result.data.metrics.subscribers, null);
    assert.equal(result.data.unavailable.subscribers, 'unsupported');
});

test('Instagram browser resource failures retry public HTTP metadata through the real Fetchary library', async () => {
    const target = 'https://www.instagram.com/reel/DeCHfZaMmPc/';
    const html = await readFile(new URL('./fixtures/social-insights/instagram-reel.html', import.meta.url), 'utf8');
    const modes = [];
    const result = await loadSocialInsights(target, (url, options) => {
        assert.equal(url, target);
        assert.equal(options.timeout, 20000);
        assert.equal(options.maxResponseBytes, 64 * 1024 * 1024);
        assert.equal(options.cacheTtlMs, 0);
        assert.equal(options.cookieFile, undefined);
        assert.equal(options.cookiesFromBrowser, undefined);
        assert.equal(options.session, undefined);
        modes.push(options.mode);
        if (options.mode === 'browser')
            throw new FetcharyBrowserError('could not read complete browser request body', { phase: 'resource' });
        return extractSocialInsights(url, {
            ...options,
            lookup: async () => [{ address: '93.184.216.34', family: 4 }],
            fetch: async () => new Response(html, { headers: { 'content-type': 'text/html' } }),
        });
    });
    assert.deepEqual(modes, ['browser', 'http']);
    assert.equal(result.fetchary_version, dependencies.fetchary);
    assert.equal(result.data.status, 'partial');
    assert.equal(result.data.url, target);
    assert.equal(result.data.metrics.likes, 9);
    assert.equal(result.data.metrics.comments, 2);
    assert.equal(result.data.metrics.views, null);
    assert.equal(result.data.metrics.score, null);
});

test('fallback preserves failure semantics for runtime, network, empty and blocked results', async () => {
    for (const failure of [
        new Error('offline'),
        new FetcharyBrowserError('missing runtime', {
            phase: 'launch',
            cause: Object.assign(new Error('missing browser'), { code: 'PINEFETCH_SOCIAL_BROWSER_UNAVAILABLE' }),
        }),
        new FetcharyBrowserError('navigation failure', { phase: 'navigation' }),
    ]) {
        const modes = [];
        await assert.rejects(
            loadSocialInsights(url, (_target, options) => {
                modes.push(options.mode);
                throw failure;
            }),
            error => error === failure
        );
        assert.deepEqual(modes, ['browser']);
    }
    for (const data of [
        { status: 'unavailable', metrics: { likes: null } },
        { status: 'partial', metrics: { likes: null } },
        { status: 'blocked', metrics: { likes: null } },
        { status: 'login_required', metrics: { likes: null } },
    ]) {
        const failure = new FetcharyBrowserError('resource failure', { phase: 'resource' });
        const modes = [];
        await assert.rejects(
            loadSocialInsights(url, (_target, options) => {
                modes.push(options.mode);
                if (options.mode === 'browser') throw failure;
                return { data };
            }),
            error => error === failure
        );
        assert.deepEqual(modes, ['browser', 'http']);
    }
    await assert.rejects(
        loadSocialInsights(url, (_target, options) => {
            if (options.mode === 'browser') throw new FetcharyBrowserError('resource failure', { phase: 'resource' });
            throw new Error('HTTP failed too');
        }),
        /HTTP failed too/
    );
});
