import assert from 'node:assert/strict';
import { chmod, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
const require = createRequire(import.meta.url);
const { browserExecutable, launchBundledBrowser } = require('../native/social-browser.cjs');
const { loadSocialInsights } = require('../native/social-insights.cjs');

test('browser runtime manifests reject missing, foreign and escaping executables', async t => {
    const directory = await mkdtemp(join(tmpdir(), 'pinefetch-social-manifest-'));
    t.after(() => rm(directory, { recursive: true, force: true }));
    await mkdir(join(directory, 'browser'));
    const executable = join(directory, 'browser', 'fixture');
    await writeFile(executable, 'synthetic executable');
    await chmod(executable, 0o700);
    const manifest = { target: `${process.platform}/${process.arch}`, executable: 'browser/fixture' };
    const invalid = error => error.code === 'PINEFETCH_SOCIAL_BROWSER_UNAVAILABLE';
    assert.throws(() => browserExecutable(directory), invalid);
    await writeFile(join(directory, 'browser.json'), JSON.stringify(manifest));
    assert.equal(browserExecutable(directory), executable);
    for (const fields of [
        { executable: '../outside' },
        { executable: executable },
        { target: 'foreign/architecture' },
        { executable: 'browser/missing' },
        { executable: 'browser' },
    ]) {
        await writeFile(join(directory, 'browser.json'), JSON.stringify({ ...manifest, ...fields }));
        assert.throws(() => browserExecutable(directory), invalid);
    }
    await writeFile(join(directory, 'browser.json'), '{bad json');
    await assert.rejects(launchBundledBrowser({}, directory), invalid);
});

test('video and account loads require browser mode and forward the dedicated launcher', async () => {
    const launcher = async () => {
        throw new Error('fixture launcher');
    };
    for (const url of ['https://www.youtube.com/@fixture', 'https://www.youtube.com/watch?v=abcdefghijk']) {
        const result = await loadSocialInsights(
            url,
            async (target, options) => {
                assert.equal(target, url);
                assert.equal(options.mode, 'browser');
                assert.equal(options.launchBrowser, launcher);
                assert.equal(options.waitAfterLoad, 5000);
                assert.equal(options.maxResponseBytes, 64 * 1024 * 1024);
                assert.equal(options.cacheTtlMs, 0);
                assert.equal(options.cookieFile, undefined);
                assert.equal(options.cookiesFromBrowser, undefined);
                assert.equal(options.session, undefined);
                await assert.rejects(options.launchBrowser(), /fixture launcher/);
                return { data: { rendered: true } };
            },
            launcher
        );
        assert.deepEqual(result.data, { rendered: true });
    }
});
