import assert from 'node:assert/strict';
import { spawn, execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
const runtime = fileURLToPath(new URL('../src-tauri/resources/social-runtime/', import.meta.url));
const requireRuntime = createRequire(`${runtime}package.json`);
const { loadSocialInsights } = requireRuntime('./social-insights.cjs');
const { launchBundledBrowser, verifyBrowserRuntime } = requireRuntime('./social-browser.cjs');
const { extractSocialInsights } = requireRuntime('fetchary');

const capture = async (url, html) => {
    let launches = 0;
    let browser;
    const result = await loadSocialInsights(
        url,
        (target, options) =>
            extractSocialInsights(target, {
                ...options,
                lookup: async () => [{ address: '93.184.216.34', family: 4 }],
                fetch: async target => {
                    assert.equal(new URL(target).origin, new URL(url).origin);
                    return new Response(html, { headers: { 'content-type': 'text/html' } });
                },
            }),
        async options => {
            launches++;
            browser = await launchBundledBrowser(options);
            return browser;
        }
    );
    assert.equal(launches, 1);
    assert.equal(browser.connected, false, 'Fetchary must close its browser after extraction');
    return result;
};

test('bundled Chromium executes JavaScript without an installed personal browser', async () => {
    assert.match(await verifyBrowserRuntime(), /HeadlessChrome\//);
    const target = 'https://www.youtube.com/@fixture';
    const html =
        '<html><head></head><body><script>setTimeout(() => { const element = document.createElement("ytd-c4-tabbed-header-renderer"); const count = document.createElement("span"); count.id="subscriber-count"; count.textContent = ["12", "345 subscribers"].join(","); element.append(count); document.body.append(element); }, 100);</script></body></html>';
    const result = await capture(target, html);
    assert.equal(result.data.kind, 'channel');
    assert.equal(result.data.metrics.subscribers, 12345);
    assert.equal(result.data.metrics.views, null);
});

test('video insights read metrics that appear only after browser JavaScript executes', async () => {
    const target = 'https://www.youtube.com/watch?v=abcdefghijk';
    const html =
        '<html><head></head><body><script>setTimeout(() => { const data = document.createElement("script"); const details = {}; details[["video", "Id"].join("")] = "abcdefghijk"; details[["view", "Count"].join("")] = "321"; data.textContent = ["var ytInitial", "PlayerResponse = "].join("") + JSON.stringify({videoDetails:details}); document.body.append(data); }, 100);</script></body></html>';
    const result = await capture(target, html);
    assert.equal(result.data.kind, 'video');
    assert.equal(result.data.metrics.views, 321);
});

test('bundled browser extracts TikTok profile counts from the public handle', async () => {
    const target = 'https://www.tiktok.com/@creator';
    const html = await readFile(new URL('./fixtures/social-insights/tiktok-profile.html', import.meta.url), 'utf8');
    const result = await capture(target, html);
    assert.equal(result.data.kind, 'profile');
    assert.equal(result.data.status, 'available');
    assert.equal(result.data.url, target);
    assert.equal(result.data.metrics.followers, 15683);
    assert.equal(result.data.metrics.following, 30);
    assert.equal(result.data.metrics.posts, 858);
    assert.equal(result.data.metrics.subscribers, null);
});

test('the relocated native runtime launches from an unrelated working directory', async () => {
    const output = execFileSync(
        `${runtime}${process.platform === 'win32' ? 'node.exe' : 'node'}`,
        [
            '-e',
            'require(process.argv[1]).verifyBrowserRuntime().then(version=>process.stdout.write(version)).catch(()=>process.exit(1))',
            `${runtime}social-browser.cjs`,
        ],
        { cwd: tmpdir(), encoding: 'utf8', timeout: 30000 }
    );
    assert.match(output, /HeadlessChrome\//);
});

test(
    'native process-group cancellation terminates the headless browser',
    { skip: process.platform === 'win32' },
    async t => {
        const code =
            'const {launchBundledBrowser}=require(process.argv[1]); (async()=>{const b=await launchBundledBrowser({timeout:10000});const s=await b.target().createCDPSession();const p=await s.send("SystemInfo.getProcessInfo");const c=await s.send("Browser.getBrowserCommandLine"); const profile=c.arguments.find(a=>a.startsWith("--user-data-dir="))?.slice(16); process.stdout.write(JSON.stringify({...p.processInfo.find(p=>p.type==="browser"),profile})+"\\n");})().catch(()=>process.exit(1));';
        const child = spawn(`${runtime}node`, ['-e', code, `${runtime}social-browser.cjs`], {
            cwd: tmpdir(),
            detached: true,
            stdio: ['ignore', 'pipe', 'pipe'],
        });
        let pid;
        const stop = () => {
            if (child.exitCode === null) {
                try {
                    process.kill(-child.pid, 'SIGKILL');
                } catch {
                    /* The utility process may have already exited. */
                }
            }
        };
        t.after(stop);
        const line = await new Promise((resolve, reject) => {
            let output = '';
            let errors = '';
            const timer = setTimeout(() => reject(new Error('Browser PID was not reported')), 15000);
            child.stdout.on('data', chunk => {
                output += chunk;
                if (output.includes('\n')) {
                    clearTimeout(timer);
                    resolve(output.split('\n')[0]);
                }
            });
            child.stderr.on('data', chunk => {
                errors += chunk;
            });
            child.on('error', error => {
                clearTimeout(timer);
                reject(error);
            });
            child.on('exit', code => {
                clearTimeout(timer);
                reject(new Error(`Browser launcher exited ${code}: ${errors}`));
            });
        });
        const info = JSON.parse(line);
        pid = Number(info.id);
        if (info.profile) t.after(() => rm(info.profile, { recursive: true, force: true }));
        const group = execFileSync('ps', ['-o', 'pgid=', '-p', String(pid)], { encoding: 'utf8' }).trim();
        assert.equal(Number(group), child.pid, 'Chromium must inherit the native utility process group');
        stop();
        const exit = await new Promise(resolve => child.once('exit', (code, signal) => resolve({ code, signal })));
        assert.equal(exit.signal, 'SIGKILL');
        await new Promise((resolve, reject) => {
            const started = Date.now();
            const check = () => {
                try {
                    process.kill(pid, 0);
                } catch (error) {
                    if (error.code === 'ESRCH') return resolve();
                    return reject(error);
                }
                if (Date.now() - started > 5000)
                    return reject(new Error('Headless browser survived process-group termination'));
                setTimeout(check, 50);
            };
            check();
        });
    }
);
