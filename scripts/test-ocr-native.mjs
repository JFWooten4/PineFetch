// Explicit macOS artifact smoke: synthetic media only; no personal data or downloads.
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, readFile, rm, writeFile, copyFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const run = promisify(execFile);
const repository = fileURLToPath(new URL('../', import.meta.url));
if (process.platform !== 'darwin') throw new Error('This smoke requires macOS and Apple Vision.');
const helper = resolve(
    process.argv[2] ||
        join(
            repository,
            'src-tauri/resources/ocr-runtime',
            `vision-ocr-${process.arch === 'arm64' ? 'arm64' : 'x86_64'}`
        )
);
const ffmpeg = process.env.PINEFETCH_FFMPEG_BIN || '/opt/homebrew/bin/ffmpeg';
const ffprobe = process.env.PINEFETCH_FFPROBE_BIN || '/opt/homebrew/bin/ffprobe';
const root = await mkdtemp(join(tmpdir(), 'pinefetch-native-ocr-'));
let child;
try {
    const source = join(root, 'image.swift');
    await writeFile(
        source,
        `import AppKit
let image = NSImage(size: NSSize(width: 1280, height: 720))
image.lockFocus()
NSColor.white.setFill()
NSRect(x: 0, y: 0, width: 1280, height: 720).fill()
let style = NSMutableParagraphStyle()
style.alignment = .center
("12 Kilometer neue Radwege" as NSString).draw(in: NSRect(x: 40, y: 300, width: 1200, height: 100), withAttributes: [.font: NSFont.boldSystemFont(ofSize: 54), .foregroundColor: NSColor.black, .paragraphStyle: style])
image.unlockFocus()
let bitmap = NSBitmapImageRep(data: image.tiffRepresentation!)!
try bitmap.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
`
    );
    await run('/usr/bin/xcrun', ['swiftc', source, '-o', join(root, 'draw')], { timeout: 60000 });
    await run(join(root, 'draw'), [join(root, 'text.png')], { timeout: 10000 });
    await writeFile(
        join(root, 'frames.txt'),
        "file 'text.png'\nduration 0.4\nfile 'text.png'\nduration 0.7\nfile 'text.png'\nduration 1.3\nfile 'text.png'\nduration 1.1\nfile 'text.png'\n"
    );
    const video = join(root, 'vfr.mkv');
    await run(
        ffmpeg,
        [
            '-nostdin',
            '-v',
            'error',
            '-f',
            'concat',
            '-safe',
            '0',
            '-i',
            join(root, 'frames.txt'),
            '-vf',
            'setpts=PTS+5/TB',
            '-fps_mode',
            'vfr',
            '-c:v',
            'ffv1',
            video,
        ],
        { timeout: 30000 }
    );
    const probe = JSON.parse(
        (await run(ffprobe, ['-v', 'error', '-show_frames', '-show_format', '-of', 'json', video])).stdout
    );
    const origin = Number(probe.format.start_time);
    assert(origin >= 5, 'Fixture must contain a nonzero timestamp origin');
    const rawTimes = probe.frames.map(frame => Number(frame.best_effort_timestamp_time) - origin);
    assert(
        new Set(rawTimes.slice(1).map((time, index) => Math.round((time - rawTimes[index]) * 1000))).size > 1,
        'Fixture must have variable frame intervals'
    );
    const { stderr } = await run(
        ffmpeg,
        [
            '-nostdin',
            '-hide_banner',
            '-loglevel',
            'info',
            '-copyts',
            '-start_at_zero',
            '-i',
            video,
            '-map',
            '0:0',
            '-vf',
            "select='isnan(prev_selected_t)+gte(t-prev_selected_t,1)',showinfo",
            '-fps_mode',
            'passthrough',
            '-an',
            '-sn',
            '-c:v',
            'png',
            join(root, 'frame-%04d.png'),
        ],
        { timeout: 30000 }
    );
    const observations = [...stderr.matchAll(/n:\s*(\d+)\s+pts:.*?pts_time:([\d.]+)/g)].map(match => ({
        id: Number(match[1]),
        ms: Math.round(Number(match[2]) * 1000),
    }));
    assert(observations.length >= 3);
    let previous;
    const selected = rawTimes.filter(time => {
        if (previous === undefined || time - previous >= 1 - 0.0001) {
            previous = time;
            return true;
        }
        return false;
    });
    assert.deepEqual(
        observations.map(frame => frame.ms),
        selected.map(time => Math.round(time * 1000))
    );
    const localHelper = join(root, 'vision-ocr');
    await copyFile(helper, localHelper);
    // Empty developer PATH and an unrelated cwd demonstrate native runtime use.
    child = spawn(localHelper, [], { cwd: root, env: { PATH: '/usr/bin:/bin' }, stdio: ['pipe', 'pipe', 'pipe'] });
    const exited = once(child, 'exit');
    let diagnostics = '';
    child.stderr.on('data', chunk => {
        diagnostics += chunk;
    });
    const lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]();
    const read = async () => {
        let timer;
        try {
            const response = await Promise.race([
                lines.next(),
                new Promise((_, reject) => {
                    timer = setTimeout(() => reject(new Error(`Vision response timed out: ${diagnostics}`)), 30000);
                }),
            ]);
            assert.equal(response.done, false, diagnostics);
            return JSON.parse(response.value);
        } finally {
            clearTimeout(timer);
        }
    };
    const ready = await read();
    assert.equal(ready.type, 'ready');
    assert.equal(ready.engine, 'apple_vision');
    assert.equal(ready.settings.uses_language_correction, false);
    assert(
        ready.settings.recognition_languages.every(language => ready.settings.supported_languages.includes(language))
    );
    for (const frame of observations) {
        const path = join(root, `frame-${String(frame.id + 1).padStart(4, '0')}.png`);
        await readFile(path);
        child.stdin.write(`${JSON.stringify({ frame_id: frame.id, path })}\n`);
        const recognized = await read();
        assert.equal(recognized.frame_id, frame.id);
        assert(
            recognized.observations.some(observation => observation.text.includes('12 Kilometer neue Radwege')),
            JSON.stringify(recognized)
        );
        assert(
            recognized.observations.every(observation => observation.bounds.width > 0 && observation.bounds.height > 0)
        );
    }
    child.stdin.end();
    const [code] = await exited;
    assert.equal(code, 0, diagnostics);
    const dependencies = (await run('/usr/bin/otool', ['-L', localHelper])).stdout;
    assert(!dependencies.includes('/opt/homebrew/') && !dependencies.includes('/Library/Developer/'), dependencies);
    console.log(
        `[OCR native] Vision revision ${ready.engine_version.vision_request_revision}; languages ${ready.settings.recognition_languages.join(', ')}; VFR observations ${observations.map(frame => frame.ms).join(', ')} ms; native helper passed with isolated cwd/PATH.`
    );
} finally {
    if (child && child.exitCode === null) child.kill('SIGKILL');
    await rm(root, { recursive: true, force: true });
}
