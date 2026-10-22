// Run explicitly: node test/link-dump-cookie-sync.integration.mjs [extension checkout]
// Uses a fresh profile and synthetic cookies, never the user's browser profile.
import assert from 'node:assert/strict';
import { chromium } from 'playwright';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { cp, mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
const source = resolve(process.argv[2] || fileURLToPath(new URL('../../PineFetch-Link-Dump', import.meta.url)));
const root = await mkdtemp(join(tmpdir(), 'pinefetch-extension-runner-'));
const extension = join(root, 'extension');
let context;
let received = 0;
const server = createServer(async (request, response) => {
    response.setHeader('Access-Control-Allow-Origin', '*');
    response.setHeader('Access-Control-Allow-Headers', 'Content-Type');
    if (request.method === 'OPTIONS') {
        response.writeHead(204);
        response.end();
        return;
    }
    assert.equal(request.url, '/syncYoutubeCookies/');
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const payload = JSON.parse(Buffer.concat(chunks));
    assert.equal(payload.secret, 'synthetic-runner-pairing');
    assert.ok(payload.cookies.length > 0);
    assert.ok(payload.cookies.every(cookie => cookie.domain.replace(/^\./, '') === 'youtube.com'));
    assert.ok(payload.cookies.every(cookie => cookie.value === 'synthetic-runner-cookie'));
    received++;
    response.writeHead(200, { 'Content-Type': 'application/json' });
    response.end(JSON.stringify({ ok: true, status: { state: 'ready', cookie_count: payload.cookies.length } }));
}).listen(0, '127.0.0.1');
await once(server, 'listening');
try {
    for (const name of ['manifest.json', 'popup.html', 'popup.js', 'popup.css', 'providers', 'vendor', 'assets']) {
        await cp(join(source, name), join(extension, name), { recursive: true });
    }
    const manifest = JSON.parse(await readFile(join(extension, 'manifest.json'), 'utf8'));
    assert.deepEqual(manifest.optional_permissions, ['cookies']);
    // The isolated fixture pregrants only cookie access so the runner can test
    // the real cookies API without approving a prompt in the user's browser.
    manifest.permissions.push('cookies');
    delete manifest.optional_permissions;
    manifest.background = { service_worker: 'runner-worker.js' };
    await writeFile(join(extension, 'manifest.json'), JSON.stringify(manifest));
    await writeFile(join(extension, 'runner-worker.js'), 'chrome.runtime.onInstalled.addListener(() => {});');
    context = await chromium.launchPersistentContext(join(root, 'profile'), {
        headless: true,
        channel: 'chromium',
        args: [`--disable-extensions-except=${extension}`, `--load-extension=${extension}`],
    });
    context.setDefaultTimeout(15000);
    const worker = context.serviceWorkers()[0] || (await context.waitForEvent('serviceworker', { timeout: 15000 }));
    const id = new URL(worker.url()).host;
    await context.addCookies([
        {
            name: 'synthetic',
            value: 'synthetic-runner-cookie',
            domain: '.youtube.com',
            path: '/',
            secure: true,
            httpOnly: true,
        },
        { name: 'unrelated', value: 'must-not-be-sent', domain: '.example.test', path: '/', secure: true },
    ]);
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.goto(`chrome-extension://${id}/popup.html`);
    await page.locator('#pfSettingsTab').click();
    await page.locator('#pfEndpointInput').fill(`http://127.0.0.1:${server.address().port}`);
    await page.locator('#pfSecretInput').fill('synthetic-runner-pairing');
    assert.equal(received, 0);
    await page.locator('#pfSendTab').click();
    await page.locator('#pfSyncCookiesButton').click();
    await page.waitForFunction(() =>
        globalThis.document.querySelector('#pfCookieSyncStatus').textContent.includes('synced to PineFetch')
    );
    assert.equal(received, 1);
    assert.deepEqual(errors, []);
    await page.screenshot({ path: '/tmp/pinefetch-link-dump-cookie-runner.png', fullPage: true });
    console.log(
        'Extension runner passed: real cookies API, click-only transfer, YouTube-only payload, and local authenticated request with synthetic data.'
    );
} finally {
    await context?.close();
    server.close();
    await rm(root, { recursive: true, force: true });
}
