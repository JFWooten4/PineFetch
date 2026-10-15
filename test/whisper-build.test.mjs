import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

const fixture = t => {
    const root = mkdtempSync(join(tmpdir(), 'pinefetch-whisper-build-'));
    t.after(() => rmSync(root, { recursive: true, force: true }));
    const scripts = join(root, 'scripts');
    mkdirSync(scripts);
    for (const name of [
        'prepare-whisper-runtime.sh',
        'check-whisper-runtime.py',
        'whisper-runtime.version',
        'whisper-av.version',
    ]) {
        copyFileSync(new URL(`../scripts/${name}`, import.meta.url), join(scripts, name));
    }
    const sourcePython = join(root, 'source-python');
    const calls = join(root, 'pip-calls.json');
    writeFileSync(
        sourcePython,
        `#!${process.execPath}
const fs = require('node:fs');
const path = require('node:path');
const args = process.argv.slice(2);
if (args[0] === '-c') {
    console.log('3.12.14');
} else if (args[0] === '-m' && args[1] === 'venv') {
    fs.mkdirSync(path.join(args[2], 'bin'), { recursive: true });
    fs.copyFileSync(process.argv[1], path.join(args[2], 'bin/python'));
    fs.chmodSync(path.join(args[2], 'bin/python'), 0o755);
} else if (args[0] === '-m' && args[1] === 'pip') {
    const callsPath = process.env.WHISPER_TEST_CALLS;
    const calls = fs.existsSync(callsPath) ? JSON.parse(fs.readFileSync(callsPath)) : [];
    calls.push(args);
    fs.writeFileSync(callsPath, JSON.stringify(calls));
    const versions = [args.find(arg => arg.startsWith('faster-whisper==')).split('==')[1], args.find(arg => arg.startsWith('av==')).split('==')[1], '3.12.14'];
    fs.writeFileSync(path.join(path.dirname(process.argv[1]), '../versions.json'), JSON.stringify(versions));
} else if (path.basename(args[0]) === 'check-whisper-runtime.py') {
    const versions = JSON.parse(fs.readFileSync(path.join(path.dirname(process.argv[1]), '../versions.json')));
    if (JSON.stringify(versions) !== JSON.stringify(args.slice(1)) || process.env.WHISPER_TEST_FAIL_DECODE === '1') {
        console.error("TypeError: open() got an unexpected keyword argument 'metadata_errors'");
        process.exit(1);
    }
} else {
    throw new Error('Unexpected fixture Python arguments: ' + JSON.stringify(args));
}
`
    );
    chmodSync(sourcePython, 0o755);
    const runtime = join(root, 'src-tauri/resources/whisper-runtime');
    const run = overrides => {
        const result = spawnSync('bash', [join(scripts, 'prepare-whisper-runtime.sh')], {
            cwd: tmpdir(),
            env: {
                ...process.env,
                PINEFETCH_WHISPER_PYTHON_BIN: sourcePython,
                WHISPER_TEST_CALLS: calls,
                ...overrides,
            },
            encoding: 'utf8',
            // First execution of temporary fixture programs can be slow on macOS.
            timeout: 30000,
        });
        assert.ifError(result.error);
        assert.equal(result.signal, null, result.stderr);
        return result;
    };
    return { runtime, run, calls: () => JSON.parse(readFileSync(calls, 'utf8')) };
};

test('Whisper builds pin PyAV and reject stale or mismatched cached dependencies', { timeout: 60000 }, t => {
    const f = fixture(t);
    assert.equal(f.run().status, 0);
    const pinnedAv = readFileSync(new URL('../scripts/whisper-av.version', import.meta.url), 'utf8').trim();
    assert(f.calls()[0].includes(`av==${pinnedAv}`));
    assert.match(
        readFileSync(join(f.runtime, '.pinefetch-whisper-runtime'), 'utf8'),
        new RegExp(`^av=${pinnedAv.replaceAll('.', '\\.')}$`, 'm')
    );
    assert.match(f.run().stdout, /Reusing bundled whisper runtime/);
    assert.equal(f.calls().length, 1);

    // A legacy stamp must not reuse a runtime installed before the PyAV pin.
    writeFileSync(join(f.runtime, '.pinefetch-whisper-runtime'), 'faster-whisper=1.2.1\npython=3.12.14\n');
    assert.equal(f.run().status, 0);
    assert.equal(f.calls().length, 2);

    // Even with a current stamp, the actual installed packages must match.
    writeFileSync(join(f.runtime, 'versions.json'), JSON.stringify(['1.2.1', '19.0.1', '3.12.14']));
    assert.equal(f.run().status, 0);
    assert.equal(f.calls().length, 3);
});

test('Whisper runtime preparation fails when audio decoding fails, before marking it ready', { timeout: 60000 }, t => {
    const f = fixture(t);
    const result = f.run({ WHISPER_TEST_FAIL_DECODE: '1' });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /metadata_errors/);
    assert.throws(() => readFileSync(join(f.runtime, '.pinefetch-whisper-runtime')), { code: 'ENOENT' });
});
