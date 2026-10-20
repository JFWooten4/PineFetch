'use strict';
const { accessSync, constants, mkdtempSync, readFileSync, rmSync } = require('node:fs');
const { rm } = require('node:fs/promises');
const { tmpdir } = require('node:os');
const { dirname, isAbsolute, join, relative, resolve } = require('node:path');
const runtimeDirectory = dirname(module.filename);
const process = require('node:process');
const puppeteer = require('puppeteer-core');
const { launch, CDP_WEBSOCKET_ENDPOINT_REGEX } = require('@puppeteer/browsers');

const runtimeError = () =>
    Object.assign(
        new Error('The bundled Social Insights browser runtime is unavailable. Rebuild or reinstall PineFetch.'),
        { code: 'PINEFETCH_SOCIAL_BROWSER_UNAVAILABLE' }
    );

function browserExecutable(directory = runtimeDirectory) {
    try {
        const manifest = JSON.parse(readFileSync(join(directory, 'browser.json'), 'utf8'));
        if (
            manifest.target !== `${process.platform}/${process.arch}` ||
            typeof manifest.executable !== 'string' ||
            isAbsolute(manifest.executable)
        )
            throw runtimeError();
        const executable = resolve(directory, manifest.executable);
        const within = relative(resolve(directory, 'browser'), executable);
        if (!within || within.startsWith('..') || isAbsolute(within)) throw runtimeError();
        accessSync(executable, constants.X_OK);
        return executable;
    } catch {
        throw runtimeError();
    }
}

async function launchBundledBrowser(options = {}, directory = runtimeDirectory) {
    options.signal?.throwIfAborted();
    const executablePath = browserExecutable(directory);
    const profile = mkdtempSync(join(tmpdir(), 'pinefetch-social-browser-'));
    let child;
    try {
        const args = await puppeteer.defaultArgs({
            headless: 'shell',
            userDataDir: profile,
            args: [...(options.args || []), '--remote-debugging-port=0'],
        });
        child = launch({
            executablePath,
            args,
            env: { ...process.env },
            detached: false,
            signal: options.signal,
            handleSIGINT: false,
            handleSIGTERM: false,
            handleSIGHUP: false,
            onExit: () => rm(profile, { recursive: true, force: true }),
        });
        const browserWSEndpoint = await child.waitForLineOutput(CDP_WEBSOCKET_ENDPOINT_REGEX, options.timeout || 20000);
        options.signal?.throwIfAborted();
        const browser = await puppeteer.connect({
            browserWSEndpoint,
            protocolTimeout: options.timeout || 20000,
            defaultViewport: { width: 1280, height: 900 },
        });
        const context = await browser.createBrowserContext();
        browser.newPage = context.newPage.bind(context);
        const close = browser.close.bind(browser);
        let closing;
        browser.close = () =>
            (closing ||= (async () => {
                try {
                    await close();
                } finally {
                    await child.close();
                }
            })());
        return browser;
    } catch (error) {
        if (child) await child.close();
        else rmSync(profile, { recursive: true, force: true });
        throw error;
    }
}

async function verifyBrowserRuntime(directory = runtimeDirectory) {
    const browser = await launchBundledBrowser(
        { timeout: 20000, signal: globalThis.AbortSignal.timeout(20000) },
        directory
    );
    try {
        const page = await browser.newPage();
        await page.setContent('<body><script>document.body.dataset.rendered="yes"</script>');
        if ((await page.evaluate(() => globalThis.document.body.dataset.rendered)) !== 'yes')
            throw new Error('Browser rendering smoke test failed.');
        return await browser.version();
    } finally {
        await browser.close();
    }
}

module.exports = { browserExecutable, launchBundledBrowser, verifyBrowserRuntime };
