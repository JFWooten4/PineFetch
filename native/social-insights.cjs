// Native-process adapter: use Fetchary's library API, never its CLI/history store.
'use strict';
const process = require('node:process');
const { Buffer } = require('node:buffer');
const { URL } = require('node:url');

const { launchBundledBrowser } = require('./social-browser.cjs');
const { extractSocialInsights, FetcharyBrowserError } = require('fetchary');
const { dirname, join } = require('node:path');
const { version } = require(join(dirname(require.resolve('fetchary')), '../package.json'));

async function loadSocialInsights(url, extract = extractSocialInsights, launchBrowser = launchBundledBrowser) {
    const parsed = new URL(url);
    if (!['https:', 'http:'].includes(parsed.protocol) || parsed.username || parsed.password)
        throw new Error('A public video URL is required.');
    const options = {
        mode: 'browser',
        waitAfterLoad: 5000,
        launchBrowser,
        cacheTtlMs: 0,
        timeout: 20000,
        maxResponseBytes: 64 * 1024 * 1024,
    };
    let result;
    try {
        result = await extract(url, options);
    } catch (error) {
        if (!(error instanceof FetcharyBrowserError) || error.phase !== 'resource') throw error;
        // A background browser resource can fail while public page metadata
        // remains readable. Retry through Fetchary's guarded HTTP extraction.
        const fallback = await extract(url, { ...options, mode: 'http' });
        if (
            !['available', 'partial'].includes(fallback.data?.status) ||
            !Object.values(fallback.data.metrics || {}).some(Number.isSafeInteger)
        )
            throw error;
        result = fallback;
    }
    return { fetchary_version: version, data: result.data };
}

module.exports = { loadSocialInsights };
if (require.main === module) {
    loadSocialInsights(process.argv[2])
        .then(result => {
            const json = JSON.stringify(result);
            if (Buffer.byteLength(json) > 262144) throw new Error('Social Insights exceed the storage limit.');
            process.stdout.write(json);
        })
        .catch(error => {
            for (let cause = error, depth = 0; cause && depth < 8; cause = cause.cause, depth++) {
                if (cause.code === 'PINEFETCH_SOCIAL_BROWSER_UNAVAILABLE') {
                    process.stderr.write(
                        'The bundled Social Insights browser runtime is unavailable. Rebuild or reinstall PineFetch.'
                    );
                    process.exitCode = 1;
                    return;
                }
            }
            process.stderr.write('Social Insights could not be fetched. Check your connection and try again.');
            process.exitCode = 1;
        });
}
