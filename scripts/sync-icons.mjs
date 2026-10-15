import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { copyFile, mkdir, mkdtemp, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import sharp from 'sharp';

const source = fileURLToPath(new URL('../src/images/logo.png', import.meta.url));
const destination = fileURLToPath(new URL('../src-tauri/icons/', import.meta.url));
const favicon = fileURLToPath(new URL('../src/images/favicon.png', import.meta.url));
const { width, height } = await sharp(source).metadata();
assert(width === height && width >= 1024, 'The logo must be square and at least 1024 pixels wide.');

// Leave optical breathing room so the desktop tile matches neighboring Dock icons.
const desktopSize = 1024;
const artworkSize = 824;
const inset = (desktopSize - artworkSize) / 2;
const temporary = await mkdtemp(join(tmpdir(), 'pinefetch-icons-'));
try {
    // Keep transparency around the rounded tile and add a centered desktop margin.
    const input = join(temporary, 'logo.png');
    const generated = join(temporary, 'icons');
    const desktopIcon = await sharp(source)
        .resize(artworkSize, artworkSize)
        .ensureAlpha()
        .extend({ top: inset, bottom: inset, left: inset, right: inset, background: '#00000000' })
        .png()
        .toBuffer();
    await sharp(desktopIcon).toFile(input);
    await promisify(execFile)(process.execPath, [
        fileURLToPath(import.meta.resolve('@tauri-apps/cli/tauri.js')),
        'icon',
        input,
        '--output',
        generated,
    ]);

    await mkdir(destination, { recursive: true });
    for (const entry of await readdir(generated, { withFileTypes: true })) {
        // This app targets desktops; omit the generated Android/iOS directories.
        if (entry.isFile()) await copyFile(join(generated, entry.name), join(destination, entry.name));
    }

    const iconset = join(destination, 'icon.iconset');
    await mkdir(iconset, { recursive: true });
    for (const size of [16, 32, 64, 128, 256, 512, 1024]) {
        await sharp(desktopIcon)
            .resize(size, size)
            .ensureAlpha()
            .png()
            .toFile(join(iconset, `icon_${size}x${size}.png`));
        if (size !== 64 && size < 1024) {
            await sharp(desktopIcon)
                .resize(size * 2, size * 2)
                .ensureAlpha()
                .png()
                .toFile(join(iconset, `icon_${size}x${size}@2x.png`));
        }
    }
    await sharp(source).resize(32, 32).png().toFile(favicon);
    console.log('[icons] Regenerated the favicon, desktop icons and macOS iconset from src/images/logo.png.');
} finally {
    await rm(temporary, { recursive: true, force: true });
}
