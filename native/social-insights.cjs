// Native-process adapter: use Fetchary's library API, never its CLI/history store.
'use strict';
const process = require('node:process');
const { Buffer } = require('node:buffer');
const { URL } = require('node:url');

const { extractSocialInsights } = require('fetchary');
const { dirname, join } = require('node:path');
const { version } = require(join(dirname(require.resolve('fetchary')), '../package.json'));

async function loadSocialInsights(url, extract = extractSocialInsights) {
    const parsed = new URL(url);
    if (!['https:', 'http:'].includes(parsed.protocol) || parsed.username || parsed.password)
        throw new Error('A public video URL is required.');
    const result = await extract(url, {
        mode: 'http',
        cacheTtlMs: 0,
        timeout: 20000,
        maxResponseBytes: 8 * 1024 * 1024,
    });
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
        .catch(() => {
            process.stderr.write('Social Insights could not be fetched. Check your connection and try again.');
            process.exitCode = 1;
        });
}
