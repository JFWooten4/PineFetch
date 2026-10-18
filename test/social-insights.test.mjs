import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { test } from 'node:test';
const require = createRequire(import.meta.url);
const { loadSocialInsights } = require('../native/social-insights.cjs');
const { extractSocialInsights } = require('fetchary');
const url = 'https://www.youtube.com/watch?v=AbC123';

test('Fetchary library extracts only matching public video metrics and preserves zero and unavailable values', async () => {
    const html = await readFile(new URL('./fixtures/social-insights/youtube.html', import.meta.url), 'utf8');
    let requests = 0;
    const result = await loadSocialInsights(url, (target, options) => {
        assert.equal(target, url);
        assert.equal(options.mode, 'http');
        assert.equal(options.cacheTtlMs, 0);
        assert.equal(options.timeout, 20000);
        assert.equal(options.cookiesFromBrowser, undefined);
        assert.equal(options.session, undefined);
        return extractSocialInsights(target, {
            ...options,
            lookup: async () => [{ address: '93.184.216.34', family: 4 }],
            fetch: async () => {
                requests++;
                return new Response(html, { headers: { 'content-type': 'text/html' } });
            },
        });
    });
    assert.equal(requests, 1);
    assert.equal(result.fetchary_version, '2.0.4');
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
