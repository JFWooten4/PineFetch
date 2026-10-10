import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import {
    chmodSync,
    copyFileSync,
    mkdirSync,
    mkdtempSync,
    readFileSync,
    rmSync,
    statSync,
    writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';

const fixture = t => {
    const root = mkdtempSync(join(tmpdir(), 'pinefetch-ocr-build-'));
    t.after(() => rmSync(root, { recursive: true, force: true }));
    for (const directory of ['scripts', 'native', 'bin']) mkdirSync(join(root, directory));
    const script = join(root, 'scripts/prepare-ocr-runtime.sh');
    const source = join(root, 'native/vision-ocr.swift');
    const boundsSource = join(root, 'native/vision-ocr-bounds.swift');
    const helper = join(root, 'src-tauri/resources/ocr-runtime/vision-ocr-arm64');
    const stamp = `${helper}.build`;
    const calls = join(root, 'compiler-calls');
    copyFileSync(new URL('../scripts/prepare-ocr-runtime.sh', import.meta.url), script);
    writeFileSync(source, 'synthetic Swift source');
    writeFileSync(boundsSource, 'synthetic bounds source');
    const executable = (name, body) => {
        const path = join(root, 'bin', name);
        writeFileSync(path, `#!/bin/sh\nset -eu\n${body}\n`);
        chmodSync(path, 0o755);
    };
    executable('uname', 'printf "Darwin\\n"');
    executable(
        'xcrun',
        [
            'if [ "$1" = --sdk ]; then printf "%s\\n" "$OCR_TEST_SDK"; exit 0; fi',
            'if [ "$2" = --version ]; then printf "%s\\n" "${OCR_TEST_COMPILER:-synthetic Swift 1}"; exit 0; fi',
            'printf "%s\\n" "$*" >> "$OCR_TEST_CALLS"',
            'if [ "${OCR_TEST_FAIL_COMPILE:-0}" = 1 ]; then exit 23; fi',
            'while [ "$#" -gt 0 ]; do',
            '  case "$1" in',
            '    -o) shift; output="$1" ;;',
            '    *.swift) source="$1" ;;',
            '  esac',
            '  shift',
            'done',
            'cat "$source" > "$output"',
            'chmod 755 "$output"',
        ].join('\n')
    );
    executable('codesign', 'if [ "${OCR_TEST_FAIL_SIGN:-0}" = 1 ]; then exit 24; fi');
    const run = (overrides = {}, relative = false) =>
        spawnSync('bash', [relative ? 'scripts/prepare-ocr-runtime.sh' : script], {
            cwd: relative ? root : tmpdir(),
            env: {
                ...process.env,
                PATH: `${join(root, 'bin')}:${process.env.PATH}`,
                PINEFETCH_OCR_TARGET_ARCH: 'aarch64',
                MACOSX_DEPLOYMENT_TARGET: '11.0',
                SDKROOT: '',
                OCR_TEST_SDK: join(root, 'sdk'),
                OCR_TEST_CALLS: calls,
                ...overrides,
            },
            encoding: 'utf8',
            timeout: 10000,
        });
    const compileCount = () => readFileSync(calls, 'utf8').trim().split('\n').length;
    return { script, source, boundsSource, helper, stamp, run, compileCount };
};

test('OCR builds reuse unchanged artifacts and rebuild when source or toolchain inputs change', t => {
    const f = fixture(t);
    assert.equal(f.run().status, 0);
    const helperTime = statSync(f.helper, { bigint: true }).mtimeNs;
    const stampTime = statSync(f.stamp, { bigint: true }).mtimeNs;
    assert.equal(f.run({}, true).status, 0);
    assert.equal(f.compileCount(), 1);
    assert.equal(statSync(f.helper, { bigint: true }).mtimeNs, helperTime);
    assert.equal(statSync(f.stamp, { bigint: true }).mtimeNs, stampTime);

    writeFileSync(f.source, 'changed Swift source');
    assert.equal(f.run().status, 0);
    assert.equal(f.compileCount(), 2);
    assert.equal(readFileSync(f.helper, 'utf8'), 'changed Swift source');
    writeFileSync(f.boundsSource, 'changed bounds source');
    assert.equal(f.run().status, 0);
    assert.equal(f.compileCount(), 3);
    assert.equal(f.run({ MACOSX_DEPLOYMENT_TARGET: '12.0' }).status, 0);
    assert.equal(f.compileCount(), 4);
    assert.equal(f.run({ MACOSX_DEPLOYMENT_TARGET: '12.0', OCR_TEST_COMPILER: 'synthetic Swift 2' }).status, 0);
    assert.equal(f.compileCount(), 5);
});

test('failed OCR compilation or signing preserves the last build, and missing or damaged artifacts are repaired', t => {
    const f = fixture(t);
    assert.equal(f.run().status, 0);
    const original = readFileSync(f.helper);
    const stamp = readFileSync(f.stamp);
    writeFileSync(f.source, 'new source');
    assert.equal(f.run({ OCR_TEST_FAIL_COMPILE: '1' }).status, 23);
    assert.deepEqual(readFileSync(f.helper), original);
    assert.deepEqual(readFileSync(f.stamp), stamp);
    assert.equal(f.run({ OCR_TEST_FAIL_SIGN: '1' }).status, 24);
    assert.deepEqual(readFileSync(f.helper), original);
    assert.deepEqual(readFileSync(f.stamp), stamp);
    assert.equal(f.run().status, 0);
    assert.equal(readFileSync(f.helper, 'utf8'), 'new source');
    writeFileSync(f.helper, 'damaged helper');
    assert.equal(f.run().status, 0);
    assert.equal(readFileSync(f.helper, 'utf8'), 'new source');
    rmSync(f.helper);
    assert.equal(f.run().status, 0);
    assert.equal(readFileSync(f.helper, 'utf8'), 'new source');
    chmodSync(f.helper, 0o600);
    assert.equal(f.run().status, 0);
    assert.notEqual(statSync(f.helper).mode & 0o111, 0);
    assert.equal(f.compileCount(), 7);
});
